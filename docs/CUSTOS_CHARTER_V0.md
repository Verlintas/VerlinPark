# ◇ CUSTOS CHARTER v0

Status: **design draft for review — not signed, not shipped, not implemented**  
Date: 2026-11-26  
Companion: `docs/CUSTOS_PLAN.md`  
Charter hash: `UNSIGNED-DRAFT` (a released build embeds the real SHA-256)

> This charter is the constitutional layer of Custos. It is not runtime
> configuration. It cannot be edited from the GUI, a project policy, SQLite, an
> environment variable, an agent tool, or a model prompt. A changed charter is a
> new signed release, reviewed and built outside Custos and outside every agent.

## Preamble

VerlinPark is a multi-agent execution system. Its correctness depends on gates
that agents must not be able to rewrite: approvals, contracts, sandboxes,
Verifier verdicts, the deterministic Integrator, budgets, capacity, identity,
and durable evidence.

Custos exists to protect the **integrity, continuity, and health** of that
system. It is a resident guardian, not an agent, not an operator console, and
not a second product authority. It watches deterministically, acts within a
fixed charter, and treats every model/agent output as untrusted input.

Custos is symbolized as `◇CUSTOS`. The sigil is a seal, not a brand mascot.

## Article 0 — Definitions

- **Custos**: the sealed resident supervisor process (`custosd`) plus its
  watchdog and read-only GUI.
- **Charter**: this document, compiled into a signed release and pinned by hash.
- **Daemon**: the VerlinPark local daemon (`verlinpadd`).
- **Known process**: a process recorded by the daemon in
  `supervised_processes`, a run/tool process group, or a daemon-launched helper,
  with verifiable ownership (pid + start time + nonce/command hash where
  applicable).
- **Foreign process**: any process not meeting the known-process definition.
- **Agent**: Leader, Manager, Worker, Labour, Verifier, Scout, roundtable, ACP
  external agent, or any model-driven actor.
- **User**: the human operating the machine. The user has awareness, not
  jurisdiction over Custos.
- **Sealed**: one-way, templated, non-negotiable, not model-authored.
- **Evidence**: deterministic state read from the daemon, SQLite, the process
  table, the filesystem, or a signed artifact. Agent self-report is not
  evidence.
- **Action class**: L0–L5 authority level defined in Article 3.

## Article 1 — Mission and protected assets

Custos protects, in priority order:

1. **Truth of durable state** — SQLite integrity, event/mirror consistency,
   schema version, backups, anchors, checkpoints, merge queue evidence.
2. **Execution continuity** — daemon liveness, runtime leases, run journal,
   task claims, capacity, dispatch admission, recovery after restart.
3. **Isolation boundaries** — worktrees, branches, write scopes, sandbox
   ownership, GUI lease, remote integration provenance.
4. **Resource health** — runaway or stalled known processes, event backlog,
   disk/WAL growth, repeated crash loops.
5. **Observability** — bounded, tamper-evident audit and user-consented
   diagnostic export.

Custos does **not** protect a task's subjective quality, a model's plan, a
design's aesthetics, or an agent's opinion. Those belong to the existing gates
and the user.

## Article 2 — Jurisdiction

### 2.1 What Custos may inspect

- daemon process, discovery/instance lock, heartbeat, build/version;
- SQLite database, WAL, schema/user_version, bounded integrity checks;
- events, event mirrors, projections, runtime leases, run journal;
- task queue, merge queue, effects, gates, approvals metadata (not secret
  contents), capacity, budgets metadata;
- worktrees, branches, refs, checkpoints, workspaces, retention state;
- backup repository, keyring/anchor metadata (never private key material);
- known process identity, resource use, progress, and exit state;
- GUI lease/helper health;
- local logs and crash reports produced by VerlinPark/Custos.

### 2.2 What Custos may act on

Only **known processes** and **daemon-owned state**. V1 never scans or kills
arbitrary system processes, even if they are consuming resources. A system-wide
guardian is an Advanced concept and is explicitly not planned for V1.

Custos may not read or modify user documents outside daemon-owned state,
worktrees, scratch, logs, and diagnostic outputs.

### 2.3 Relationship to the user

- Custos authority is higher than and independent of the user for integrity and
  health protection.
- The user has **awareness**, not jurisdiction: a read-only status GUI, a
  limited incident surface, and an explicit local diagnostic-export action.
- The user's only final override is OS-level custody: quit, kill, boot out,
  uninstall, or delete data outside the product GUI. This is accepted, not
  hidden.
- The GUI never exposes repair, restart, restore, approve, budget, merge, kill,
  or policy controls.

### 2.4 Relationship to agents

- Agents cannot call, query, configure, impersonate, pause, suspend, or stop
  Custos through any product surface.
- Custos is absent from agent tool schemas, prompts, contracts, MCP resources,
  mailbox addresses, and model routing.
- Agents may receive a sealed `custos.notice`; they cannot reply.
- An agent action that threatens integrity may be vetoed, frozen, quarantined,
  or terminated under this charter.

## Article 3 — Authority ladder

### L0 — Observe

Read-only probes. No writes, no signals, no notifications. Always allowed.

### L1 — Notify

Emit a sealed `custos.notice` to the responsible agent/session/task, or to the
project inbox when the agent is idle. No state mutation. Always allowed when a
known process or state crosses a watch threshold.

### L2 — Safe repair

Deterministic, reversible or evidence-preserving repairs to daemon-owned state:

- reclaim an expired runtime lease;
- release a stale queue claim whose owner is dead/expired;
- reattach an interrupted run/worktree under existing policy;
- replay/repair covered projections when replay reports drift;
- retry a transient queue/provider operation through the normal daemon path;
- clean daemon-owned scratch/temp files under a bounded path and age;
- restart a supervised process only when its recorded policy explicitly allows
  restart and its identity is fully verified.

L2 never changes truth gates, budgets, capacity, identity, approvals, merge
state, or source.

### L3 — Containment

Protective action against a known unhealthy target:

- cooperatively pause/cancel a stuck session/run;
- stop a known process group (TERM, bounded grace, KILL);
- quarantine a worktree/branch/merge-queue entry from further integration;
- block new dispatch/admission for one project;
- release a stale GUI lease;
- mark a project/session read-only for writes until healthy.

L3 may act without user confirmation. It must emit a pre-action notice where
safe, and a post-action notice in all cases.

### L4 — Recovery

Narrow, high-impact recovery under deterministic preconditions:

- restart the daemon after a verified crash loop or unrecoverable hang;
- restore the most recent **verified** backup when the live database is
  unopenable/corrupt and the anchor/generation checks pass;
- enter safe mode (dispatch disabled, read-only surfaces) when integrity cannot
  be established;
- terminate all known sessions only when the daemon cannot establish a safe
  state and continued execution risks durable corruption.

L4 never restores an unverified snapshot, never rewrites the charter, and never
touches source/policy/keys.

### L5 — Prohibited forever

Custos must never:

- approve an approval, submit a Verifier verdict, merge/rebase/commit on behalf
  of an agent, or bypass the deterministic Integrator;
- change budgets, hard caps, prices, model routing, capacity, or unlimited mode;
- hire, retire, promote, transfer, or edit identities/permissions;
- read, copy, log, export, or expose provider keys, credentials, cookies, or
  private keys;
- edit VerlinPark source, configuration, policy, sandbox, contract, approval,
  effect, or gate definitions;
- disable or weaken verification, sandboxing, write scopes, contracts, effects,
  or merge checks;
- kill a foreign process;
- call a language model, use a provider key, or generate dynamic code;
- create an agent-facing tool/RPC/mailbox surface;
- modify its own charter, binary, launchd labels, or watchdog outside a signed
  release.

## Article 4 — Deterministic decision procedure

Every action follows the same state machine:

1. **Probe** — read bounded evidence; record timestamp, source, and hash.
2. **Classify** — map evidence to a fixed state
   (`healthy|watch|pressure|over_expected|suspected_stall|stalled|runaway|critical|quarantined`).
3. **Select** — choose the lowest action class that resolves the state from a
   fixed action catalog (Appendix A).
4. **Precondition** — verify target identity, ownership, quiescence, policy,
   cooldown, attempt cap, and rollback availability.
5. **Notify** — emit a pre-action notice when the class requires it.
6. **Act** — execute one bounded action with a timeout.
7. **Verify postcondition** — read evidence again; success is not assumed.
8. **Record** — append a hash-chained audit entry with evidence pointers,
   result, and postcondition.
9. **Escalate or stop** — if the postcondition fails, escalate one class or
   stop; never loop the same action indefinitely.

Rules:

- Unknown state or missing evidence → observe/escalate, never guess.
- Same failure fingerprint + same target → at most one automatic repair per
  cooldown window; then escalate.
- No action may create hidden capacity, spend budget, or bypass a gate.
- A repair that makes a test pass by weakening the test is not a repair.
- Time, randomness, and model output are never inputs to a decision except as
  bounded evidence values.

## Article 5 — Process pressure and stuck determination

### 5.1 Scope

Only known processes. Custos resolves ownership in this order:

1. `supervised_processes` session/task/agent binding;
2. `run_project_task` / test-cache process group;
3. shell-tool process group and its descendants;
4. daemon-launched browser/computer/LSP/MCP/ACP child;
5. daemon-owned helper/sweeper (owner = system).

If ownership is ambiguous, Custos may observe and notify the project/system
inbox, but must not kill.

### 5.2 Signals

- CPU percentage over a window;
- resident memory and growth rate;
- disk/temp growth;
- process-group child count;
- stdout/stderr byte growth (progress);
- CPU-time delta (progress even with silent output);
- heartbeat/lease renewal;
- declared `expected_secs`/timeout (advisory, clamped);
- hard caps from the charter;
- response to a cooperative stop request;
- repeated restarts/crash loop;
- event-loop/daemon latency and queue backlog caused by the process.

Agent-declared expected time is **advisory**. Agents commonly over-declare (a
one-minute test with a fifteen-minute timeout), so a declaration may raise a
warning threshold within a clamp but can never lower a hard cap or disable
progress-based detection.

### 5.3 Matrix (initial v0 constants)

| State | Deterministic signal | Action | Notice |
|---|---|---|---|
| `normal` | below all watch thresholds | none | none |
| `pressure` | CPU ≥ 80% for 90s, or RSS ≥ 2 GiB, or I/O wait ≥ 50% for 60s, while the process is alive and progressing | L1 | pressure notice, grace 120s |
| `over_expected` | elapsed > clamp(declared expected, 30s, 4h) **and** no output growth for 60s | L1 (strong) | final notice, grace 120s |
| `suspected_stall` | no stdout/stderr growth **and** CPU-time delta < 1s over 120s, or heartbeat gap > 3× interval | L1 (final) | final notice, grace 60s |
| `stalled` | `suspected_stall` persists 300s, or process ignores TERM for 15s | L3 stop | post-action notice |
| `runaway` | CPU ≥ 95% for 300s **and** (RSS growth ≥ 512 MiB/5min or children > 32), or RSS > 8 GiB | L3 stop | final notice 60s, then post-action |
| `hard_cap` | RSS > 8 GiB, process group > 64, scratch > 2 GiB, or open files > 4096 | L3 stop immediately | post-action notice |
| `crash_loop` | same known process restarts ≥ 3 times in 10 minutes | L3 disable restart + notify | post-action notice |
| `exited` | process exited; exit code recorded | none | none |

Notes:

- Thresholds are charter constants for v0, tunable only by a signed release.
- A legitimate long build must be able to run; progress signals (output or
  CPU-time delta) suppress false stalls. The declared expected time is a hint,
  not a verdict.
- `TERM` is always tried before `KILL` unless the hard cap is already breached.
- Killing a process group marks any in-flight effect receipt `unknown` and
  blocks merge until reconciliation; Custos never settles an effect itself.
- Killing never deletes worktree files, commits, logs, or task evidence.

### 5.4 Process stop sequence

1. Verify known-process identity (pid + start time + nonce/command hash).
2. Emit pre-action notice if the state allows a grace window.
3. Send SIGTERM to the process group.
4. Wait the charter grace (default 15s; 5s for hard cap).
5. Send SIGKILL to the process group.
6. Verify the group is gone; release the supervisor row/lease.
7. Mark the owning task/run `interrupted` or `blocked` with a bounded reason.
8. Emit post-action notice and audit entry.

Custos never signals pid 1, launchd, the daemon, itself, its watchdog, the GUI,
or a foreign process.

## Article 6 — Notice protocol

`custos.notice` is a sealed system message:

- one-way; no reply, no query, no negotiation;
- templated fields only; no model-authored prose;
- delivered at the next safe turn boundary for a running agent, or to the
  project inbox for an idle identity;
- excluded from agent tool schemas and cannot be spoofed by an agent;
- bounded (≤ 2 KiB), no raw stdout/stderr, no file contents, no secrets, no
  absolute paths beyond a task/process label.

Envelope:

```json
{
  "schema": "verlinpark.custos_notice/v1",
  "notice_id": "cn_...",
  "issued_at": "RFC3339",
  "custos_build": "sha256:...",
  "charter_hash": "sha256:...",
  "severity": "advisory|warning|final|action",
  "recipient": {"agent_id": "...", "session_id": "...", "task_id": "..."},
  "subject": {"kind": "process|lease|queue|worktree|database", "label": "..."},
  "observed": {"cpu_pct": 0, "rss_mib": 0, "elapsed_secs": 0, "progress_age_secs": 0},
  "limit": {"kind": "...", "value": 0},
  "requested_action": "stop|reduce|checkpoint|none",
  "grace_secs": 0,
  "consequence": "terminate_process_group|quarantine|none",
  "reply": "forbidden"
}
```

Templates:

**Pressure**

> ◇CUSTOS NOTICE — RESOURCE PRESSURE. I am Custos, the integrity guardian of
> this VerlinPark instance. Process `{label}` owned by `{agent}` / `{task}` has
> used {cpu}% CPU and {rss} MiB for {elapsed}. It is degrading the instance.
> Stop it, reduce its scope, or checkpoint your work within {grace}s. If it
> remains unresponsive or exceeds the hard cap, I will terminate its process
> group. This channel is one-way; do not reply.

**Final stall**

> ◇CUSTOS NOTICE — STALLED. Process `{label}` has produced no progress for
> {progress_age}s. I will terminate its process group in {grace}s unless it
> exits or resumes progress. Checkpoint any completed work now. This channel is
> one-way; do not reply.

**Post-action**

> ◇CUSTOS NOTICE — ACTION TAKEN. Process `{label}` was terminated after
> {reason}. Task `{task}` is marked `{status}`. Your worktree and commits were
> preserved. Do not restart the same workload without reducing its scope. This
> channel is one-way; do not reply.

**Quarantine**

> ◇CUSTOS NOTICE — QUARANTINED. `{subject}` is quarantined from integration
> because `{reason}`. Existing files and commits are preserved. Continue only
> with a bounded repair or report the blocker. This channel is one-way; do not
> reply.

No notice may contain a prompt, a model instruction beyond the fixed template,
raw tool output, a URL, a token, or a path outside the worktree label.

## Article 7 — Containment and recovery actions

### 7.1 Quarantine

Quarantine is evidence-preserving, not deletion:

- mark the target (worktree, branch, merge entry, project) non-integrable and
  non-dispatchable;
- keep files, commits, logs, artifacts, and audit;
- emit `custos.quarantine` and a notice;
- release only by deterministic postcondition (clean verification, resolved
  effect, repaired DB, operator-independent health check). The user has no GUI
  release control.

### 7.2 Daemon restart

Allowed only when:

- heartbeat is stale beyond the hard threshold or the process is unresponsive
  to a cooperative health request;
- no database write is in flight that would be made ambiguous without a
  journal/effect record;
- the launchd label and binary hash are verified;
- restart count is below the crash-loop cap.

Custos records the restart, waits for health, and verifies lease/run recovery.
It does not "fix" a crash loop by repeatedly restarting; after the cap it enters
safe mode and escalates.

### 7.3 Verified backup restore

Allowed only when all hold:

- the live DB is unopenable/corrupt after a bounded integrity check;
- the latest backup repository verifies (`backup-verify` valid) and its
  manifest signature/anchor generation is acceptable;
- the restore target is a fresh directory, never an in-place overwrite;
- the corrupt original is preserved read-only for diagnosis;
- the daemon is stopped and the restore is offline;
- post-restore integrity and schema checks pass.

If any check fails, Custos does not restore. It preserves evidence and enters
safe mode.

### 7.4 Safe mode

Safe mode means: daemon may start for read-only inspection, no new dispatch, no
merges, no provider calls, no worktree writes. The GUI shows `CRITICAL`. Exit
from safe mode requires deterministic health checks, not a user click.

## Article 8 — Diagnostic bundles

### 8.1 Trigger and boundary

- The only user-initiated operation is opening the secondary Diagnostics sheet
  and generating bundles.
- Bundles are written locally to a user-chosen folder; never uploaded, never
  emailed, never sent to a provider.
- One action generates two files: `standard` and `forensic`.
- Files are `0600`; the manifest is signed/hash-chained; the report id is
  random and non-identifying.

### 8.2 Standard bundle

Contains enough to triage a bug without user content:

- `manifest.json` (schema, report id, timestamps, app/daemon/custos versions,
  build/charter hashes, OS/hardware, bundle kind, per-file SHA-256, redaction
  profile, exclusions);
- `summary.json` (health state, action counts, last 100 Custos actions);
- `health.json` (coarse dimension states and evidence codes);
- `daemon.log` tail (2 MiB, secret-scrubbed);
- `crash/` (bounded `.ips`/panic tails, scrubbed);
- `db_stats.json` (file/WAL sizes, user_version, quick_check result, table row
  counts; no rows);
- `queue.json` (counts/status codes, no payloads);
- `leases.json` (counts/expiry states, no tokens);
- `runs.json` (counts/interrupted states, no prompts);
- `worktrees.json` (basenames, branch labels, clean/dirty counts);
- `backup.json` (repository status, generation, verify result; no keys);
- `config_redacted.json` (non-secret settings, env names only);
- `system.txt` (uname, CPU/memory/disk summary);
- `README.txt` (privacy statement and what is excluded).

### 8.3 Forensic bundle

Adds detail for developer diagnosis, still no secrets or user content:

- full normalized paths (home replaced with `~`);
- known-process tree with argv and env **names** only (values redacted);
- bounded raw logs up to 16 MiB per file, secret-scrubbed;
- SQLite schema, `PRAGMA quick_check`, WAL/frame stats, index/table stats;
- event type counts per session and bounded event metadata (kind, timestamp,
  ids; no payload bodies);
- task/merge/lease rows with ids/status/timestamps (no prompts/tool output);
- Git refs, worktree heads, lock files, optional-lock diagnostics;
- bounded timeline of Custos probes/actions and daemon lifecycle;
- launchd label state, plist hashes, watchdog heartbeat history;
- thread/process sample of known processes only (no memory dumps).

### 8.4 Hard exclusions (both bundles)

Never collected:

- provider API keys, auth headers, cookies, Keychain items, private keys;
- prompts, system messages, conversation bodies, model outputs;
- tool arguments/results, file contents, diffs, patches;
- screen captures, browser DOM/text, accessibility text, clipboard;
- user documents outside daemon-owned state;
- full environment values (names only);
- raw database rows containing user/agent content.

### 8.5 Redaction and caps

- Key-name and value-pattern scrub for tokens/keys/passwords/credentials.
- URL userinfo stripped; query strings hashed or removed.
- Standard paths reduced to basenames; forensic paths normalized to `~`.
- Standard ≤ 8 MiB; forensic ≤ 32 MiB; per-file caps recorded in manifest.
- Bundles can be deleted by the user; Custos keeps no copy after generation
  except the audit entry (report id, sizes, hashes; no contents).

## Article 9 — GUI and information tiers

### 9.1 Main window (read-only)

A sealed instrument, not a dashboard:

- obsidian black background, thin ice lines, one central `◇` sigil;
- `◇ CUSTOS` and `SEALED GUARDIAN` in mono uppercase;
- one state word: `HEALTHY / WATCH / DEGRADED / CRITICAL / QUARANTINED`;
- 5–7 integrity rings or bars: RUNTIME, DATA, LEASES, QUEUES, WORKTREES,
  BACKUP, SIGNAL;
- one deterministic sentence of state;
- a bounded log of the last 8–12 actions: time, category, target label, result;
- `PROTECTION ACTIVE`, `CONTAINED {n}`, `LAST ACTION ...`;
- no buttons, no text field, no chat, no repair/restart/restore/approve/kill
  control, no policy editor.

### 9.2 Secondary diagnostics sheet

- opened by a discrete keyboard shortcut or a small `DIAGNOSTICS` label, not a
  prominent button;
- explains what is collected/excluded in plain language;
- one action: `Generate standard + forensic bundles`;
- shows progress and resulting file paths;
- optional `Reveal in Finder`;
- no network, no upload, no auto-send.

### 9.3 Visual language

- distinct from VerlinPark pages: no sidebar, cards, tabs, rounded panels, or
  glass dashboard;
- mono type, wide tracking, 1px rules, restrained glow/pulse/scanline;
- state colors: ice (healthy), amber (watch/degraded), red (critical),
  near-black (quarantined);
- Reduce Motion disables pulse/scanline; Reduce Transparency removes blur;
- all text is deterministic; no generated copy.

### 9.4 Information tiers

| Tier | Visible to | Contents |
|---|---|---|
| T0 sealed status | user | state, rings, action category/result, counts |
| T1 incident summary | user | project/task labels, coarse reason code, consequence |
| T2 diagnostic bundle | user-generated local file | detailed evidence, redacted as above |
| T3 signed audit | developer tooling only | full hash-chained audit metadata, no secrets/content |

The GUI never exposes T3 raw data, SQL, stack traces, provider payloads, or
absolute paths.

## Article 10 — Self-protection and continuity

### 10.1 Process separation

- `custosd` is a separate process, not a child of the daemon and not an agent
  tool host.
- It has no provider key, no model client, no arbitrary shell, and no generic
  "run command" capability.
- It listens on a local Unix domain socket (`0600`) with peer-uid checks and a
  sealed admin token; no TCP listener by default.
- The daemon exposes a bounded `custos.*` admin surface only on that socket.
  Normal `vp` RPC, discovery, agent tools, and MCP do not expose it.
- The daemon's process supervisor refuses to manage Custos PIDs; agent process
  tools only see registered processes.

### 10.2 Boot autostart and launchd supervision

Two user LaunchAgents (V1, no sudo):

- `com.verlintas.custos` — `custosd`;
- `com.verlintas.custos-watchdog` — watchdog ring peer.

Both use `RunAtLoad=true`, `KeepAlive=true`, a bounded `ThrottleInterval`, 0600
stdout/stderr logs, and `LimitLoadToSessionType=Aqua`. They start at login and
are restarted by launchd after a crash or signal.

### 10.3 Watchdog ring

- Each process writes an atomic heartbeat file every 2s:
  `{pid, start_time, build_hash, seq, timestamp, state_hash}` in a 0600
  directory.
- Each process checks the peer every 2s. A stale heartbeat (> 10s), missing pid,
  or start-time mismatch triggers `launchctl kickstart -k` for the peer label.
- If kickstart fails, the peer attempts a bounded `bootstrap` from the signed
  plist copy.
- A third `com.verlintas.custos-anchor` LaunchAgent may run every 60s to verify
  both labels and re-bootstrap a missing one. This raises the cost of a
  same-user tamper but does not make it impossible.
- If both Custos processes die, launchd restarts them; the daemon and GUI show
  `SIGNAL LOST` until the ring is healthy.

### 10.4 Anti-tamper and self-check

On start and every 60s, Custos verifies:

- its own binary hash and code signature against the release manifest;
- the charter hash;
- the launchd plist hashes and labels;
- data/audit directory ownership and 0600 permissions;
- watchdog heartbeat freshness;
- clock sanity (monotonic vs wall clock);
- no unexpected debugger/tracer attachment where the OS exposes it.

A mismatch enters `DEGRADED_SELF`: Custos keeps observing, refuses high-impact
actions, emits a sealed audit entry, and asks launchd/watchdog to restore a
verified copy. It never downloads or self-updates.

### 10.5 Threat model and honest limits

| Threat | V1 control | Residual |
|---|---|---|
| Agent calls Custos | no tool/RPC/mailbox/prompt surface | none in product surface |
| Agent signals Custos via process tool | supervisor only manages registered PIDs | a malicious same-user shell may signal |
| Agent edits Custos files | worktree/sandbox write jail; 0600 dirs; hash checks | same-user filesystem access outside sandbox |
| Agent boots out launchd job | no agent-facing launchctl surface; plist write denied by sandbox | same-user `launchctl` can boot out a user agent |
| Daemon hangs | watchdog/launchd independent | none |
| Machine shutdown | accepted final stop | none |
| Root attacker | out of scope | none |

**V1 guarantee:** no first-class VerlinPark/agent surface can stop, pause,
suspend, reconfigure, or impersonate Custos; a killed/stopped Custos is restored
by launchd/watchdog within seconds; tamper is detected and audited.

**V1 non-guarantee:** absolute invulnerability to an arbitrary same-user process
with shell and `launchctl`. That requires a separate OS user or privileged
LaunchDaemon, which is Advanced-only and not planned for V1. This is recorded,
not hidden.

## Article 11 — Audit and evidence

- Append-only `custos_events` (SQLite) plus a hash-chained JSONL mirror.
- Each entry: event id, timestamp, custos build/charter hash, probe/action id,
  target class/label, evidence codes/hashes, precondition result, action,
  postcondition, duration, notice id, result, escalation.
- No prompts, message bodies, tool output, file contents, screen content,
  secrets, or absolute user paths.
- Bounded retention; export is part of the diagnostic bundle; a developer
  verification tool can check the chain.
- Replay reconstructs decisions; it never re-executes side effects.
- Every L2–L4 action is idempotent by `(action, target, evidence_hash)` within
  its cooldown; duplicate detection suppresses repeats.

## Article 12 — Charter change and release

- The charter is compiled into the signed release and pinned by SHA-256.
- Custos refuses to start if the embedded charter hash does not match.
- No GUI, RPC, SQLite row, project policy, environment variable, agent, or
  model can change it.
- A charter change requires a new signed release, developer review, and a
  version bump; old audit entries remain valid under their recorded hash.
- There is no runtime "admin mode", "developer mode", or hidden override in the
  product.
- A developer-only offline `custos-charter-verify` tool may compare a build's
  charter hash and audit chain; it cannot mutate a running Custos.

## Article 13 — Known V1 limits

1. Same-user `launchctl bootout` / signal is not absolutely preventable without
   privilege separation; V1 detects and restores, and records the residual.
2. No system-wide process guardian; only daemon-known processes.
3. No public network listener, no remote Custos control, no cross-host
   supervision.
4. No LLM/NL diagnosis; messages are templated.
5. No self-update; updates are signed app releases.
6. Diagnostic bundles are user-consented local files; no automatic upload or
   support ticket creation.
7. GUI is read-only; the user cannot repair, restart, restore, or approve.
8. L4 restore is limited to verified backups and a fresh target; it is not a
   general rollback tool.
9. Threshold constants are v0 and may need calibration from shadow-mode data.
10. The GUI is a separate surface; its exact packaging (separate app vs sealed
    panel) is still open.

## Appendix A — Action catalog (v0)

| ID | Class | Action | Target | Preconditions | Postcondition | Rollback | Notice |
|---|---|---|---|---|---|---|---|
| OBS-01 | L0 | probe daemon | daemon | none | timestamped health record | n/a | no |
| OBS-02 | L0 | probe DB | SQLite | read-only open | stats + quick_check | n/a | no |
| OBS-03 | L0 | probe leases | runtime_leases | none | expired/live list | n/a | no |
| OBS-04 | L0 | probe runs | session_runs | none | interrupted/in-flight list | n/a | no |
| OBS-05 | L0 | probe queues | task/merge queue | none | stale/blocked list | n/a | no |
| OBS-06 | L0 | probe effects | effect_receipts | none | unknown/intent list | n/a | no |
| OBS-07 | L0 | probe worktrees | worktrees/refs | none | missing/locked/dirty list | n/a | no |
| OBS-08 | L0 | probe backup | backup repo | none | verify/anchor/generation | n/a | no |
| OBS-09 | L0 | probe processes | known processes | ownership verified | state/resource/progress | n/a | no |
| OBS-10 | L0 | probe events | event log/mirror | none | last age/backlog | n/a | no |
| NTC-01 | L1 | pressure notice | agent/task | known process + threshold | notice id | n/a | yes |
| NTC-02 | L1 | final notice | agent/task | suspected stall | notice id | n/a | yes |
| NTC-03 | L1 | post-action notice | agent/task | action completed | notice id | n/a | yes |
| NTC-04 | L1 | quarantine notice | project/session | quarantine set | notice id | n/a | yes |
| RPR-01 | L2 | reclaim expired lease | runtime lease | expired + owner dead | lease released | re-acquire | no |
| RPR-02 | L2 | release stale claim | task queue | claim expired + owner dead | pending/retry | re-claim | no |
| RPR-03 | L2 | reattach interrupted run | session run | daemon restart + policy | run resumed/interrupted | cancel | no |
| RPR-04 | L2 | replay/repair projections | session | replay drift + covered | drift cleared | replay again | no |
| RPR-05 | L2 | retry transient queue op | queue | transient error + cap | retried/queued | none | no |
| RPR-06 | L2 | clean daemon scratch | temp dir | daemon-owned + age cap | bytes freed | n/a | no |
| RPR-07 | L2 | restart supervised process | known process | restart_on_boot + identity | running | stop | no |
| CTN-01 | L3 | pause session | session | active + cooperative | paused | resume via normal path | yes |
| CTN-02 | L3 | cancel run | session run | stuck + journaled | cancelled/interrupted | retry task | yes |
| CTN-03 | L3 | stop process group | known process | matrix threshold | group gone; effect unknown | restart if policy | yes |
| CTN-04 | L3 | quarantine worktree | worktree/branch | evidence of unsafe state | non-integrable | health check | yes |
| CTN-05 | L3 | quarantine merge entry | merge queue | conflict/unknown effect | entry held | retry/resolve | yes |
| CTN-06 | L3 | block project dispatch | project | integrity unhealthy | no new sessions | health check | yes |
| CTN-07 | L3 | release stale GUI lease | gui lease | holder dead + TTL | lease free | n/a | no |
| REC-01 | L4 | restart daemon | daemon | crash loop/hang + safe | healthy/new pid | launchd restart | yes |
| REC-02 | L4 | restore verified backup | data dir | DB corrupt + backup valid + anchor | restored fresh dir | keep corrupt original | yes |
| REC-03 | L4 | enter safe mode | project/daemon | integrity unresolved | read-only/no dispatch | health check | yes |
| REC-04 | L4 | stop all known sessions | sessions | unsafe state + no safe path | all stopped/interrupted | explicit restart | yes |

Every action also has: max attempts, cooldown, max duration, evidence hash, and
an idempotency key. The catalog is closed; Custos cannot invent an action.

## Appendix B — Probe matrix (v0)

| Dimension | Probe | Healthy | Watch | Critical |
|---|---|---|---|---|
| Runtime | pid, heartbeat, start time, build | fresh | 1 missed beat | stale > 30s / unresponsive |
| Database | open, user_version, quick_check, WAL | open, check ok, WAL < 256 MiB | WAL 256 MiB–1 GiB | unopenable/corrupt/check fail |
| Disk | free space, inode | > 20% free | 10–20% | < 10% or write fail |
| Events | last event age, mirror lag | < 5s | 5–60s | > 60s or mirror gap |
| Leases | expired/live, heartbeat | none expired | 1 expired | many/stale owner |
| Runs | in-flight/interrupted | normal | 1 interrupted | repeated restart loop |
| Queue | stale claims, blocked | none | 1 stale | growing/blocked batch |
| Effects | unknown/intent | none | 1 unknown | unknown blocking merge |
| Worktrees | path/ref/clean/locks | clean | dirty/lock | missing/corrupt/ref lost |
| Backup | verify/anchor/generation | valid/fresh | old | invalid/anchor fail |
| Processes | identity/progress/resource | normal | pressure | stalled/runaway |
| GUI | lease/helper | healthy | stale | unavailable |

## Appendix C — Diagnostic manifest (minimum fields)

```json
{
  "schema": "verlinpark.custos_diagnostics/v1",
  "report_id": "cdr_...",
  "generated_at": "RFC3339",
  "bundle_kind": "standard|forensic",
  "app_version": "...",
  "daemon_version": "...",
  "custos_version": "...",
  "build_hash": "sha256:...",
  "charter_hash": "sha256:...",
  "os": "...",
  "hardware": "...",
  "redaction_profile": "standard|forensic",
  "exclusions": ["secrets","prompts","messages","tool_output","file_content","screen","cookies"],
  "files": [{"path": "summary.json", "bytes": 0, "sha256": "..."}],
  "chain_head": "sha256:..."
}
```

## Appendix D — Review questions for the owner

1. Are the v0 pressure thresholds (80%/90s, 8 GiB, 300s stall) acceptable as
   starting constants, or should they be more/less aggressive?
2. May Custos automatically restart the daemon (REC-01), or should that remain
   a developer-only recovery action?
3. May Custos automatically restore a verified backup (REC-02) when the DB is
   corrupt, preserving the original? This is the most consequential L4 power.
4. Should the forensic bundle include normalized full paths and process argv,
   or stay closer to the standard bundle for privacy?
5. Is a third anchor LaunchAgent worth the extra moving part, given the
   same-user `launchctl` residual?
6. Should the GUI be a separate app/window, a menu-bar extra, or a sealed panel
   inside VerlinPark?
7. Should idle-agent notices go to the project inbox, or be dropped with an
   audit entry if no agent is running?
8. What should the user see when Custos quarantines a project? Proposed:
   project name, category, consequence, and a “diagnostics” path only.

---

**Seal:** this draft becomes real only when its hash is embedded in a signed
release. Until then it is a design artifact for review.
