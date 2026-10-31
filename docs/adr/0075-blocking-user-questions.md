# ADR-0075: Blocking user questions

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

Agents could ask each other questions through the mailbox, but there was no way
to stop a run and ask the human for a decision. Approvals cover tool permission,
not product/architecture choices such as “migrate or keep the old schema?”.

## Decision

1. **First-class question state.** `Question` records the session, asking agent,
   question text, optional options, status (`pending`/`answered`/`cancelled`/
   `expired`), answer, and timestamps.
2. **Blocking tool.** `ask_user` is available to Leader/Manager/Worker/Labour,
   not Verifier. It creates `question.requested`, sets the session/agent to
   `waiting_input`, and waits for one answer.
3. **Answer surface.** `question.answer` / `vp answer`, Chat question cards with
   option buttons or free text, `question.list` / `vp questions`, and a
   Manage Dash cross-session inbox.
4. **Resume semantics.** The answer becomes the tool result
   (`User answered: …`); the same run continues. `VERLINPARK_QUESTION_TIMEOUT_SECS`
   (default 1800, range 60–86400) marks the question expired and returns an error
   tool result instead of hanging forever.
5. **Cancellation.** Cancel marks the question cancelled; daemon restart expires
   pending questions and fails `waiting_input` sessions (same recovery rule as
   active runs).
6. **Event sourcing.** Question projections are rebuilt from
   `question.requested/answered/cancelled/expired` events, compared by replay,
   repaired by `session.repair`, and included in bundles.
7. **No system notification dependency.** The existing notification center sees
   `question.requested`; the run does not require macOS notification permission.

## Consequences

- A run can safely stop on a real ambiguity instead of guessing.
- While waiting, the session keeps its global admission slot; a question is not
  a way to free capacity.
- Only one blocking answer per question; multi-question interviews require
  sequential calls.
- The timeout is configurable through an environment variable.
- Questions are visible in Chat, the notification center, and a Manage Dash
  cross-session inbox; the inbox is pull-based, not a task queue.

## Alternatives considered

- **Reuse approvals:** conflates permission with product decisions and has a bool
  answer.
- **Mailbox message:** non-blocking; the agent would continue and likely guess.
- **Free-form `ask_user` without options:** supported, but options make common
  choices one click.
