# ◇ CUSTOS — ASCII / box-drawing visual studies

Status: **design draft, no implementation**  
Date: 2026-11-26  
Companion: `docs/CUSTOS_CHARTER_V0.md`, `docs/CUSTOS_PLAN.md`

These are fixed-width character studies for the sealed Custos **window layout**
(sigil, rings, status). They are not the CUSTOS wordmark itself; see
`docs/CUSTOS_WORDMARK_ASCII.md` for the name/logo studies. They are not pixel
mockups and not final assets. The chosen direction will be rendered as a
monospaced text layer with a static fallback, not as an image.

> **Motion note (2026-11-26):** scanline is not part of the implemented Custos
> motion language. The layout studies below still mention it as a concept, but
> the final wordmark and critical animation use an 8-frame print-head reveal, a
> baseline carrier + glow, and a 7-frame jitter. Treat scanline references here
> as discarded exploration.

## Rules

- Fixed-width monospace only; no proportional fallback, no ligatures.
- Target width: 56–62 columns inside the window; the art never carries
  information that is not also present in text.
- Character animation: 8–12 fps, pulse ≥ 1.2s, scan ≥ 4s, no flashing > 3 Hz.
- Reduce Motion freezes the static frame; VoiceOver ignores the art layer.
- State is carried by the status word and color, not by a subtle glyph change.
- No ASCII art in the diagnostics sheet; that surface is plain text and file
  manifests.
- The menu-bar item is a small `◇` template glyph with the state color; its
  popover is three lines, no controls.

## Palette

| State | Ice / accent | Use |
|---|---|---|
| `HEALTHY` | ice cyan `#9FE8FF` | steady low glow |
| `WATCH` | pale amber `#FFC46B` | slow pulse |
| `DEGRADED` | amber-red `#FF8A5C` | faster pulse |
| `CRITICAL` | red `#FF3B4D` | scanline + red core |
| `QUARANTINED` | near-black + red rim | static, no pulse |
| `SIGNAL LOST` | dim grey `#6B7280` | broken ring, no animation |

---

## 1. MONOLITH

Concept: nested diamond/cross, almost no decoration. The strongest static
readability; the animation is only a slow core pulse and a single scanline.

```
╔══════════════════════════════════════════════════════════╗
   ◇ CUSTOS                                         SEALED
╚══════════════════════════════════════════════════════════╝

                   ┌──────────────────┐
                   │      ◇◇◇◇◇◇      │
             ┌─────┴──────────────────┴─────┐
             │                              │
       ┌─────┤          ┌──────┐            ├─────┐
       │     │          │  ◇   │            │     │
       │     │          └──────┘            │     │
       └─────┤                              ├─────┘
             │          ◇◇◇◇◇◇◇◇            │
             └─────┬──────────────────┬─────┘
                   │      ◇◇◇◇◇◇      │
                   └──────────────────┘

   HEALTHY                                  PROTECTION ON
   RUNTIME ▓▓▓▓▓  DATA ▓▓▓▓▓  SIGNAL ▓▓▓▓▓  RING ▓▓▓▓▓
```

Core pulse, 3 frames (only the center changes):

```
       │  ◇   │          │  ◆   │          │  ◇   │
       │      │          │      │          │      │
       └──────┘          └──────┘          └──────┘
```

Scanline, 3 frames (a thin `─` moves across the inner cross; keep opacity low):

```
   ──────────────────────────────────────────────────────
        ─────────────────────────────────────────────
             ───────────────────────────────────
```

Best for: main window default. Lowest visual noise, strongest "sealed
instrument" feel.

---

## 2. VIGIL

Concept: a central eye/slit. The Red Queen without a face; the scanline is the
only motion. More presence than MONOLITH, still readable.

```
╔══════════════════════════════════════════════════════════╗
   ◇ CUSTOS                                          VIGIL
╚══════════════════════════════════════════════════════════╝

                   ┌──────────────┐
               ┌───┤              ├───┐
             ┌─┤   │     ◇◇◇      │   ├─┐
           ┌─┤ │   │    ╱   ╲     │   │ ├─┐
         ┌─┤ │ │   │   ╱  ◇  ╲    │   │ │ ├─┐
         │ │ │ │   │  ────◇────   │   │ │ │ │
         └─┤ │ │   │   ╲  ◇  ╱    │   │ │ ├─┘
           └─┤ │   │    ╲   ╱     │   │ ├─┘
             └─┤   │     ◇◇◇      │   ├─┘
               └───┤              ├───┘
                   └──────────────┘

   HEALTHY                                  PROTECTION ON
   SIGNAL ▓▓▓▓▓  DATA ▓▓▓▓▓  RUNTIME ▓▓▓▓▓  RING ▓▓▓▓▓
```

Slit scan, 3 frames:

```
       ────◇────          ───◇─────          ─────◇───
          │                 │                  │
```

Blink, 3 frames (rare; only on state change, not idle):

```
        ╱   ╲             ─────             ╱   ╲
       ╱  ◇  ╲           ──◇──             ╱  ◇  ╲
        ╲   ╱             ─────             ╲   ╱
```

Best for: main window alternate or boot transition. More "watcher" than
MONOLITH, slightly less neutral.

---

## 3. REACTOR

Concept: radial/hex rings with orbiting nodes. The most active variant; use it
when Custos is containing something, or as an optional "Ops" view. The core is
still a single `◇`.

```
╔══════════════════════════════════════════════════════════╗
   ◇ CUSTOS                                        REACTOR
╚══════════════════════════════════════════════════════════╝

                     ╱╲
                   ╱    ╲
                 ╱   ◇◇   ╲
               ╱   ╱    ╲   ╲
             ╱   ╱   ◇◇   ╲   ╲
           ╱___╱___╱____╲___╲___╲
           ╲   ╲   ╲    ╱   ╱   ╱
             ╲   ╲   ◇◇   ╱   ╱
               ╲   ╲    ╱   ╱
                 ╲   ◇◇   ╱
                   ╲    ╱
                     ╲╱

   HEALTHY                                  PROTECTION ON
   CORE ▓▓▓▓▓  RING ▓▓▓▓▓  SIGNAL ▓▓▓▓▓  DATA ▓▓▓▓▓
```

Orbit, 3 frames (nodes travel around the outer hex; keep only two nodes):

```
        ◇                    ◇                    ◇
      ╱   ╲                ╱   ╲                ╱   ╲
    ◇       ◇            ◇       ◇            ◇       ◇
      ╲   ╱                ╲   ╱                ╲   ╱
        ◇                    ◇                    ◇
```

Best for: containment/attention states, not the default. It says "something is
running" more loudly than MONOLITH.

---

## 4. RELIC

Concept: a vertical chip/spine with two sealed cores. Cyberpunk Relic energy;
the animation is a data packet traveling down the spine. Very good as a compact
window or menu-bar popover.

```
╔══════════════════════════════════════════════════════════╗
   ◇ CUSTOS                                          RELIC
╚══════════════════════════════════════════════════════════╝

                 ░░░░░░░░░░░░░░░░░░
                 ░    ┌────────┐    ░
                 ░    │   ◇    │    ░
                 ░    └────────┘    ░
                 ░░░░░░░░░░░░░░░░░░
                          │
                     ┌────┴────┐
                     │  ◇   ◇  │
                     └────┬────┘
                          │
                 ░░░░░░░░░░░░░░░░░░
                 ░    ┌────────┐    ░
                 ░    │   ◇    │    ░
                 ░    └────────┘    ░
                 ░░░░░░░░░░░░░░░░░░

   HEALTHY                                  PROTECTION ON
   SIGNAL ▓▓▓▓▓  DATA ▓▓▓▓▓  RUNTIME ▓▓▓▓▓  RING ▓▓▓▓▓
```

Packet travel, 3 frames:

```
                          ·                    ◆
                          │                    │
                     ┌────┴────┐          ┌────┴────┐
                     │  ◇   ◇  │          │  ◇   ◇  │
                     └────┬────┘          └────┬────┘
                          │                    │
                          ◆                    ·
```

Best for: compact window or menu-bar popover; also a good boot frame. The
double-core reads as "two layers of Custos" (authority + self-heal).

---

## 5. CATHEDRAL

Concept: a large diamond/orbit, almost no machinery. Ceremonial, quiet, best as
a boot/safe-mode frame rather than the daily operational window.

```
╔══════════════════════════════════════════════════════════╗
   ◇ CUSTOS                                       CATHEDRAL
╚══════════════════════════════════════════════════════════╝

                        ◇
                      ◇   ◇
                    ◇       ◇
                  ◇           ◇
                ◇               ◇
              ◇                   ◇
            ◇          ◇◇◇          ◇
              ◇                   ◇
                ◇               ◇
                  ◇           ◇
                    ◇       ◇
                      ◇   ◇
                        ◇

   HEALTHY                                  PROTECTION ON
   SIGNAL ▓▓▓▓▓  DATA ▓▓▓▓▓  RUNTIME ▓▓▓▓▓  RING ▓▓▓▓▓
```

Orbit, 3 frames (one node travels around the diamond):

```
                        ◇
                      ◇   ◇
                    ◇       ◇
                  ◇           ◇
                ◇       ◇       ◇
              ◇                   ◇
            ◇          ◇◇◇          ◇
```

Best for: launch/boot, safe mode, or the moment Custos restores a verified
backup. Not for the always-open window; too little information density.

---

## Comparison

| Variant | Density | Static strength | Motion | Best use |
|---|---|---|---|---|
| MONOLITH | low | strongest | core pulse + scan | default main window |
| VIGIL | medium | strong | slit scan / rare blink | alternate main / boot |
| REACTOR | high | medium | orbiting nodes | containment/attention |
| RELIC | medium | strong | data packet | compact window / menu popover |
| CATHEDRAL | low | ceremonial | orbit | boot / safe mode / recovery |

## Recommendation

- **Main window:** MONOLITH. It is the most "sealed instrument" and leaves room
  for the status text and bounded log.
- **Boot / launch:** CATHEDRAL or VIGIL, one static frame fading to MONOLITH.
- **Containment state:** REACTOR, but only the outer ring changes; the center
  remains a `◇`.
- **Menu-bar popover:** RELIC, 3 lines: `◇ CUSTOS`, state word, contained count.
- **Safe mode / recovery:** CATHEDRAL static, red rim, no pulse.

The final choice can mix a static main frame with a single motion language
(pulse, scan, or packet), but should not combine all three. One motion language
per window is the hardcore rule.

## Implementation note

Render the art as a `Text` layer with a fixed-width monospaced font and explicit
`lineSpacing`; do not use a proportional font or rely on Unicode width tables
for alignment. If a glyph is missing, fall back to `#`/`+`/`-` ASCII rather than
substituting a proportional symbol. The status text and log remain normal
localizable text; the art layer is `accessibilityHidden`.
