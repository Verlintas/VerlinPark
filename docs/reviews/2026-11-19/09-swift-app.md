# Swift/macOS reliability review — `apps/macos/VerlinPark`

Baseline: `d239dca`  
Scope: `apps/macos/VerlinPark/` (`Core/`, `Features/AppModel.swift`, major views, `Workspace/`, helpers, tests)  
Review type: read-only source review. No `xcodebuild`, no live daemon, no provider calls.

## Executive summary

The app has a sound general split — `AppModel` is `@MainActor`, `CoreClient` is an actor, most loaders use `selectionGeneration`/`isCurrent`, and secrets are kept out of UserDefaults and logs. The serious reliability gaps are concentrated in the connection/event-stream boundary and in destructive/offline flows:

- Reconnect is not a resync. Events can be dropped during `connect`, daemon broadcast lag is not reported to the app, and the app never reloads global pending approvals/questions after a reconnect.
- A stale `AsyncStream` termination can clear the **new** event continuation, leaving the app “connected” while live updates have silently stopped.
- Every RPC has a hard 30-second timeout with no cancellation and no idempotency key. Legitimate long operations (`session.land`, `session.pr`, replay/diagnosis, MCP/provider probes) can complete after the app has reported failure, and a retry can duplicate side effects.
- “Remove stored API key” only deletes the Keychain item; the running daemon keeps its provider key. Conversely, a missing/unreadable Keychain item during reconnect can silently replace the daemon provider with no key.
- Long sessions load up to 100,000 events into memory, merge/sort them on the main actor, trim live arrays at 4,000, and rebuild the full transcript on every change. This produces both memory growth and main-thread stalls.

Top 10 findings are below. Additional lower-severity observations and test gaps follow.

## Findings table

| ID | Severity | Confidence | Area | File:line | Finding |
|---|---|---|---|---|---|
| S1 | High | High | Reconnect / events | `Core/CoreClient.swift:70-85`, `Features/AppModel.swift:950-959`, `:986-1012` | Reconnect drops events, has no backfill/`since_seq`, and does not refresh global approvals/questions. |
| S2 | High | Medium | Concurrency / stream lifecycle | `Core/CoreClient.swift:100-107`, `Features/AppModel.swift:9962-9970` | Old stream termination can clear the new `eventContinuation`; live updates stop while RPCs still work. |
| S3 | High | High | RPC lifecycle | `Core/CoreClient.swift:4627-4641`, `:2667`, `:2687` | Universal 30s timeout; no request cancellation, no operation id, and timeout does not degrade connection state. |
| S4 | High | High | Keychain / provider secrets | `Features/AppModel.swift:5131-5169`, `:5140-5146` | Key removal does not revoke the daemon key; missing Keychain key on reconnect can clear the daemon key. |
| S5 | High | High | Long transcripts / UI performance | `Features/AppModel.swift:9885-9910`, `:9973-9986`, `Features/StageView.swift:1363-1364` | 100k-event load, main-actor merge, 4k live trim, O(n) dedupe, and full transcript rebuild on each change. |
| S6 | Medium | High | Offline behavior | `Features/StageView.swift:744-768`, `Features/AppModel.swift:1631-1643` | Composer stays enabled offline and clears the draft before a request that then fails; text is lost. |
| S7 | High | Medium | Approval security | `Core/SystemNotifier.swift:28-45`, `:65-68`, `:94-98` | Every approval gets a one-tap Approve notification action, including HostState/Destructive/remote/MCP, with no risk or input context. |
| S8 | Medium | High | Daemon discovery | `Core/DaemonController.swift:25-49`, `:59-71` | A 1s health failure deletes a live daemon’s discovery file; the daemon does not rewrite it, and a duplicate daemon is launched. |
| S9 | Medium | Medium | Destructive actions | `Features/DashboardGitCards.swift:29-31`, `Features/ContextCheckpointUI.swift:130-136`, `Features/StudioView.swift:370-408` | Checkpoint prune runs immediately; discard/land/PR confirmations resolve the selected session at confirmation time. |
| S10 | Medium | High | Stale selection responses | `Features/AppModel.swift:3416-3423`, `:5196-5197`, `:9218-9240` | Several async completions are not generation/session-guarded and write old-session data into the current UI. |

## Detailed findings

### S1 — Reconnect drops events and never performs a real resync

**Evidence**

- `CoreClient.connect` calls `disconnect()`, opens the socket, sets `isConnected = true`, then sends `event.subscribe` with empty params and no `since_seq` (`CoreClient.swift:51-85`). `events()` is the only method that installs `eventContinuation` (`CoreClient.swift:100-107`).
- `AppModel.connectIfNeeded` does not start consuming events until after `configureStoredProvider()` and `agentInfo()` (`AppModel.swift:950-959`). Any event delivered between `event.subscribe` and `consumeEvents()` is yielded into a nil continuation and dropped.
- The reconnect `refresh()` reloads projects/sessions and policies, but not the global pending-approval list or pending-question list (`AppModel.swift:986-1012`). Those are only refreshed by the dashboard or by live events (`DashboardView.swift:535-539`, `AppModel.swift:10250-10264`).
- The daemon subscribes the broadcast receiver at auth time and silently continues on `Lagged(skipped)` (`crates/vp-daemon/src/http.rs:317-380`); there is no `resync_required` notification to the client. `event.subscribe` has no history payload (`crates/vp-daemon/src/dispatch/daemon.rs:103`).
- The app never persists a last event sequence: `since_seq` is only used by `event.list` for selected-session history (`CoreClient.swift:1806-1817`).

**Impact**

After a daemon restart, a slow-client lag, or even a slow provider configure during reconnect, the app can permanently miss:

- approval requests, including high-risk ones;
- user questions;
- task/integration/session failure events;
- other sessions’ status changes.

The app continues to look connected because the socket is alive. A user may see a stale pending list, or miss that an agent is blocked on approval, until they reopen Manage Dash or manually refresh.

**Repro**

1. Start a run and open a pending approval in session A.
2. Kill the daemon while the app is reconnecting, or issue an approval from the CLI while `configureStoredProvider()` is slow.
3. Reconnect. The selected session’s transcript is reloaded, but the new approval is absent from the approval center until `loadPendingApprovals()` is invoked by the dashboard.
4. For lag: publish more events than the daemon broadcast buffer while the app is suspended, then resume. The daemon logs `websocket event subscriber lagged`; the app does not detect a gap.

**Fix**

- Add `since_seq`/last-received-sequence persistence and a snapshot/backfill contract, or at minimum reload all globally relevant projections after every reconnect: pending approvals, pending questions, notifications, and the current session’s event tail.
- Install the event consumer **before** `event.subscribe`, or buffer pre-consumer events in `CoreClient` until `events()` attaches.
- Handle daemon lag explicitly. If the daemon cannot add a protocol notification, do a full refetch after reconnect and periodically during long-lived streams.
- Reconcile by sequence rather than assuming the live stream is complete.

**Test**

- A fake transport/daemon fixture that emits an approval between `event.subscribe` and `events()`, then asserts the approval is visible after reconnect.
- A “lag” test that drops events and asserts the app performs the expected resync rather than silently continuing.
- An end-to-end reconnect test with a pending approval in a non-selected session.

### S2 — Stale stream termination can clear the new event continuation

**Evidence**

- `CoreClient.events()` installs a continuation and sets `onTermination` to unconditionally clear `eventContinuation`:

  ```swift
  AsyncStream { continuation in
      self.eventContinuation = continuation
      continuation.onTermination = { @Sendable _ in
          Task { await self.clearEventContinuation() }
      }
  }
  ```

  (`CoreClient.swift:100-107`)

- `disconnect()` finishes and nils the current continuation (`CoreClient.swift:87-98`). `connect()` calls `disconnect()` first (`:52`).
- `AppModel.consumeEvents()` cancels the old `eventTask` and immediately creates a new stream (`AppModel.swift:9962-9970`).
- `clearEventContinuation()` is unconditional (`CoreClient.swift:4725-4727`). It has no stream identity or generation check.

**Impact**

The old stream’s termination callback can run after the new stream has installed its continuation. It then sets `eventContinuation = nil`, so later daemon events are dropped, and the new `for await` loop may never receive a termination signal. The UI still reports `connected`; only RPC-backed refreshes work. This is a silent live-update failure and a plausible cause of “the app is connected but stopped updating”.

**Repro**

A deterministic unit test with an injected socket/stream is needed. At the app level, repeated reconnect cycles (daemon restart or network churn) can expose it; once the continuation is cleared, no subsequent reconnect occurs because the receive loop is still healthy.

**Fix**

- Give each subscription an ID and clear only if the stored continuation still belongs to that ID.
- Prefer a small event-bus actor that owns exactly one stream and a generation counter, rather than storing a raw continuation on the client.
- When the consumer ends unexpectedly, explicitly mark the connection unhealthy and schedule reconnect; do not rely on the receive loop to notice a consumer-side termination.

**Test**

- Connect, disconnect, connect in quick succession, then deliver a synthetic event; assert it reaches the consumer.
- Cancel `eventTask` and replace the stream; assert the new continuation survives the old termination callback.

### S3 — One 30-second timeout for every RPC, with no cancellation or idempotency

**Evidence**

- `CoreClient.request` gives every method the same 30-second timer (`CoreClient.swift:4627-4632`). There is no timeout parameter and no `withTaskCancellationHandler`.
- A caller’s task cancellation does not resume the continuation; the request waits for the timeout or a response. The `Task` used to send is not linked to the caller.
- Long operations use this same path: `session.land` (`CoreClient.swift:2667-2670`), `session.pr` (`:2687-2690`), `session.replay`/diagnosis, `mcp.server.probe`, `agent.probe`, and `remote_*`. `session.land` and `session.pr` are synchronous daemon handlers (`crates/vp-daemon/src/dispatch/session.rs:1043-1060`), not fire-and-forget jobs.
- Mutating calls do not send an idempotency/operation id. For example, `prompt` sends only `session_id` and `content` (`CoreClient.swift:4243-4248`), even though the daemon supports `operation_id` for prompt/defer/steer.
- RPC timeouts do not change `AppModel.connection`; `handle` only changes it when the state is `.connecting` (`AppModel.swift:10449-10453`). A hung daemon that keeps the socket open still displays as connected.

**Impact**

A land/PR/merge or probe that takes longer than 30 seconds is reported as a timeout even if it succeeds. Users naturally retry; non-idempotent operations can then be duplicated (land twice, open multiple PRs, start duplicate work, duplicate fork/checkpoint/gate operations). Because the connection remains `connected`, the app does not trigger its reconnect path for a daemon that has stopped responding to RPCs.

**Repro**

1. Configure a project with `VERLINPARK_INTEGRATION_COMMAND` that sleeps 40 seconds.
2. Land a session. The app shows “The daemon did not respond in time” at 30s while the daemon continues.
3. Retry from the UI. Depending on the operation, a second land/PR/action is attempted.

For provider/MCP probes, use a server whose first response is intentionally delayed past 30s.

**Fix**

- Per-operation timeout classes: short for reads, long/streamed for land/PR/probe/replay; expose progress or a job handle for operations that can exceed the interactive timeout.
- Wire `withTaskCancellationHandler` into `request`, and cancel the send/continuation when the caller task is cancelled.
- Send `operation_id` for prompt/defer/steer and add equivalent idempotency receipts for other mutating UI actions.
- Treat repeated RPC timeouts as a connection-health signal (probe `ping`, mark `.failed`, schedule reconnect).

**Test**

- A stub transport that delays a response past the timeout; assert the caller can cancel promptly.
- A slow `land` test that asserts a long timeout or job-handle path, not a false failure.
- A duplicate-prompt test with the same operation id.

### S4 — “Remove stored key” does not revoke the daemon key; missing Keychain key can clear the daemon key

**Evidence**

- `clearStoredProviderKey` deletes the Keychain item, clears local fields, and writes `provider.hasAPIKey = false`; it does not call the daemon (`AppModel.swift:5140-5146`). The Settings button says “Remove stored key” (`SettingsView.swift:1382-1388`).
- `resolvedProviderAPIKey()` returns `""` when the Keychain read fails or the boolean is false (`AppModel.swift:5131-5138`).
- `configureStoredProvider()` runs on every successful reconnect and sends `apiKey: resolvedProviderAPIKey()` (`AppModel.swift:5148-5169`).
- `CoreClient.configureAgent` omits `api_key` when the string is empty (`CoreClient.swift:4561-4563`). The daemon’s `configure` replaces the provider with `params.api_key` (optionally `None`) on every call (`crates/vp-core/src/agent/service/constructors.rs:125-150`).

**Impact**

- After “Remove stored key”, the daemon still holds the provider key in its in-memory provider object and can continue making calls. The user reasonably expects revocation; the secret remains usable until the daemon is restarted or reconfigured.
- If the UserDefaults flag says a key is stored but the Keychain item is missing/unreadable (keychain denial, reinstall, signing change), the next reconnect silently sends no key and replaces the daemon provider with an unauthorized one. The app then reports provider failures while the UI still says a key is stored.
- Saving a key succeeds only after the daemon is configured; if the Keychain write fails, the daemon has the key but the local state does not, creating the same desync on the next reconnect.

**Repro**

1. Configure a remote provider and confirm a probe succeeds.
2. Click “Remove stored key”.
3. Without restarting the daemon, invoke a provider probe or start a session. The daemon still uses the removed key.
4. Alternatively, delete the Keychain item externally and reconnect; the app sends no key and the daemon’s provider loses authorization.

**Fix**

- Add an explicit daemon RPC to clear provider credentials (or a tri-state `api_key` field: omitted = keep, null = clear, string = replace). Do not overload omission as both “keep” and “clear”.
- On keychain read failure, do not call `agent.configure` with an empty key; surface a configuration error and keep the daemon’s current configuration until the user chooses to clear it.
- Save the Keychain item before/atomically with configuring the daemon, or roll back the daemon config on Keychain failure.
- The removal button should call both the Keychain delete and the daemon clear RPC, then verify with `agent.info`.

**Test**

- Keychain write/read failure injection.
- Remove-key test that asserts the daemon no longer has credentials immediately.
- Reconnect test where the Keychain read fails; assert no destructive daemon reconfiguration.

### S5 — Long sessions can exhaust memory, freeze the main actor, and abruptly lose history

**Evidence**

- `loadEvents` pages `event.list` in 500-event chunks until either the page is short or `loaded.count > 100_000` (`AppModel.swift:9885-9910`). It then merges and sorts the whole array on `AppModel`’s main actor (`mergeEvents`, `:9919-9928`).
- Live appends use an O(n) identity check and trim at 4,000 (`AppModel.swift:9973-9986`). The load path has no equivalent 4,000 trim.
- `SessionChatView` rebuilds the entire transcript from `model.events` on each `events` change (`StageView.swift:1360-1365`). `TranscriptProjection.build` is a full pass over all events (`TranscriptModel.swift:108-114`).
- `Event.payload` is an arbitrary `JSONValue` (up to the daemon’s event/payload caps); retaining up to 100k of them is not a bounded UI model.

**Impact**

- Opening a long-running session can allocate tens or hundreds of MB and block the main thread during merge/sort plus full transcript build.
- The first live event after a large load takes the “live” trim branch and drops history down to 4,000 entries. The user sees the transcript suddenly shrink.
- Every live event after that still runs an O(n) `contains` scan and a full transcript rebuild, producing visible stalls during active runs and tool-heavy turns.

**Repro**

1. Seed a session with >20k events (long tool loop).
2. Open it; observe a large memory jump and main-thread pause while loading.
3. Let one new event arrive; observe the event array/transcript collapse to the 4,000-entry window.

**Fix**

- Use one retention policy for load and live paths (for example 4k–8k events, or a windowed transcript plus on-demand paging).
- Incrementally update the transcript index rather than rebuilding from all events on every change; keep a per-sequence index and append/patch new events.
- Move merge/sort/projection to a background task or a dedicated model, then publish a stable snapshot to the main actor.
- Load older history only on demand, with a visible “load earlier” boundary.

**Test**

- A performance test that opens a 20k-event session and asserts time/memory bounds.
- A retention test that appends a live event after a large load and asserts history does not silently shrink.
- A transcript projection test that verifies incremental updates equal a full rebuild.

### S6 — Offline send loses the user’s prompt

**Evidence**

- The composer’s disabled state checks only empty text, `model.isBusy`, compaction, and session status (`StageView.swift:744-746`); it does not check `model.connection`.
- `send()` clears the draft before awaiting the request (`StageView.swift:763-768`).
- `sendMessage` performs the RPC and only calls `handle(error)` on failure (`AppModel.swift:1631-1643`); the draft is not restored and there is no outbox/queue.

**Impact**

If the daemon is offline, starting/restarting, or the connection is failed, the user can type and press Return while the session is idle. The app clears the text, then shows an error banner. The message is gone and the user must retype it. This is especially likely immediately after app launch or during daemon recovery.

**Repro**

1. Stop the daemon (or force `connection = .failed` in a preview/test harness).
2. Select an idle session, type a prompt, press Return.
3. The draft clears; the send fails; no retry is offered.

**Fix**

- Include `connection == .connected` in `sendDisabled`, or explicitly support an offline outbox.
- Clear the draft only after the prompt RPC succeeds, or restore it on failure.
- Use the existing `operation_id` support when retrying so a timed-out but accepted prompt is not duplicated.

**Test**

- A model/view test with a failed connection: assert the button is disabled or the draft is preserved.
- A failed RPC test that asserts the draft and error are both surfaced.

### S7 — High-risk approvals can be approved from a notification with no risk context

**Evidence**

- `SystemNotifier.requestAuthorization` registers an Approve action with no authentication/foreground requirement (`SystemNotifier.swift:28-37`).
- `notify` sets the approval category for every approval notice (`SystemNotifier.swift:65-68`).
- The notification body is only `tool · agent` (built in `AppModel.swift:10322-10330`); it does not include `approval_risk`, `approval_reason`, the tool input, or the session goal detail.
- Selecting Approve calls `onResolveApproval`, which calls `resolveGlobalApproval` directly (`SystemNotifier.swift:94-98`, `AppModel.swift:1974-1982`).

**Impact**

A user can approve a HostState, Destructive, remote, MCP, or sensitive-GUI request from the lock screen/notification center without seeing the command, path, risk, or reason. The in-app approval card may show those details; the notification path bypasses that context. This weakens the approval boundary the project otherwise treats as security-sensitive.

**Repro**

1. Run an agent action that produces a HostState/Destructive approval while the app is backgrounded.
2. Observe the notification: “Approval needed · goal / tool · agent”.
3. Choose Approve from the notification. The request is resolved without the app foregrounding or showing the risk/input.

**Fix**

- Do not attach Approve actions to HostState/Destructive/remote/MCP/sensitive-GUI notices; require opening the app for those.
- Include a bounded risk label and reason in the notification for lower-risk approvals.
- Or make notification actions always open the approval card and require an explicit second confirmation.
- Consider `UNNotificationAction` authentication requirements if supported by the target macOS deployment.

**Test**

- A notifier unit test asserting the category/actions for a low-risk vs destructive notice.
- An AppModel test asserting the resolve callback is not wired for high-risk notices.

### S8 — Discovery file is deleted on a transient health failure, and token can be sent to a stale port

**Evidence**

- `DaemonController.ensureRunning` health-checks for 1 second (`isHealthy` sets `timeoutInterval = 1`, `DaemonController.swift:59-71`).
- If the health check fails, it immediately removes the discovery file and launches a new daemon (`DaemonController.swift:26-49`). It does not verify that the recorded PID is dead.
- The daemon writes `daemon.json` only at startup (`crates/vp-daemon/src/main.rs:203`) and removes it at shutdown; it does not rewrite it periodically.
- After launch, `ensureRunning` polls for 10 seconds and then reports failure even if the original daemon is alive and healthy but momentarily busy.
- `readDiscovery()` accepts a JSON record without verifying file permissions or process identity (`DaemonController.swift:52-57`).

**Impact**

A transient `/health` timeout (daemon busy, startup, DB lock, scheduling stall) deletes a valid discovery file. The running daemon will not rewrite it, so the CLI and the app’s next reconnect cannot discover it. The app then launches a duplicate daemon, which fails the instance lock, and reports “daemon did not become healthy” while a healthy daemon still runs. When the port is eventually released, a stale discovery file could also point at an unrelated local listener; the app’s auth token would be sent there before auth failure is detected.

**Repro**

1. Start the daemon and confirm `daemon.json` exists.
2. Block `/health` for >1s (or pause the daemon process/port) while keeping the socket process alive.
3. Call `ensureRunning`/relaunch the app. Observe `daemon.json` is deleted, a second daemon launch is attempted, and discovery remains broken until daemon restart.

**Fix**

- Do not delete the discovery file until the recorded process is confirmed dead and no authenticated connection is possible; retry `/health` with a bounded grace period.
- Verify `pid` is alive and belongs to `verlinpadd` before reusing or deleting a record.
- If a launch is attempted, use a lock/claim and preserve the previous file on failure.
- Never send the auth token to a port without verifying the daemon identity (pid/version/auth challenge).

**Test**

- A fake `/health` server that returns 200/503/timeout and a temporary discovery directory.
- Assert the file is preserved on a transient failure and only removed after confirmed death.

### S9 — Destructive checkpoint actions lack confirmation; confirmation dialogs target the current selection

**Evidence**

- The “Prune >30d, keep 5” button calls `pruneCheckpoints()` immediately, with no confirmation (`DashboardGitCards.swift:29-31`). The model calls `pruneCheckpoints(... dryRun: false)` (`AppModel.swift:9606-9622`).
- The context-checkpoint “Prune” button calls `pruneContextCheckpoints(keepLatest: 1)` immediately (`ContextCheckpointUI.swift:130-136`, `AppModel.swift:9455-9475`).
- `PreviewPanelView`’s discard/land/PR confirmation dialogs live at view scope (`StudioView.swift:370-408`) but the actions call model methods that resolve `selectedSessionID` at confirmation time (`discardChanges` `AppModel.swift:5203-5213`, `landSession` `:9213-9222`, `openPullRequest` `:9233-9240`). The view itself is not `.id(session.id)` scoped; only the chat view is (`StageView.swift:73-74`).

**Impact**

- Prune can irreversibly delete multiple checkpoints immediately on a single click; the button is adjacent to Stats and has no undo.
- If the session selection changes while a confirmation is open (notification/deep link, programmatic selection, keyboard-driven selection in a future multi-window build), “Discard changes”, “Land”, or “Open PR” can apply to a different session than the one the dialog described.

**Repro**

1. Open a project with several checkpoints; click “Prune >30d, keep 5”. Multiple checkpoints are removed with no confirmation.
2. Open “Discard uncommitted changes?” for session A; change selection to session B through a notification/command while the dialog is open; confirm. The model resolves `selectedSessionID` as B.

**Fix**

- Add a confirmation dialog showing the candidate count/age for both prune actions, with a dry-run first.
- Capture `sessionID` (and ideally `expectedHead`/diff hash) in the dialog action and pass it to the model; validate it is still current before acting.
- Scope `PreviewPanelView` to the selected session or reset its dialogs on selection changes.

**Test**

- A model test asserting prune requires an explicit confirmation flag.
- A dialog-state test that switching sessions cancels or retargets the pending action.

### S10 — Several async completions write old-session results into the current UI

**Evidence**

- `loadProcesses` captures `sessionID` but never checks it after the RPC; it assigns `supervisedProcesses`, `selectedProcessID`, `processLogs`, and summary state unconditionally (`AppModel.swift:3416-3423`).
- `commitSession` assigns `lastCommit` and then calls `loadDiff()` without checking that the captured session is still selected (`AppModel.swift:5186-5197`).
- `landSession` and `openPullRequest` assign `lastLanding`/`lastPullRequest` and refresh current views without a session check (`AppModel.swift:9213-9240`).
- In contrast, loaders such as `loadDiff` and `loadUsage` do use `isCurrent`/generation checks, showing the intended pattern.

**Impact**

A slow processes/git/land response can land after the user switches sessions and populate the new session’s inspector with old data. `lastLanding`/`lastPullRequest` can show a success banner for session A while session B is selected. The daemon operation itself is applied to the correct captured session, but the UI becomes misleading and may invite a wrong follow-up action.

**Repro**

1. Open a session with a large process list or a slow land operation.
2. Immediately switch to another session.
3. Observe old processes/commit/landing data appear under the new session, or a toast/success card that belongs to the previous session.

**Fix**

- Capture `sessionID` plus `selectionGeneration` at the start of each mutation and guard every assignment / follow-up refresh with `isCurrent`.
- Reset the relevant fields in `selectSession`, which already does this for many but not all states.
- Prefer a per-session result envelope so stale completions are discarded rather than merged into current state.

**Test**

- Delayed RPC tests that switch session before completion and assert no old data is applied.
- A `lastLanding`/`lastCommit` test that asserts these are cleared / ignored on selection change.

## Additional lower-severity observations

1. **Line-number ruler rebuilds the full file index on every keystroke.** `WorkspaceCodeEditor.textDidChange` calls `ruler?.textChanged()` (`WorkspaceCodeEditor.swift:131-137`), and `refreshLineStarts` scans the whole `NSString` (`:224-244`). Editing a large file will become laggy. Fix: update line starts incrementally for the edited range, or only index the visible region plus a boundary cache.

2. **Reduce Motion is not honored by all direct animations.** `VPMotion` only reduces when `VPMotion.resolved`/`delayed` is used (`Motion.swift:37-45`). Direct calls remain in `RootView.swift:33`, `:59-60`, `:131-132`, `:272-286` (error banner, command palette, focus mode, inspector), plus many Settings/Dashboard/Notification views. The project claim that Reduce Motion is honored “throughout” is not accurate. Fix: audit every `.animation(VPMotion…)`/`withAnimation(VPMotion…)` call site and route through a reduce-motion-aware helper.

3. **Empty/error states can be misleading or silent.** `SessionChatView` shows “Talk to the Leader” whenever the transcript is empty and the status is not `running` (`StageView.swift:1295-1300`), including failed/cancelled sessions with no visible error entry. Several loaders swallow errors and keep stale state (`loadTodos` `AppModel.swift:5228`, `loadTeamPlan` `:5244`, `loadMergeQueue` `:9189`, `loadMailbox` `:9046`). Fix: distinguish empty vs failed/error-fetch states and surface a retry.

4. **Keychain access is synchronous on the main actor.** `resolvedProviderAPIKey()` calls `Keychain.read` from `AppModel` (`AppModel.swift:5137`), and `saveProviderSettings` calls `Keychain.save` on the main actor (`:5111`). A Keychain interaction can block the UI. `Keychain` also does not set `kSecAttrAccessible` explicitly. Fix: perform Keychain work off the main actor and define an accessibility class (`kSecAttrAccessibleAfterFirstUnlock` or stricter).

5. **Protocol/version is not checked before use.** `CoreClient.connect` decodes `AuthResult` only for `.ok` and ignores `server.protocol`/`version` (`CoreClient.swift:70-85`); `Discovery.protocol` is also ignored. Connecting to an incompatible/older daemon will produce opaque decode/RPC failures rather than an explicit upgrade error. Fix: validate protocol major and surface a typed error before consuming responses.

6. **Event-driven refresh fan-out is not scoped.** `agent.usage`/`agent.cost` call `loadUsage()` for the selected session on every event, regardless of which session emitted it (`AppModel.swift:10062-10064`). `memory.*`, `intake.*`, and `scaling.decision` events trigger project/global reloads for events from any project (`:10135-10144`, `:10238-10248`). During concurrent sessions this amplifies RPC traffic and UI churn. Fix: filter by selected project/session before scheduling, or coalesce through a single debounced refresh coordinator.

7. **Failed `connect` leaves the socket/resources open.** If auth or `event.subscribe` throws, `connect` exits after setting `isConnected = true` and resuming the socket, without closing it (`CoreClient.swift:61-85`). AppModel catches and retries, so this is usually short-lived, but it can leave a live receive task and a stale `disconnectHandler` path. Fix: `do/catch` around auth/subscribe that calls `disconnect()` and rethrows.

8. **A2A one-time token is retained in `AppModel` despite the model comment.** `A2AModels.A2AStartResult` says the token must never be stored in a view model (`A2AModels.swift:612-616`), but `startA2A` stores it in `a2aOneTimeToken` (`AppModel.swift:164`, `:3847`). It is not rendered, but it remains in memory until stop/clear or app exit. Fix: pass it directly to the one permitted handoff surface or clear it immediately after use.

9. **Session status in the pinned Rail list is stale.** `session.updated` updates `sessions` (`AppModel.swift:10091-10114`) but not `allSessions`; the pinned list reads `allSessions` when non-empty (`RailView.swift:217-220`). Pinned session statuses can lag until `loadAllSessions` runs again. Fix: update both projections or derive one from the other.

10. **`MemoryMonitor` reads and decodes the discovery token every 2 seconds** (`MemoryMonitor.swift:36-61`), including the secret, just to read a PID. It also spawns a detached task per tick and can overlap slow samples. Fix: parse only `pid` or subscribe to daemon events/status; cancel the previous sample.

## Test gaps

The Swift suite is broad on model decoding but weak on the runtime behavior this review targets.

- **No `CoreClient` tests.** `CoreClient.swift` is ~4.7k lines and exposes ~360 RPC methods, but tests only decode protocol fixtures. There is no injected transport/URLSession, no assertion of request method/params, and no coverage of connect/auth/subscribe/disconnect.
- **Zero async XCTest coverage found.** The tests contain no `async func test` and no `XCTestExpectation`/fulfillment-based async tests. There are `@MainActor` tests for pure `AppModel` state and stores, but no task lifecycle, cancellation, or timeout tests.
- **No reconnect/backoff/resync tests.** Nothing covers daemon restart, missed events, broadcast lag, stale discovery, approval/question reload, or the `eventContinuation` race.
- **No failure injection for RPC calls.** The universal timeout, `CoreError.disconnected`, `notConnected`, typed `CoreError.rpc`, malformed event payloads, and partial/failed decodes are untested.
- **Keychain is effectively untested and hard to test.** `Keychain` uses `Bundle.main.bundleIdentifier` with no injectable service/access group; there are no tests for save/read/delete failure, missing items, or the clear-key daemon behavior.
- **Destructive UI flows are not tested.** No tests for prune confirmation/target capture, discard/land/PR session targeting, or notification approval actions. The only `approvalID` test covers string extraction.
- **Accessibility is tested only at the helper level.** `testMotionRespectsReduceMotion` checks `VPMotion.resolved`, but not that every view uses it; no snapshot/UI test asserts reduce-motion behavior or VoiceOver labels/announcements.
- **Window state persistence is only policy-tested.** `WindowFramePolicy` has good geometry tests, but `WindowStateStore` storage, multi-screen change handling, reset, and decode failure are not tested.
- **No long-session/performance tests.** No test opens a large event set, measures transcript build time, or asserts bounded memory/retention.
- **No offline/empty/error state tests for the composer, process panel, workspace file saves, or session switching.**
- **No protocol negotiation test** against a mismatched `server.protocol`/`version`, nor a stale discovery record test.

Recommended minimum additions: an injectable `CoreTransport` for `CoreClient`; deterministic fake-daemon tests for connect/reconnect/lag/cancellation; Keychain dependency injection; AppModel generation-race tests; and a small XCTest performance suite for the event/transcript path.

## Clean areas

- **Secret handling is generally careful.** API keys are entered in `SecureField`s (`SettingsView.swift:1262-1269`, `:1365-1371`, `SetupWizardView.swift:238-245`), stored through the Keychain wrapper, and not written to UserDefaults (only `provider.hasAPIKey` is). No `print`, `NSLog`, `os_log`, or `Logger` calls were found in the Swift sources.
- **No API keys or gate tokens are rendered.** A2A exposes only token presence (`A2AModels.swift:332-345`), gate views deliberately omit tokens (`GateBadge.swift:4-6`), and the one-time A2A token is not bound to a view.
- **Approval rows have reasonable in-app safeguards.** The transcript approval buttons disable while a decision is pending (`TranscriptViews.swift:327-377`), global approval actions disable while the center is busy (`DashboardTeamCards.swift:262-279`), and the approval model preserves risk/reason/coalescing metadata.
- **Destructive actions that delete individual records generally confirm.** Skill delete, model-preset delete, checkpoint row delete, source delete, memory prune, test-cache clear, and GUI timeline prune have confirmation dialogs or alerts. The gap is specifically the checkpoint prune buttons and target capture described in S9.
- **Workspace writes use the daemon’s path jail and SHA-256 conflict token.** `saveWorkspaceFile` passes `expectedSha256` (`AppModel.swift:3157-3175`), the editor is read-only unless the daemon marks it editable, and close-with-unsaved-changes is confirmed (`StudioView.swift:1597-1617`).
- **Window and layout persistence have sensible bounds.** Window frames are clamped to a visible screen and minimum size; rail/inspector widths are clamped; pins are deduplicated and pruned; notices are capped at 100.
- **Model decoding is deliberately additive.** Most models use `decodeIfPresent` with conservative defaults, and the protocol fixture suite is extensive, so older daemon payloads generally decode safely.
- **The actor split itself is clean.** `AppModel` is main-actor isolated, `CoreClient` is an actor, and most selected-session loaders already use `selectionGeneration`/`isCurrent`; S10 is an exception list rather than a wholesale failure.

## Assumptions

- Review is against baseline `d239dca`; no build, test, or live provider run was performed, per the review instructions.
- The project builds with Swift 6 language mode but `SWIFT_STRICT_CONCURRENCY: minimal` (`apps/macos/project.yml:10-11`), so the compiler does not prove sendability or actor-isolation correctness for all closures.
- Rust daemon snippets were read only to validate client/server semantics (event subscription, broadcast lag, `agent.configure` replacement); this is not a Rust review.
- S2, S7, and the dialog-target part of S9 depend on scheduling/user interaction and are marked Medium confidence; the code paths are confirmed, but the exact timing is not reproduced without a test harness.
- The local security boundary is the current-user macOS account and loopback transport; S8’s stale-port token concern assumes same-user/port-reuse exposure, which is outside the normal threat model but relevant to discovery correctness.
