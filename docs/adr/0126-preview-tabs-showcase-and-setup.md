# ADR 0126: Preview as a result board, file tabs, and first-run setup

- Status: Accepted
- Date: 2026-11-09
- Area: UI / onboarding

## Context

Preview had ten categories in a single segmented row, a flat file list, and
only one open file. It was a dense inspector rather than the “agent result
whiteboard” the product needs. Manage Dash exposed nine equally prominent
sections. Providers required the user to understand base URL, context window,
role overrides, team capacity, and model presets before the first useful run.

## Decision

1. **Preview has a primary result board.** `Showcase` is the default category
   and renders published artifacts as cards (image/text/binary previews,
   author, size, open action). Agents already publish artifacts; no new agent
   protocol is required.
2. **Primary vs secondary categories.** Showcase, Changes, Files, and Plan are
   primary. History, Queue, Mailbox, Cost, Console, Terminal, and Trace live in
   a More menu. This reduces horizontal crowding.
3. **Files are a recursive tree with tabs.** The flat file list becomes a
   folder tree. Opening files creates browser-like tabs with independent
   drafts, dirty indicators, save/reload, and close confirmation.
4. **Manage Dash starts essential.** Overview, Inbox, Team & Tasks, and Usage
   are visible by default; Git, Checkpoints, Storage, Policies, and Diagnostics
   are behind an “Advanced sections” switch.
5. **Provider setup is preset-first.** Provider presets fill base URL, model,
   and context window. Only the API key is required for remote providers.
   Context/model heuristics fill common families. Role overrides, capacity,
   base URL, context override, and session defaults move behind “Advanced”.
6. **A first-run wizard is the default path.** It connects one model, adds one
   project, and finishes. It is skippable and can be reopened from Settings →
   General. The app remains usable without configuring budgets, team sizes,
   policies, or model presets.
7. **Collab header and zoom are legible.** Graph/Ops uses a full-width
   segmented control so labels never truncate. Zoom starts at 100%, steps by
   5%, snaps to 5% increments, and a click on the percentage resets to 100%.
8. **Collab Ops is task-focused.** Overview, Board, Inbox, and History replace
   one long stack; the command bar keeps Run next, Preview, Coordinate, and a
   More menu.

## Consequences

- Preview becomes a result surface first and an inspector second.
- Multiple files can stay open while other categories are inspected.
- First-run configuration is one provider and one folder; advanced controls
  remain available but do not block the first run.
- The setup wizard writes only provider fields and the project; it does not
  install external tools or configure skills automatically.
