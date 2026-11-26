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

- **Custos**: the sealed resident role ring (`custosd core|warden|anchor`)
  plus its signed baseline/vault, watchdog heartbeats, and read-only GUI.
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
project inbox when the agent is idle. No state mutation. Allowed when a known
process or state crosses a notice threshold (score ≥ 60 or an equivalent state
rule).

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

### L4 — Recovery (authorized, quorum-gated)

Narrow, high-impact recovery under deterministic preconditions:

- restart the daemon after a verified crash loop or unrecoverable hang;
- restore the most recent **verified** backup when the live database is
  unopenable/corrupt and the anchor/generation checks pass;
- enter safe mode (dispatch disabled, read-only surfaces) when integrity cannot
  be established;
- terminate all known sessions only when the daemon cannot establish a safe
  state and continued execution risks durable corruption.

L4 actions require 2-of-3 role agreement and the Article 7 preconditions. L4
never restores an unverified snapshot, never rewrites the charter, and never
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
   (`normal|watch|pressure|suspected_stall|stalled|runaway|hard_cap|crash_loop|critical|quarantined|exited`).
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

### 5.2 Signals (advanced probe stack)

The v0 design deliberately avoids a single `ps`/CPU-percent trigger. The
primary probe is macOS `proc_pid_rusage` (`rusage_info_v4`), which is cheap and
provides a progress vector without fork/exec:

- user/system CPU time, instructions retired, cycles, energy;
- disk bytes read/written, logical writes;
- pageins, faults, wakeups, context switches;
- resident memory and memory growth.

Supporting probes:

- `proc_pidinfo(PROC_PIDTASKINFO)`: state, thread count, RSS, virtual size,
  pageins, faults;
- `proc_pidinfo(PROC_PIDLISTFDS)` on a watch state only: open-file count;
- `proc_listchildpids`: aggregate the process group and count live children;
- supervisor output growth (stdout/stderr bytes), child-exit events;
- run/task/lease heartbeat sequence;
- bounded `sample` (1 second, top frames only) for a suspected stall, used as
  evidence only, never as the sole kill trigger;
- daemon event-loop latency, queue backlog, memory pressure, free disk, load
  average, and scratch/temp growth caused by the process;
- declared `expected_secs`/timeout (advisory, clamped);
- charter hard caps;
- response to a cooperative stop request;
- repeated restarts/crash loop.

Agent-declared expected time is **advisory**. Agents commonly over-declare (a
one-minute test with a fifteen-minute timeout), so a declaration may raise a
warning threshold within a clamp but can never lower a hard cap or disable
progress-based detection.

**Progress vector P** (sampled every 5s over a 60s window) is any positive
delta in: output bytes, CPU time, instructions/cycles, disk I/O, child exit
count, or heartbeat sequence. A process is `progressing` if any P component
moved in the last 60s. A process is `flat` only when every P component is
unchanged for the stated duration.

### 5.3 Deterministic pressure score

Custos computes a bounded score (0–100) from independent signal families:
**resource** (CPU/memory/disk/children/fds), **progress** (P flat), and
**control** (heartbeat, TERM response, declared expectation).

| Signal | Points |
|---|---:|
| CPU ≥ 95% for 180s | +20 |
| RSS ≥ 6 GiB | +20 |
| RSS growth ≥ 1 GiB/min for 3 min | +30 |
| disk write ≥ 1 GiB/min for 5 min | +25 |
| process-group children ≥ 128 | +25 |
| open files ≥ 8192 | +15 |
| no progress for 5 min | +20 |
| no progress for 10 min | +35 |
| no progress for 15 min | +45 |
| heartbeat gap > 3× interval | +30 |
| declared expected exceeded + no output 3 min | +10 |
| TERM ignored for 30s | +40 |
| bounded stack sample shows all threads blocked | +10 |

The no-progress rows are bands: apply only the highest matching duration, not
all three. Resource signals may stack because they are independent. Score is
capped at 100. Actions:

| State | Condition | Action | Notice |
|---|---|---|---|
| `normal` | score < 40 | none | none |
| `watch` | 40–59 | log only | none |
| `pressure` | 60–79 | L1 | pressure notice, grace 120s |
| `suspected_stall` | 80–89, no progress ≥ 5 min | L1 final | final notice, grace 120s |
| `stalled` | score ≥ 90, no progress ≥ 10 min, **and** ≥ 2 signal families | L3 stop | post-action |
| `runaway` | score ≥ 90 with memory/disk/children growth, **and** ≥ 2 signal families | L3 stop | final notice 30s, then post-action |
| `hard_cap` | RSS > 12 GiB, children > 128, scratch > 8 GiB, or open files > 8192 | L3 stop immediately | post-action |
| `crash_loop` | same known process restarts ≥ 5 times in 15 minutes | L3 disable restart + notify | post-action |
| `exited` | process exited; exit code recorded | none | none |

Kill guards:

- A kill requires at least **two independent signal families** (for example
  resource + progress, or progress + control). A single high CPU number is not
  enough.
- Any progress in the last 60s blocks a kill; the maximum action is a notice,
  except for a hard cap.
- A process that exits on TERM is never sent KILL.
- A legitimate long build is protected by instructions/CPU-time progress even
  with no stdout.
- The declared expected time can only raise a threshold within the clamp
  `[30s, 8h]`; it cannot suppress progress detection or hard caps.
- Thresholds and weights are charter constants for v0, tunable only by a
  signed release.

### 5.4 Process stop sequence

1. Verify known-process identity (pid + start time + nonce/command hash).
2. Verify the target is not Custos, its peers, launchd, the daemon, the GUI, or a
   foreign process.
3. Emit pre-action notice if the state allows a grace window.
4. Send SIGTERM to the process group.
5. Wait the charter grace (default 30s; 5s for hard cap).
6. If the process is still alive, send SIGKILL to the process group.
7. Verify the group is gone; release the supervisor row/lease.
8. Mark the owning task/run `interrupted` or `blocked` with a bounded reason.
9. Emit post-action notice and audit entry.

L3 process stops are executed by the warden role with a core intent and an
anchor audit. If the core is unavailable, the warden may execute a hard-cap stop
alone and record it for quorum audit; it may not start a new discretionary
containment until the core is restored.

Custos never signals pid 1, launchd, the daemon, itself, its peers, the GUI, or
a foreign process.

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

### 7.2 Daemon restart (automatic, quorum-gated)

Custos is **authorized to restart the daemon automatically** through the warden
when all hold:

- heartbeat is stale beyond the hard threshold or the process is unresponsive
  to a cooperative health request;
- no database write is in flight that would be made ambiguous without a
  journal/effect record;
- the launchd label and binary hash are verified;
- restart count is below the crash-loop cap;
- 2-of-3 roles agree on the restart intent (core + warden + anchor).

Custos records the restart, waits for health, and verifies lease/run recovery.
It does not "fix" a crash loop by repeatedly restarting; after the cap it enters
safe mode and escalates. A restart does not clear interrupted runs, unknown
effects, or merge evidence; normal recovery still applies.

### 7.3 Verified backup restore (automatic, quorum-gated)

Custos is **authorized to restore the most recent verified backup
automatically** when all hold:

- the live DB is unopenable/corrupt after a bounded integrity check;
- the latest backup repository verifies (`backup-verify` valid) and its
  manifest signature/anchor generation is acceptable; the backup is not older
  than the anti-rollback anchor;
- the restore target is a fresh directory on the same volume, never an
  in-place overwrite;
- the corrupt original is preserved read-only for diagnosis;
- the daemon is stopped and the restore is offline;
- 2-of-3 roles agree on the restore intent;
- post-restore integrity/schema checks pass before the daemon starts.

The swap is atomic: validate the fresh directory, stop the daemon, rename the
live directory to `data-dir.corrupt.<timestamp>`, rename the fresh directory to
the original path with the same 0700 ownership, then start and verify. If the
swap fails, rename the corrupt original back and enter safe mode. At most one
automatic restore per failure fingerprint; a second failure escalates to safe
mode instead of looping.

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

A sealed instrument, not a dashboard. It is an **independent window owned by
the VerlinPark app** (like the Settings window), not a separate `.app` and not
a panel embedded in the main Stage:

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

### 10.1 A role-separated process constellation

Custos is not one process. It is a small ring of processes with disjoint
authority, so killing one process does not decapitate the guardian:

| Role | Launchd label | Authority | Cannot |
|---|---|---|---|
| **core** | `com.verlintas.custos-core` | probes, scoring, decision state machine, action intents, read-only admin socket | execute signals, restore backup, edit its own binary |
| **warden** | `com.verlintas.custos-warden` | execute approved L3 stops, session pause/cancel, quarantine, daemon restart, backup restore | invent policy, change thresholds, approve gates |
| **anchor** | `com.verlintas.custos-anchor` | self-heal, hash/plist/charter/audit-chain verification, vault restore, peer restart, backup verification, watchdog ring | decide containment, approve actions, read secrets |

Rules:

- No role is a single point of authority. A compromised or killed core leaves
  existing containment in place and the warden/anchor restore it; a killed
  warden leaves decisions but no new stops until restored; a killed anchor
  loses self-heal but core/warden restore it.
- Roles communicate over 0600 Unix domain sockets in a 0700 runtime directory,
  with peer uid/gid/pid checks and a sealed per-install token. There is no TCP
  listener by default.
- Action intents are signed with the charter hash, role id, monotonic epoch,
  target identity, evidence hash, and expiry. Warden rejects stale/duplicate
  intents; anchor audits the chain.
- A single-writer epoch lease prevents split-brain if launchd starts a second
  core. Only the lease holder writes decisions; the other exits.
- The admin socket exposed to the GUI/daemon is read-only. `custos.act`,
  `custos.policy.set`, and pause/resume are not product RPCs.
- The `custosd` role processes are not children of the daemon and not an agent
  tool host. They have no provider key, no model client, no arbitrary shell, and
  no generic "run command" capability.
- The daemon's process supervisor refuses to manage Custos PIDs; agent process
  tools only see registered processes.

### 10.2 Boot autostart and launchd supervision

Three user LaunchAgents (V1, no sudo):

- `com.verlintas.custos-core`;
- `com.verlintas.custos-warden`;
- `com.verlintas.custos-anchor`.

All use `RunAtLoad=true`, `KeepAlive=true`, a bounded `ThrottleInterval`, 0600
stdout/stderr logs, separate process groups, and
`LimitLoadToSessionType=Aqua`. They start at login and are restarted by launchd
after a crash or signal. The anchor starts first and waits for the other
heartbeats; the core and warden retry their sockets until the ring is ready.

### 10.3 Watchdog ring

- Each role writes an atomic heartbeat every 2s:
  `{role, pid, start_time, build_hash, charter_hash, epoch, seq, timestamp,
  state_hash}` in a 0700 directory with 0600 files.
- Each role checks the other two every 2s. A stale heartbeat (> 10s), missing
  pid, start-time mismatch, or hash mismatch triggers a bounded
  `launchctl kickstart -k` of the peer label.
- If kickstart fails, the peer attempts a bounded `bootstrap` from the signed
  plist copy in the vault. A restart request is idempotent and logged.
- If one role is alive and two are dead, the survivor may restart both; this is
  the last-survivor path, not a normal authority.
- If all three die, launchd restarts them. If the labels are booted out, the
  GUI shows `SIGNAL LOST`; this is the documented same-user residual.
- A fourth optional `com.verlintas.custos-anchor-check` timer may run every 60s
  to verify all three labels and re-bootstrap a missing one. It is optional
  because it adds another moving part, not because the ring needs it.

### 10.4 Anti-tamper and self-heal (overwrite of Custos-owned files)

Custos keeps a signed baseline in two places:

1. the app bundle's read-only `Contents/Resources/Custos/` (primary trusted
   copy);
2. a 0500/0400 vault under `~/Library/Application Support/VerlinPark/Custos/
   vault/<build>/` (working copy for fast restore).

The vault holds the role binaries, launchd plists, charter, manifest, default
config, and the last verified audit-chain head. It never holds provider keys,
user data, prompts, or worktree content.

On start and every 30s, each role verifies:

- its own binary hash and code signature against the release manifest;
- peer binary and plist hashes;
- the charter hash;
- data/audit directory ownership and 0600 permissions;
- heartbeat freshness and pid/start-time identity;
- the audit-chain head;
- clock sanity (monotonic vs wall clock);
- unexpected debugger/tracer attachment where macOS exposes it.

On a mismatch:

1. **Quarantine, do not execute.** Rename the tampered file to
   `Custos/quarantine/<timestamp>/...` with 0400 permissions and record its
   SHA-256.
2. **Restore from the vault.** Copy the signed baseline, verify hash and
   signature, then `launchctl kickstart -k` the affected role.
3. **Fallback to the app bundle.** If the vault copy is also bad, restore from
   `Contents/Resources/Custos/`.
4. **Fail closed.** If both baselines are bad, enter `DEGRADED_SELF`: keep
   observing, refuse L3/L4 actions, show `CRITICAL`, and emit a sealed
   `custos.self_heal` audit entry. Never download or self-update.
5. **Audit-chain tamper.** Preserve the tampered file, start a new chain from the
   last verified head, and record an explicit `audit.tamper_detected` gap. Do
   not silently rewrite history; missing entries cannot be recovered.

The overwrite authority is limited to **Custos-owned files** (its binaries,
plists, charter cache, runtime/heartbeat files, vault, audit head). It never
overwrites VerlinPark source, project policy, user documents, worktrees, or
database rows; database recovery is only REC-02.

### 10.5 Quorum and action execution

- **L2 safe repair:** core decides, warden executes, anchor audits.
- **L3 containment:** core intent + warden execution + anchor audit. If core is
  unavailable, the warden may execute a hard-cap stop alone and record it for
  quorum audit; it may not start discretionary containment.
- **L4 recovery (daemon restart, verified restore):** 2-of-3 signed intents
  (core + warden + anchor) with all Article 7 preconditions. If quorum is not
  reached, no action; enter safe mode and notify the audit.
- **Self-heal:** anchor may restore a tampered Custos-owned file and restart a
  peer without quorum, because it is restoring the baseline, not exercising
  product authority. It may not use self-heal to change policy or erase audit.
- Actions are idempotent by `(action, target, evidence_hash, epoch)`; a stale
  epoch or duplicate intent is rejected.
- The epoch lease is monotonic; a restarted core gets a higher epoch, and old
  intents cannot be replayed.

### 10.6 Threat model and honest limits

| Threat | V1 control | Residual |
|---|---|---|
| Agent calls Custos | no tool/RPC/mailbox/prompt surface | none in product surface |
| Agent kills one Custos role | ring detects stale heartbeat and restarts it; authority is split | a same-user shell may signal; brief availability loss |
| Agent kills all three | launchd restarts; anchor/last survivor re-bootstraps | repeated kills cause availability loss, not authority takeover |
| Agent edits Custos binary/plist | hash/signature check; quarantine + vault/app-bundle restore | a same-user writer can tamper the vault too |
| Agent boots out launchd labels | anchor/peer re-bootstrap from signed plist | same-user `launchctl` can boot out labels; no root without Advanced |
| Agent starts a fake role | sealed socket token, peer creds, epoch lease, role separation | same-user can read a 0600 token if they have shell |
| Daemon hangs | ring + launchd independent | none |
| Machine shutdown | accepted final stop | none |
| Root attacker | out of scope | none |

**V1 guarantee:** no first-class VerlinPark/agent surface can stop, pause,
suspend, reconfigure, or impersonate Custos; killing one role does not remove
the others; a killed/stopped role is restored by launchd/ring within seconds;
tampered Custos-owned files are detected and overwritten from a signed baseline
when an intact copy exists.

**V1 non-guarantee:** absolute invulnerability to an arbitrary same-user process
with shell and `launchctl`, or to an attacker who can tamper every baseline copy.
That requires a separate OS user or privileged LaunchDaemon, which is
Advanced-only and not planned for V1. This is recorded, not hidden.

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
- Self-heal entries record the tampered path hash, quarantine location, baseline
  source, restored hash, peer restart, and postcondition. Audit-gap entries
  record `tamper_detected` and the last verified head; they never fabricate
  missing events.
- Quorum entries record the role ids, epochs, intent hashes, and agreement or
  refusal. A refused quorum is an audited non-action, not a silent failure.

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
9. Threshold constants are v0 and may need calibration from shadow-mode data;
   the score model is deliberately conservative and still heuristic.
10. The GUI is an independent window inside the VerlinPark app, not a separate
    app and not a panel embedded in the main Stage.
11. The multi-process ring improves availability and prevents single-process
    decapitation, but it is not a consensus system against root or an attacker
    who can tamper every baseline copy.
12. Self-heal overwrites only Custos-owned files. It cannot recover audit
    entries that were deleted before detection; it records an explicit gap.
13. Process probes are macOS-specific (`proc_pid_rusage`/`proc_pidinfo`); a
    portable fallback is not part of V1.
14. Stack sampling is evidence only, bounded to 1s and top frames; it is not a
    memory dump and never includes user content.

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
| CTN-03 | L3 | stop process group | known process | score ≥ 90 + 2 signal families, or hard cap | group gone; effect unknown | restart if policy | yes |
| CTN-04 | L3 | quarantine worktree | worktree/branch | evidence of unsafe state | non-integrable | health check | yes |
| CTN-05 | L3 | quarantine merge entry | merge queue | conflict/unknown effect | entry held | retry/resolve | yes |
| CTN-06 | L3 | block project dispatch | project | integrity unhealthy | no new sessions | health check | yes |
| CTN-07 | L3 | release stale GUI lease | gui lease | holder dead + TTL | lease free | n/a | no |
| REC-01 | L4 | restart daemon | daemon | crash loop/hang + safe + 2/3 quorum | healthy/new pid | launchd restart | yes |
| REC-02 | L4 | restore verified backup | data dir | DB corrupt + backup valid + anchor + 2/3 quorum | atomic fresh-dir swap | restore corrupt original | yes |
| REC-03 | L4 | enter safe mode | project/daemon | integrity unresolved | read-only/no dispatch | health check | yes |
| REC-04 | L4 | stop all known sessions | sessions | unsafe state + no safe path + 2/3 quorum | all stopped/interrupted | explicit restart | yes |
| SELF-01 | SELF | verify role hashes | core/warden/anchor | startup + 30s tick | verified/hash mismatch | n/a | no |
| SELF-02 | SELF | quarantine tampered file | Custos-owned file | hash/signature mismatch | 0400 copy + hash recorded | restore from baseline | yes |
| SELF-03 | SELF | restore from vault | role binary/plist/charter | baseline hash valid | verified replacement | fallback to app bundle | yes |
| SELF-04 | SELF | restart peer role | core/warden/anchor | stale heartbeat/hash mismatch | new pid/start/hash | launchd retry | yes |
| SELF-05 | SELF | audit-chain tamper record | audit head | chain break detected | explicit gap, preserved file | none (no silent rewrite) | yes |
| SELF-06 | SELF | enter DEGRADED_SELF | whole Custos | both baselines invalid | observe-only, L3/L4 refused | new signed release | yes |

`SELF` is not product authority; it restores Custos's own signed baseline and
cannot change policy, approve gates, or erase audit. Every action also has: max
attempts, cooldown, max duration, evidence hash, and an idempotency key. The
catalog is closed; Custos cannot invent an action.

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
| Processes | identity + `proc_pid_rusage` progress vector + score + optional 1s sample | score < 40 | score 40–79 | score ≥ 90 / hard cap |
| Custos ring | role heartbeat/pid/start/hash, epoch lease | all 3 fresh | 1 stale/restarted | 2+ stale / baseline invalid |
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

Answered in this round: daemon restart (yes, automatic), verified restore (yes,
automatic), multi-process ring (yes), GUI (independent window inside the app),
threshold caution (raised, multi-signal score). Remaining:

1. Are the new score/threshold constants acceptable? Proposed: notice at 60,
   final notice at 80, stop at 90 with two signal families; hard caps 12 GiB
   RSS / 128 children / 8 GiB scratch / 8192 fds; no-progress 10–15 min.
2. Is a three-role ring enough, or should there be a fourth minimal liveness
   watchdog (no authority) to raise the cost of killing two roles?
3. Is the 0500/0400 vault plus app-bundle baseline enough for self-heal, or
   should the baseline be a signed read-only disk image / notarized helper?
4. Do we accept the same-user `kill`/`launchctl` residual for V1, or add an
   optional privileged `custos-guard` LaunchDaemon (one-time admin install)?
5. Should the forensic bundle include normalized full paths and process argv,
   or stay closer to the standard bundle for privacy?
6. Should idle-agent notices go to the project inbox, or be dropped with an
   audit entry if no agent is running?
7. Should a stack sample (1s, top frames) be included in the forensic bundle
   for a suspected stall, or only in the developer-signed audit?
8. What should the user see when Custos quarantines a project? Proposed:
   project name, category, consequence, and a “diagnostics” path only.
9. Should Custos have a menu-bar status dot in addition to the independent
   window, or only the window?
10. Should audit-chain tamper restore the head from anchor memory and mark a
    gap, or only flag the gap without writing a new head? Proposed: preserve
    the tampered file, write a `tamper_detected` gap marker, never fabricate
    missing entries.

---

**Seal:** this draft becomes real only when its hash is embedded in a signed
release. Until then it is a design artifact for review.
