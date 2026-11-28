# 0244 — Supervisor-delegated approvals

- Status: Accepted
- Date: 2026-11-28
- Scope: approval routing for non-asset permission requests
- Depends on: ADR-0193 (task contracts), ADR-0234 (Run Contract), ADR-0243
  (shell analysis), risk-based approval broker

## Context

Smart mode already auto-allows routine workspace writes, builds, and tests, but
every host-state/scope contradiction still interrupts the operator. In a real
plan, routine scratch/test work produced repeated `HostState` prompts that the
operator had to adjudicate. The operator asked for routine approvals to be
reviewed by the requesting agent's superior, and only hardware/account/host
damage to remain user-owned.

The approval broker is a security boundary: delegation must not become a way to
approve destructive or credential/account actions, and a supervisor must not be
able to approve its own request.

## Decision

1. **Asset-risk gate first.** `classify_asset_risk` runs before routing.
   `Destructive` risk, native `computer`, sensitive/host-state browser, mutating
   MCP, remote ACP, `remote_*`, hardware commands (disk/firmware/power/thermal),
   account/credential operations (keychain, SSH keys, cloud/registry logins,
   force-push), and host-system commands stay with the operator. The list is a
   bounded heuristic; the sandbox, contract, and effect gates remain
   authoritative.
2. **Hierarchy routing.** Non-asset requests go to the nearest ancestor agent
   recorded in the session: Labour→Worker, Worker→Manager, Manager→Leader.
   Verifier/Scout climb to their manager. A Leader has no supervisor. Missing
   parents, self-parenting, or a lookup failure fall back to the operator.
3. **One bounded reviewer call.** The supervisor is invoked as a separate
   provider call with only a `review_approval` decision tool,
   `temperature=0`, `max_tokens=768`, a 20s timeout, and a redacted/bounded
   input preview (shell command, path/URL, MCP action; file/patch content is
   reduced to a length). The reviewer must call the tool with
   `{decision, reason}`; a text JSON reply remains a parser fallback. It cannot
   execute the requested action. The live 10-task run showed that 256 tokens
   truncated DeepSeek's response (`finish_reason=length`), so the limit and
   structured tool call were raised in the same round.
4. **Dialectical review.** The reviewer prompt requires the strongest
   counterargument and considers the requester, operator/asset owner, security,
   and failure modes; uncertainty means deny. The shared agent prompt also asks
   every agent to reason dialectically and report dissenting evidence.
5. **Budget and audit.** The review reserves tokens from the session budget,
   records usage under the reviewer with `task_id: null` (coordination
   overhead), and appends `approval.review.started`, `.decided`, or `.failed`
   events with reviewer/decision/tokens/latency but no raw input or secrets.
   Insufficient budget, provider failure, timeout, or invalid JSON leaves the
   approval pending for the operator (fail open to the human, not to the tool).
6. **Default and override.** Delegation defaults on
   (`VERLINPARK_DELEGATE_APPROVALS=1`); `0/false/off/no/disabled` turns it off
   and restores the old user-only behavior. The setting is exposed in
   `agent.info.delegate_approvals`. Test harnesses set it off explicitly so
   scripted providers are unaffected.
7. **No privilege widening.** Approval templates/grants and contract denial run
   first. A supervisor cannot approve a blocked assessment, a `Deny`, a
   destructive/asset-risk request, its own request, or another session. The
   review only resolves the same approval row the operator would have resolved.

## Consequences

- Routine strict-mode writes and non-asset scope prompts no longer interrupt
  the operator when a supervisor exists.
- Asset-risk and destructive requests still create pending operator approvals.
- Review adds one small provider call per delegated approval and is visible in
  usage/cache telemetry and `approval.review.*` events.
- The approval row/reason includes `[delegated to <role> <name>]`; the decision
  event carries the reviewer identity. A dedicated reviewer column/UI is not
  added in this phase.
- ACP permission bridges (including stdio) and remote delegation approvals
  remain user-owned in this phase; remote ACP is asset-risk in the classifier.
- The reviewer is an LLM and can be wrong or prompt-injected; the deterministic
  asset gate, contract/sandbox/effect gates, and operator-visible events bound
  the blast radius. This is not a guarantee.
- Input redaction is the existing pattern/key redactor. An inline secret in a
  shell command that does not match a known pattern can still reach the
  reviewer provider; review inputs are bounded but not classified by entropy.

## Live validation (2026-11-28)

A Strict-mode one-task DeepSeek plan produced 7 supervisor reviews: 7 valid
decisions, 0 failures, 5 approve and 2 deny. Latency was 984–3,804ms (average
2,185ms), with 7,477 reviewer input / 2,560 output tokens. The Manager denied a
`git status` shell in favor of the narrower `git` tool and a Verifier
inline-code command; both agents adapted and T1 still merged. One Leader Strict
approval had no supervisor and was manually approved, exercising the user
fallback. The 10-task scale run's two review failures were the pre-fix
256-token truncation that motivated the 768-token structured tool call.

## Tests

- `approval_review::tests` covers asset classification, supervisor selection,
  reply parsing, and the dialectical/untrusted-data prompt.
- `agent::tests::strict_worker_write_is_reviewed_by_its_supervisor` runs a real
  Leader→Manager→Worker delegation in Strict mode; the Worker's write is
  approved by the Manager reviewer, then verified and merged without operator
  action.
