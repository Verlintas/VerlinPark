# ADR-0041: Typed agent mailbox and text artifact store

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

Agents could only exchange information through synchronous `delegate` summaries
and the session chat transcript. There was no durable typed message addressed to
a specific agent, no unread/read state, and no shared artifact reference. Managers
and Workers had to paste long reports into prompts, and the user had no compact
place to inspect handoffs.

## Decision

1. **Session-scoped typed messages.** Add `agent_messages` with sender/receiver
   IDs and display names, `kind`, `body`, optional JSON `payload`, `status`
   (`unread`/`read`), timestamps, and `read_at`. Messages never cross sessions.
2. **Agent tools.** All roles get:
   - `send_message` (`to` agent id or unique name, `kind`, `body`, optional
     `payload`);
   - `read_mailbox` (unread-only by default; returned unread messages are marked
     read);
   - `publish_artifact` (name, kind, MIME type, text content);
   - `list_artifacts`;
   - `read_artifact`.
   These are internal tools, not shell/edit calls, and require no tool approval.
3. **Text artifact store.** Add `artifacts` with session, agent, optional task ID,
   name, kind, MIME type, text content (max 100 KB), and timestamp. Artifacts are
   session-scoped and immutable. There is no binary/path artifact yet.
4. **Events and transactions.** Message send/read and artifact publish update the
   projection and append `mailbox.message_sent`, `mailbox.message_read`, or
   `artifact.published` in one SQLite transaction. The event stream drives the app
   refresh.
5. **APIs.** RPC: `mailbox.list`, `mailbox.read`, `artifact.list`, `artifact.get`.
   CLI: `vp mailbox`, `vp read-message`, `vp artifacts`, `vp artifact`. Preview →
   Mailbox shows messages and artifacts. Agent tools use the same engine APIs.
6. **Bundle support.** Session bundles include `agent_messages` and `artifacts`;
   import restores them with the session. Replay currently does not compare these
   two projections (they are audit/evented but outside the critical repair set).
7. **Limits.** Message body ≤20 KB, kind ≤64 bytes, artifact name ≤200 bytes,
   artifact content ≤100 KB. Delivery is pull-based: a message is durable and
   evented but does not wake an idle agent or create a new run.
8. **Access control.** Any agent in the session may message any other agent in the
   same session; the UI is read/mark-read. There is no cross-session mailbox and no
   per-role send restriction.

## Consequences

- Managers can leave durable handoffs and reports instead of only returning one
  summary; Workers/Verifiers can read them later.
- The user can inspect typed messages and artifacts in Preview → Mailbox, and use
  the CLI headlessly.
- Messages do not schedule work. A recipient sees them on its next turn or when
  explicitly asked to read the mailbox.
- Text artifacts are intentionally small and simple; binary files, retention
  policies, artifact GC, and content-addressed storage remain future work.
- Mailbox/artifact projections are exported and imported but not covered by
  `session.replay --repair` yet.
- The event stream is the audit trail; message bodies are stored in SQLite and
  mirrored events, so secrets must not be sent through the mailbox.

## Alternatives considered

- **Chat-only handoffs:** loses addressing, unread state, and payload structure.
- **Filesystem-only artifacts:** needs path jail/retention and does not travel in
  session bundles; text-in-DB is the smallest useful step.
- **Async delivery that wakes agents:** requires a scheduler/checkpoint design;
  pull-based messages are safe now.
- **Cross-session mailbox:** increases privacy/authorization surface; session scope
  is intentional.
