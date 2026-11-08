# ADR-0116: Black crystal glass refinement

- **Status:** Accepted
- **Date:** 2026-11-08
- **Refines:** [0115](0115-warm-graphite-atelier-ui.md)

## Context

ADR-0115 established the atelier layout and a warm graphite palette. The
structure worked, but the combination of warm coral, editorial serif headlines,
sparkle icons, a rotating orbital mark, and soft glows read as fantasy rather
than as a professional agent workbench. The desired direction is “black crystal,
semi-transparent”: dark obsidian planes, visible glass depth, cold specular
edges, and a technical rather than magical atmosphere.

## Decision

1. **Obsidian palette with an ice accent.** The canvas and rail move to neutral
   blue-black; the accent becomes cold ice (`#A9C9D6`) with a deep steel fill
   (`#4C7285`) for primary buttons. Role and state colors stay muted. The coral
   accent is removed.
2. **Semi-transparent planes.** `VPCrystalSurface` combines an in-window
   material with a dark tint. The Rail, Stage, Studio, and top chrome opt into
   it over the static `VPCrystalBackdrop`, so the workspace has visible depth
   instead of flat opaque columns. Cards use a translucent surface token.
3. **Crystal edges.** Surfaces use a specular `edgeGradient` border (bright at
   the top-left, dimmer at the bottom-right) instead of uniform hairlines.
   Circular toolbar buttons become rounded glass squares; fields keep a focused
   ice ring.
4. **No fantasy cues.**
   - No aurora, drifting orbs, or sparkles in the shell.
   - Resident identity uses `cube.transparent` and a static faceted glass slab.
   - Large moments use the UI sans, not a serif.
   - Hero motion is static under Reduce Motion; the crystal does not orbit.
5. **Sheets stay readable.** Settings, Dashboard, New Session, Search,
   Notifications, Shortcuts, and Fork use the crystal backdrop, but remain
   opaque windows with translucent cards. They do not blur the desktop.
6. **Motion remains from ADR-0114.** Expand/collapse, stage swaps, inspector
   slide, hover lift, status pulse, streaming caret, and tool expansion keep the
   existing vocabulary. The change is visual, not an animation rewrite.
7. **Contrast is part of the system.** Ice is used for text and borders; the
   deep steel fill is used when white button text is required. Status colors
   retain their semantic hue.

## Consequences

- The product reads as a technical black-glass workbench rather than a magical
  dashboard.
- The shell now depends on material rendering and specular edges; low-power or
  Reduce Transparency environments still fall back to a near-black surface.
- The palette is cooler and less warm; long-session comfort is preserved by
  keeping saturation low.
- Hand-tuned visual QA remains required because contrast and material behavior
  are not covered by unit tests.

## Alternatives considered

- **Keep coral and only remove sparkles:** still reads warm/fantasy because the
  accent and serif carry most of the tone.
- **Fully opaque black:** loses the “crystal, semi-transparent” request.
- **Use bright glass everywhere:** unreadable and expensive.
- **Animate the crystal:** turns a static brand mark back into a magical object.
- **Copy an existing glass design system:** rejected; the layout and tokens are
  independent to this product.
