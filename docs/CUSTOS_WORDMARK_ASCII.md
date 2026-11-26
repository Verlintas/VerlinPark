# ◇ CUSTOS — ASCII wordmark studies

Status: **design draft, no implementation**  
Date: 2026-11-26  
Companion: `docs/CUSTOS_CHARTER_V0.md`, `docs/CUSTOS_PLAN.md`

This file is the **CUSTOS name itself**, not the window layout. The earlier
`docs/CUSTOS_GUI_ASCII.md` contains layout studies; this one is the wordmark /
sigil lockup.

## Rules

- Fixed-width monospace; no proportional fallback, no ligatures.
- The static frame must be recognizable without animation.
- Animation is character-based: scanline rows, node travel, glitch jitter, or
  core pulse. 8–12 fps, pulse ≥ 1.2s, scan ≥ 4s, no flashing > 3 Hz.
- State color changes the ink/glow, not the letterforms. `HEALTHY` ice cyan,
  `WATCH` amber, `CRITICAL` red, `QUARANTINED` near-black + red rim, `SIGNAL
  LOST` dim grey.
- The wordmark never contains an essential status that is absent from text.
- Reduce Motion freezes the static frame.
- No `sample`, stack, path, or user content in the art itself.

---

## 1. SEALED SLAB

Concept: heavy slab letters with cut corners; the most "hardcore" static
option. Reads well at small sizes and in monochrome.

```
        ◇
▄▄▄▄▄▄  █    █  ▄▄▄▄▄▄  ▄▄▄▄▄▄  ▄▄▄▄▄▄  ▄▄▄▄▄▄
█       █    █  █         ██    █    █  █
█       █    █  ▀▀▀▀▀▄    ██    █    █  ▀▀▀▀▀▄
█       █    █       █    ██    █    █       █
█       █    █  ▄▄▄▄▄▀    ██    █    █  ▄▄▄▄▄▀
▀▀▀▀▀▀  ▀▀▀▀▀▀  ▀▀▀▀▀▀    ██    ▀▀▀▀▀▀  ▀▀▀▀▀▀
```

Scanline frame (one row becomes `▓`, then moves up/down):

```
        ◇
▄▄▄▄▄▄  █    █  ▄▄▄▄▄▄  ▄▄▄▄▄▄  ▄▄▄▄▄▄  ▄▄▄▄▄▄
█       █    █  █         ██    █    █  █
█       █    █  ▀▀▀▀▀▄    ██    █    █  ▀▀▀▀▀▄
▓▓▓▓▓▓  ▓▓▓▓▓▓  ▓▓▓▓▓▓  ▓▓▓▓▓▓  ▓▓▓▓▓▓  ▓▓▓▓▓▓
█       █    █  ▄▄▄▄▄▀    ██    █    █  ▄▄▄▄▄▀
▀▀▀▀▀▀  ▀▀▀▀▀▀  ▀▀▀▀▀▀    ██    ▀▀▀▀▀▀  ▀▀▀▀▀▀
```

Core pulse: replace the top `◇` with `◆` for one frame, then back. The
letterforms do not move.

Best for: main header, boot frame, monochrome fallback.

---

## 2. THIN CIRCUIT

Concept: box-drawing letters, almost a circuit trace. Lighter and more
"instrument" than SEALED SLAB; the node travel is the animation.

```
                    ◇
   ┌────  │   │  ┌────  ─────  ┌───┐  ┌────
   │      │   │  │        │    │   │  │
   │      │   │  └───┐    │    │   │  └───┐
   │      │   │      │    │    │   │      │
   └────  └───┘  ────┘    │    └───┘  ────┘
```

Node travel (a `◇` moves along the top rule; three frames):

```
   ┌◇───  │   │  ┌────  ─────  ┌───┐  ┌────
   ┌─◇──  │   │  ┌────  ─────  ┌───┐  ┌────
   ┌──◇─  │   │  ┌────  ─────  ┌───┐  ┌────
```

Pulse: the top `◇` becomes `◆` for one frame. The traces stay still.

Best for: a lighter main header or a settings-adjacent panel. Less brutal than
SEALED SLAB, more "sealed system".

---

## 3. TERMINAL

Concept: pure ASCII `#`, no Unicode box drawing. The most terminal-native and
the best glitch animation. It also degrades safely if a font lacks box glyphs.

```
              ◇
  ####  #   #   ####  #####   ###    ####
 #      #   #  #        #    #   #  #
 #      #   #   ###     #    #   #   ###
 #      #   #      #    #    #   #      #
  ####   ###   ####     #     ###   ####
```

Glitch frame A — one-column jitter:

```
               ◇
   ####  #   #   ####  #####   ###    ####
  #      #   #  #        #    #   #  #
  #      #   #   ###     #    #   #   ###
  #      #   #      #    #    #   #      #
   ####   ###   ####     #     ###   ####
```

Glitch frame B — one scan row becomes `▒`, then restores:

```
              ◇
  ####  #   #   ####  #####   ###    ####
 #      #   #  #        #    #   #  #
 ▒▒▒▒   ▒   ▒   ▒▒▒     ▒    ▒   ▒   ▒▒▒
 #      #   #      #    #    #   #      #
  ####   ###   ####     #     ###   ####
```

Best for: menu-bar popover, low-color mode, or a deliberately raw boot frame.

---

## 4. ANSI SHADOW

Concept: gothic block letters with a hard diagonal cut. The most "sealed
artifact"; reads like an engraved plate. Use a slow scanline overlay rather than
changing the letterforms.

```
        ◇
  ██████╗██╗   ██╗███████╗████████╗ ██████╗ ███████╗
 ██╔════╝██║   ██║██╔════╝╚══██╔══╝██╔═══██╗██╔════╝
 ██║     ██║   ██║███████╗   ██║   ██║   ██║███████╗
 ██║     ██║   ██║╚════██║   ██║   ██║   ██║╚════██║
 ╚██████╗╚██████╔╝███████║   ██║   ╚██████╔╝███████║
  ╚═════╝ ╚═════╝ ╚══════╝   ╚═╝    ╚═════╝ ╚══════╝
```

Scanline overlay (moves across the plate, never replaces a letter):

```
  ────────────────────────────────────────────────────────
```

Core pulse: the top `◇` alternates `◇`/`◆`; the plate stays fixed.

Best for: boot splash, About/Custos identity, static "sealed release" frame.
Slightly less readable at small sizes than SEALED SLAB.

---

## 5. MONOLITH LOCKUP

Concept: SEALED SLAB inside a thin frame, with a tagline. The strongest
"this is a separate sealed program" lockup; best as the main window header or
boot frame.

```
        ◇
╔══════════════════════════════════════════════════╗
║                                                  ║
║  ▄▄▄▄▄▄  █    █  ▄▄▄▄▄▄  ▄▄▄▄▄▄  ▄▄▄▄▄▄  ▄▄▄▄▄▄  ║
║  █       █    █  █         ██    █    █  █       ║
║  █       █    █  ▀▀▀▀▀▄    ██    █    █  ▀▀▀▀▀▄  ║
║  █       █    █       █    ██    █    █       █  ║
║  █       █    █  ▄▄▄▄▄▀    ██    █    █  ▄▄▄▄▄▀  ║
║  ▀▀▀▀▀▀  ▀▀▀▀▀▀  ▀▀▀▀▀▀    ██    ▀▀▀▀▀▀  ▀▀▀▀▀▀  ║
║                                                  ║
╚══════════════════════════════════════════════════╝
        ◇
   S E A L E D   G U A R D I A N
```

Orbit frame: the top/bottom `◇` moves to the left/right edge of the frame for
one cycle; the inner wordmark does not move:

```
◇       ╔══════════════════════════════════════════════╗
        ║  ...                                         ║
        ╚══════════════════════════════════════════════╝
                                                       ◇
```

Best for: primary identity, boot, and the main window header. The frame is
thin enough to stay quiet; the slab is heavy enough to feel sealed.

---

## Comparison

| Variant | Static | Motion | Readability | Best use |
|---|---|---|---|---|
| SEALED SLAB | heavy slab | scan row + core pulse | strongest | main header / monochrome |
| THIN CIRCUIT | box traces | node travel | high | lighter header |
| TERMINAL | pure `#` | jitter + `▒` scan | high | menu popover / raw mode |
| ANSI SHADOW | engraved gothic | scanline + core pulse | medium | boot / identity plate |
| MONOLITH LOCKUP | slab in frame | orbit + core pulse | strongest | primary lockup / boot |

## Recommendation

- **Primary:** MONOLITH LOCKUP. It reads as a separate sealed program, not a
  VerlinPark page, and the frame gives the ASCII art a physical boundary.
- **Compact header:** SEALED SLAB without the outer frame, when vertical space
  is tight.
- **Menu-bar popover:** TERMINAL, three rows:
  `◇ CUSTOS`, state word, contained count.
- **Boot:** MONOLITH LOCKUP static frame fading to the main window; no glitch
  on boot (glitch is for incidents, not startup).
- **Incident/containment:** keep the same lockup but switch the ink to red and
  run the `▒` scan row once. Do not switch wordmark styles mid-session; the
  identity stays stable.

The final choice can mix a static lockup with one motion language. Do not
combine scan, orbit, and glitch in the same window; one motion language is the
hardcore rule.

## Implementation note

Render as a `Text` layer with a fixed-width monospaced font and explicit
`lineSpacing`. The box-drawing and block glyphs must be measured with the same
font; if a glyph is missing, fall back to pure ASCII `#`/`+`/`-` rather than a
proportional substitute. The wordmark is `accessibilityHidden`; the state word
and log carry the accessible information.
