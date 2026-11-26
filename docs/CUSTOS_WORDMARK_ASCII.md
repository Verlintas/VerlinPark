# CUSTOS — pure-ASCII wordmark studies

Status: **design draft, no implementation**  
Date: 2026-11-26  
Companion: `docs/CUSTOS_CHARTER_V0.md`, `docs/CUSTOS_PLAN.md`

This is the **CUSTOS name itself**. The first attempt used Unicode block and
box-drawing glyphs and did not render reliably; this version is **pure ASCII**
and draws the diamond mark explicitly with `/` and `\`, placed before the word.

## Rules

- Only ASCII characters: letters, digits, space, `# + - | / \ _ = * . :`.
- No Unicode box drawing, block elements, or diamond glyphs.
- Fixed-width monospace; the static frame must read without animation.
- The diamond is drawn, not typed as a glyph.
- Animation is character-based; 8–12 fps; no flashing above 3 Hz.
- State color changes ink/glow, not letterforms.
- The art is `accessibilityHidden`; status text carries the accessible meaning.

---

## 1. DIAMOND / PIXEL

The cleanest option: a four-row diamond and four-row pixel letters. Reads well
small, survives monochrome, and has enough negative space to feel sealed.

```text
  /\      ###  #   #  ###  #####  ###   ###
 /  \    #     #   # #       #   #   # #
 \  /    #     #   #  ##     #   #   #  ##
  \/      ###   ###  ###     #    ###  ###
```

Animation options:

- a `+` appears at the diamond center for one frame, then disappears;
- a single `-` scan row crosses the word once every 4 seconds;
- the diamond can blink between `/\` and `<>` once on state change.

Best for: primary wordmark, main header, compact boot frame.

---

## 2. DIAMOND / SLAB

Heavier letters, five rows. More weight than PIXEL; the diamond is larger and
the word feels engraved.

```text
   /\      ####  ##  ##  ####  ######  ####   ####
  /  \    ##     ##  ## ##       ##   ##  ## ##
 /    \   ##     ##  ##  ###     ##   ##  ##  ###
  \  /    ##     ##  ##    ##    ##   ##  ##    ##
   \/      ####   ####  ####     ##    ####  ####
```

Animation options:

- a `:` scan row moves up/down through the slab;
- the diamond center holds a single `*` for one frame;
- no letter movement; the slab stays fixed.

Best for: boot/identity plate, a heavier main header, or a title card.

---

## 3. DIAMOND / WIRE

Thin `+ - |` letters. It looks like an instrument panel or a circuit trace,
not a logo. The diamond is the only closed shape.

```text
   /\     +----  |   |  +----  +----+  +---+  +----
  /  \    |      |   |  |         |    |   |  |
 /    \   |      |   |  +---+     |    |   |  +---+
  \  /    |      |   |      |     |    |   |      |
   \/     +----  +---+  +----     |    +---+  +----
```

Animation options:

- a `*` node travels along the top `+----` rule;
- the diamond outline stays fixed;
- one frame every 4 seconds, no continuous motion.

Best for: a quieter secondary header, diagnostics-adjacent panels, or a
large-but-light main window.

---

## 4. DIAMOND / TERMINAL

Pure `#` letters, five rows. The rawest option; looks like it was printed by a
terminal, not designed in a graphics tool.

```text
   /\      ####  #   #   ####  #####   ###    ####
  /  \    #      #   #  #        #    #   #  #
 /    \   #      #   #   ###     #    #   #   ###
  \  /    #      #   #      #    #    #   #      #
   \/      ####   ###   ####     #     ###   ####
```

Animation options:

- one-row glitch: shift the word one column for a single frame;
- one row becomes `====` for a scanline frame, then restores;
- the diamond stays fixed.

Best for: menu-bar popover, raw/terminal mode, or a deliberately unpolished
boot frame.

---

## 5. DIAMOND / CUSTOS

Mixed-case `Custos` in a four-row terminal face, with the four-row diamond
before it. More elegant and less brutal; closer to a hacker-tool nameplate.

```text
  /\       ___  _   _  ___  _    ___   ___
 /  \     / __| | | | | / __| | |_ / _ \ / __|
 \  /    | (__  | |_| | \__ \ | __| | (_) | \__ \
  \/      \___|  \__,_| |___/  \__| \___/ |___/
```

Animation options:

- a `*` pulses inside the diamond;
- a `-` scanline crosses the word once per 6 seconds;
- no letter movement.

Best for: a softer identity variant, documentation header, or About panel.
Less "sealed guardian" than PIXEL/SLAB.

---

## Comparison

| Variant | Weight | Static readability | Motion | Best use |
|---|---|---|---|---|
| DIAMOND / PIXEL | medium | strongest | center pulse / scan row | primary header |
| DIAMOND / SLAB | heavy | strong | scan row | boot/identity plate |
| DIAMOND / WIRE | light | high | node travel | quiet secondary header |
| DIAMOND / TERMINAL | medium | high | glitch / `=` row | menu popover |
| DIAMOND / CUSTOS | light-medium | high | diamond pulse | docs/About |

## Recommendation

- **Primary:** DIAMOND / PIXEL. It is the best balance of hardcore, legible,
  pure ASCII, and animation-safe.
- **Boot / large identity:** DIAMOND / SLAB.
- **Menu-bar popover:** DIAMOND / TERMINAL, at three lines:
  `CUSTOS`, state word, contained count.
- **Quiet secondary header:** DIAMOND / WIRE.
- **About/docs:** DIAMOND / CUSTOS.

The chosen variant should keep the diamond at the left of the word, never above
it, and should use one motion language only. Do not mix scan, node, and glitch
in the same window.

## Implementation note

Render as a `Text` layer with a fixed-width monospaced font and explicit
`lineSpacing`. The diamond and letters must be measured in the same font. If a
glyph is missing, fall back to ASCII `#`/`+`/`-`; never substitute a proportional
symbol. The wordmark is `accessibilityHidden`.
