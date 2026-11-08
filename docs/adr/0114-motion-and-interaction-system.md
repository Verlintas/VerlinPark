# ADR-0114: Motion and interaction system

- **Status:** Accepted
- **Date:** 2026-11-08

## Context

The app had almost no shared motion vocabulary. A handful of views used ad-hoc
`easeOut` timing, while the rail, studio tabs, dashboard sections, chat entries,
settings forms, and sheets switched instantly. Hover and press feedback were
inconsistent, pending states used static dots, and Settings was still a stock
`TabView`. Reduce Motion was not handled systematically. The UI read as a
functional prototype rather than a finished native workbench.

Motion is also an accessibility and performance surface: continuous effects,
per-row hover state, and large blurred backdrops must be centralized, bounded,
and disabled when the user asks for reduced motion.

## Decision

1. **One motion vocabulary.** `VPMotion` owns duration tokens, spring curves,
   repeat animations, stagger math, and reduce-motion resolution. Views use
   `vpAnimation`, `vpAppear`, `vpRise`, `vpContent`, and `vpScaleFade` rather
   than local timing constants.
2. **Reusable interaction primitives.**
   - `VPPressableButtonStyle` for subtle press scale/opacity.
   - `VPPrimaryButtonStyle` for focus/primary actions with control-size aware
     padding, gradient fill, hover glow, and press scale.
   - `VPGlassIconButtonStyle` for circular toolbar actions.
   - `vpHoverLift`, `vpHoverHighlight`, `vpGlowBorder`, and `vpFocusRing` for
     cards, rows, pending states, and composers.
3. **Status and feedback primitives.**
   - `VPStatusDot` with a bounded pulse ring.
   - `VPThinkingIndicator` with staggered dots.
   - `VPStreamingCaret` for streamed assistant text.
   - `VPAnimatedNumberText` and `VPCountBadge` for numeric transitions.
   - `VPGradientProgress` for context/progress bars.
   - `VPSkeleton`/`VPSkeletonRows` and `VPShimmer` for loading states.
   - `VPFloatingSymbol` for empty states.
4. **Navigation motion.**
   - Rail selection and project rows share a matched-geometry highlight and
     hover treatment.
   - Studio and Preview use `VPSegmentedControl` with a matched-geometry
     indicator; Preview modes use a horizontally scrollable chip control.
   - Dashboard sections animate content and sidebar highlight.
   - Settings is now a sidebar shell with seven sections, persisted selection,
     `⌘1–⌘7` shortcuts, hover/selection motion, and content transitions.
5. **Command palette.** `⌘K` opens a keyboard-first palette for projects,
   sessions, and actions (new session, Manage Dash, search, settings, focus
   mode, add project). It has live filtering, up/down selection, return/escape,
   hover, and an animated backdrop.
6. **Focus mode.** `⇧⌘F` hides the rail and studio with spring transitions,
   leaving a wide stage and an exit control. The command palette can toggle it.
7. **Top chrome.** A slim draggable chrome bar exposes connection state,
   project/session breadcrumb, memory, command palette, and focus mode without
   opening a menu. It respects the traffic-light safe area.
8. **Transient feedback.** `VPToastStack` shows at most four auto-dismissing
   success/info/warning/error toasts with a progress line. Session creation,
   provider connection, ACP/MCP changes, landing, PR creation, fork, and
   checkpoint restore/delete emit toasts in addition to their existing status
   text.
9. **Comprehensive application.**
   - Stage header, composer focus glow, send bounce, empty state, chat entry
     rise, streaming caret, thinking dots, queued/paused status, tool cards,
     approval/question pulse, and welcome onboarding cards.
   - Collab org graph stagger, animated connectors, card hover, plan/queue rows,
     history/file selection, console/terminal status.
   - Dashboard metrics, pending actions, attention rows, projects, runtime,
     recent activity, and all card roots.
   - Settings, brand, model presets, MCP/ACP forms, notification center, search,
     new-session sheet, and MCP probe/editing states.
10. **Reduce Motion and performance.** Motion modifiers resolve through
    `VPMotion.resolved`, continuous pulses/shimmer/aurora stop under Reduce
    Motion, toast progress becomes static, and hover effects are limited to
    cards/rows rather than every text run. The aurora uses three blurred
    circles, not per-frame canvas work.

## Consequences

- Motion is consistent and centrally adjustable; a future motion preference can
  be added without touching every view.
- Reduce Motion is honored across custom effects, not just system transitions.
- The app has a recognizable interaction identity: matched-geometry navigation,
  soft hover lift, gradient focus rings, pulsing pending states, and restrained
  spring transitions.
- Settings and command palette are now product surfaces rather than stock
  controls, which increases UI code and test surface.
- Some system controls (menus, pickers, alerts, sheets) retain native motion and
  appearance; the custom system intentionally complements rather than replaces
  platform behavior.
- Toasts are transient UI state, not persisted notifications; the Rail
  notification center remains the durable inbox.

## Alternatives considered

- **Keep ad-hoc animations:** impossible to keep consistent and easy to violate
  Reduce Motion.
- **Animate everything:** distracting, expensive, and hostile to accessibility.
- **Third-party animation library:** unnecessary for the primitive set and adds
  dependency risk.
- **Replace all system controls:** loses native macOS behavior and keyboard
  semantics.
- **Use a single global animation for all state:** removes hierarchy and makes
  small changes feel slow.
