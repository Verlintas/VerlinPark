# ADR-0115: Warm graphite atelier UI

- **Status:** Accepted
- **Date:** 2026-11-08

## Context

The motion slice (ADR-0114) made interaction coherent, but the visual language
was still a mix of translucent materials, an aurora backdrop, a global green
accent, duplicated session headers, and a rail that only listed projects. The
stage, Studio, Dashboard, and Settings each looked like separate products. The
rail wasted its widest surface, sessions were buried behind a project click, and
the inspector could not be dismissed.

The product needs an independent identity: a calm, native macOS workbench for
long agent runs, not a generic dark dashboard and not a visual copy of any
reference app. Visual references were used only for product behavior; no layout,
palette, or assets were copied.

## Decision

1. **One visual system: warm graphite atelier.** `VPTheme` owns the canvas,
   rail, flat surface layers, hairlines, warm coral accent, semantic state
   colors, per-role colors, and deterministic project tints. The previous
   drifting aurora is replaced by a static ambient wash; color is reserved for
   identity, state, and the primary action.
2. **Typography has three registers.**
   - Rounded sans for interface labels and session titles.
   - Editorial serif for large moments (welcome, resident surface, quiet stage).
   - Monospaced micro-labels for operational metadata, budgets, and event data.
3. **The rail is a navigator.** Projects expand in place to show recent sessions,
   with active counts, pulsing running state, relative age, and a show-more row.
   The selected project auto-expands, the first project opens when none is
   selected, and `⌘K` remains the fast path. The rail also owns the resident
   entry, new-session action, Dashboard/Inbox/Settings actions, and runtime
   footer.
4. **The stage has one header.** Session goal, branch, token usage, context, and
   actions live in the stage header; `SessionChatView` no longer repeats them.
   Transcript content is capped at a readable width and centered. User,
   assistant, tool, approval, and question cards share flat surfaces and semantic
   accents. Tool output expands on demand.
5. **The composer is the primary object.** It uses a raised flat card, coral
   focus ring, a circular accent send control, and a metadata row for model,
   status, send keys, and focus shortcut. The model is never hidden or inferred.
6. **The inspector is optional and persisted.** `⌥⌘I` or the top chrome toggles
   Studio; the choice is stored in `verlinpark.inspector.visible`. Studio keeps
   its two modes but uses the new header, card language, role colors, and a
   matching external-resident panel.
7. **Settings and Dashboard adopt the same shell.** Settings remains a
   seven-section sidebar; Dashboard remains a nine-section sidebar. Both use the
   rail surface, coral selection treatment, flat cards, and accent tint for
   system controls.
8. **Sheets and overlays are first-class.** New Session is a brief-first sheet
   with admission priority and a three-field budget grid. Search, notifications,
   keyboard shortcuts, fork, and command palette use the same surfaces,
   typography, and motion. The palette remains keyboard-first.
9. **The resident surface is explicitly external.** A restrained orbital mark,
   editorial headline, bridge contract card, and future-capability grid make it
   clear that Aviiya lives in NovaBAIC. No local persona or chat runtime is
   implied.
10. **Motion is integrated, not layered on.** `vpStage` and `vpInspector`
    transitions join the existing `VPMotion` vocabulary. Expand/collapse,
    selection, focus rings, tool expansion, and header content swaps animate
    through shared springs; continuous effects stop under Reduce Motion.

## Consequences

- The app has one recognizable surface and accent language instead of
  per-screen experiments.
- The rail carries real navigation weight and surfaces sessions without an
  extra project click.
- Dark mode is the designed appearance; system menus, pickers, alerts, and
  sheets retain native behavior and appearance.
- UI code grows in the shell and shared design system, while several legacy
  glass helpers remain unused or compatibility-only.
- Visual QA is still manual; screenshots and interaction checks are not part of
  CI.
- `StudioView.swift` and `StageView.swift` remain large and should be split in a
  follow-up, but this slice intentionally avoids a risky file-wide rewrite.

## Alternatives considered

- **Keep the glass/aurora system and only change colors:** does not fix the
  hierarchy or duplicate headers.
- **Copy a reference app's visual language:** rejected; references are for
  functional analysis only.
- **Permanent three-column layout:** wastes horizontal space and makes the stage
  feel cramped; the inspector is now optional.
- **Light theme first:** the product is used for long, dim-room agent runs;
  dark-first is the deliberate default.
- **Replace all system controls with custom chrome:** loses native keyboard,
  menu, and accessibility behavior.
- **One global accent for every state:** destroys the distinction between
  primary action, success, warning, danger, and informational states.
