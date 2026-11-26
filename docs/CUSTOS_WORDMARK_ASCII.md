# CUSTOS — accepted wordmark + animation

Status: **design draft, no implementation**  
Date: 2026-11-26  
Companion: `docs/CUSTOS_CHARTER_V0.md`, `docs/CUSTOS_PLAN.md`

The accepted letterform is the tight four-row `Custos` wordmark. There is **no
small diamond before the word**: the large diamond belongs to the center of the
Custos window, and repeating it here would weaken both marks.

This file is pure ASCII. No Unicode box drawing, block glyphs, or `◇`
characters.

## Accepted wordmark

```text
  ___         _
 / __|_  _ __| |_ ___ ___
| (__| || (_-<  _/ _ (_-<
 \___|\_,_/__/\__\___/__/
```

## Animation principle

The wordmark is an identity, not a loading spinner. The letters should not
continuously morph, rotate, or bounce. The animation is a **terminal signal
lock**:

1. boot/reveal: a print head writes the word left to right;
2. idle: the word is static, with a single blinking cursor at the baseline;
3. state change: ink/glow changes; one short jitter is allowed on `CRITICAL`;
4. `QUARANTINED` / `SIGNAL LOST`: static, no cursor, no pulse.

This keeps the static frame strong and makes motion mean something.

---

## 1. Boot reveal: print head

A vertical `#` cursor sweeps left to right, revealing the word behind it. Each
step is 80–100 ms; the whole reveal is 600–900 ms. The cursor disappears after
the final frame.

Frame 0 — empty:

```text
#
```

Frame 1 — first slice:

```text
  ___#
 / __#
| (__#
 \___#
```

Frame 2 — middle:

```text
  ___         _#
 / __|_  _ __| |#
| (__| || (_-<  _#
 \___|\_,_/__/\__#
```

Frame 3 — complete:

```text
  ___         _
 / __|_  _ __| |_ ___ ___
| (__| || (_-<  _/ _ (_-<
 \___|\_,_/__/\__\___/__/
```

The reveal is a mask, not a rewrite: the final frame is always the exact
accepted wordmark. If the process is interrupted, show the completed static
frame rather than a half-written logo.

---

## 2. Idle: cursor blink

The wordmark is static. A single `_` cursor after the last line blinks at
1.6 s on / 1.6 s off. This is the only idle character animation.

On:

```text
  ___         _
 / __|_  _ __| |_ ___ ___
| (__| || (_-<  _/ _ (_-<
 \___|\_,_/__/\__\___/__/_
```

Off:

```text
  ___         _
 / __|_  _ __| |_ ___ ___
| (__| || (_-<  _/ _ (_-<
 \___|\_,_/__/\__\___/__/ 
```

The trailing `_` is intentionally outside the wordmark, like a terminal prompt.
Do not blink any letter inside the word.

Optional slow glow: the ink brightness breathes ±8% over 6 s. No character
changes. Reduce Motion disables both the cursor and the glow.

---

## 3. State change: ink, not movement

Keep the letterforms identical:

- `HEALTHY`: ice cyan; cursor blink 1.6 s.
- `WATCH`: amber; cursor blink 1.2 s; no letter movement.
- `DEGRADED`: amber-red; cursor blink 0.9 s; one slow scanline every 8 s.
- `CRITICAL`: red; one 2-frame row jitter, then static; scanline every 4 s.
- `QUARANTINED`: near-black with red rim; no cursor, no pulse, no scanline.
- `SIGNAL LOST`: dim grey; no cursor, no pulse.

### CRITICAL jitter

Frame A — normal:

```text
  ___         _
 / __|_  _ __| |_ ___ ___
| (__| || (_-<  _/ _ (_-<
 \___|\_,_/__/\__\___/__/
```

Frame B — second line shifts one column for one frame:

```text
  ___         _
  / __|_  _ __| |_ ___ ___
| (__| || (_-<  _/ _ (_-<
 \___|\_,_/__/\__\___/__/
```

Then back to frame A. This is the only glitch; do not loop it.

---

## 4. Optional scanline

If a scanline is preferred over the cursor, use a separate `-` row that moves
between the wordmark rows. It never replaces a letter row.

```text
  ___         _
 / __|_  _ __| |_ ___ ___
 -------------------------
| (__| || (_-<  _/ _ (_-<
 \___|\_,_/__/\__\___/__/
```

Timing: one sweep every 8 s in `HEALTHY`/`WATCH`, every 4 s in `CRITICAL`.
Do not combine the scanline with the blinking cursor; choose one idle motion.

---

## Comparison

| Direction | Idle motion | Feel | Recommendation |
|---|---|---|---|
| SIGNAL LOCK | cursor blink | terminal, restrained | **primary** |
| SCAN | slow scanline | radar/instrument | alternate |
| GLITCH | rare jitter | cyberpunk, noisy | CRITICAL only |
| BREATH | glow only | quiet, static | Reduce Motion / low-power |

## Recommendation

Use **SIGNAL LOCK**:

- boot: print-head reveal;
- idle: static wordmark + blinking `_` cursor;
- state: ink/glow only;
- CRITICAL: one short row jitter, then static;
- QUARANTINED / SIGNAL LOST: fully static.

This is the most "sealed instrument" option and keeps the wordmark readable at
all times. The only constant motion is a one-character cursor, which is easy to
ignore and easy to stop for Reduce Motion.

## Implementation note

Render as a `Text` layer with a fixed-width monospaced font and explicit
`lineSpacing`. The reveal is a width mask over the final string; do not build
the logo from per-character glyphs that can drift. The wordmark is
`accessibilityHidden`; the status word and log carry the accessible meaning.
