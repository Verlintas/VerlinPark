# ADR-0118: Workspace shell, command ranking, and notification inbox

- **Status:** Accepted
- **Date:** 2026-11-08

## Context

The black-crystal shell exposed several reliability gaps at the edges:

- Focus mode used a fixed 30×30 icon button for an “Exit Focus” label, so the
  label clipped and sat on top of the stage.
- Rail and inspector widths were fixed. Narrow windows could leave the stage
  unreadably small, and the layout did not remember user adjustments.
- The app restored no window frame, so every launch moved the workbench back to
  its default geometry.
- Command palette filtering was substring-only despite a large action/session
  index; it did not learn recents and did not show why a result matched.
- The notification center was a flat list with no filters, search, grouping, or
  safe “clear read” operation.

These are presentation-state problems with deterministic rules, so they belong
in small pure models rather than additional view conditionals.

## Decision

1. **One sizing model.** `WorkspaceLayout` resolves rail, inspector, stage, and
   divider widths. It clamps the rail to 240–320 pt, the inspector to 320–560 pt,
   keeps the stage at least 520 pt, and hides the inspector when the window
   cannot satisfy those constraints. The rules are unit-tested.
2. **Resizable columns.** `VPSplitHandle` provides a 7 pt draggable divider with
   a hover highlight, accessibility adjustable action, double-click reset, and
   `⌥⌘0` reset of both rail and inspector widths. Widths persist in
   `@AppStorage`.
3. **Window geometry persistence.** `WindowStateStore` stores the main window
   frame as JSON, restores it once per launch, and sanitizes it against current
   screen `visibleFrame`s. Off-screen frames are centered; undersized frames are
   raised to the 1120×720 minimum. Move/resize notifications are debounced
   before writing.
4. **Focus mode lives in the chrome.** The separate “Exit Focus” overlay is
   removed. In focus mode the top chrome replaces the focus icon with a labeled
   capsule, hide controls that would target hidden panels, and keeps the
   `⇧⌘F` path unchanged.
5. **One command path.** The app menu, command palette, and keyboard shortcuts
   all dispatch through `WorkspaceCommandRouter`; the old invisible shortcut
   buttons are removed. `WorkspaceCommand` names the actions that need RootView
   state, while direct app/model actions stay with their owners.
6. **Ranked command palette.** `CommandPaletteRanker` scores exact, prefix,
   word-prefix, substring, and subsequence matches, rewards recent commands, and
   returns character ranges for title/subtitle emphasis. `VPHighlightedText`
   renders those ranges. Scope chips filter actions, projects, and sessions; the
   palette keeps English and localized text searchable.
7. **Layout presets.** `WorkspaceLayoutStore` persists the widths and inspector
   visibility, exposes Balanced / Reading / Team presets, and lets Settings
   adjust both widths with sliders.
8. **Inbox model.** `NotificationInboxModel` owns filter, query, project,
   sorting, counts, and day grouping. The view adds filter chips, search, project
   scoping, “mark all read”, “clear read”, per-notice open/mark/remove actions,
   and distinct empty states for an empty inbox versus an empty filter.
9. **Search scopes and pinning.** The session search surface filters messages
   versus raw events, reports per-source counts, and highlights query matches.
   Pinned sessions/projects and a persisted rail sort keep frequent work near the
   top without changing selection semantics.
10. **Accessibility fallback.** Crystal surfaces, the command palette backdrop,
    and toasts use solid tones when Reduce Transparency is enabled.
11. **No hidden state or magic priorities.** Recents, widths, filter selection,
    pinning, rail sort, and window geometry are explicit persisted values. The
    ranking, grouping, and sort rules are pure functions with deterministic
    fallbacks.

## Consequences

- The stage remains readable at the 1120 pt minimum window width.
- User layout choices survive launch; invalid saved geometry cannot strand the
  window off-screen.
- Command discovery is faster and explainable; recent actions stay near the top.
- The notification center is usable across projects without reading every row.
- New UI strings participate in the existing localization table.
- The shell adds a small amount of persisted presentation state that must be
  cleared if the user wants a pristine layout; `⌥⌘0` covers the workspace part.
- Pinned items and rail sort are presentation metadata only; they never alter
  daemon session state or scheduling priority.
- Search filtering operates on the daemon's existing hit list, so it cannot
  change the result cap or FTS ranking.

## Alternatives considered

- **Keep fixed widths:** simple, but violates the readable-stage invariant.
- **Auto-hide panels based only on width:** surprising and stateful; a draggable
  divider with a hard minimum is more predictable.
- **Restore frames without validation:** can put the window entirely off-screen
  after a monitor change.
- **Substring-only palette:** leaves too many near-matches and cannot reward
  recents.
- **Filter the notification list in the view:** duplicates count/group logic and
  makes tests brittle.
- **A separate focus-mode window:** breaks continuity and window state.
