# CUSTOS — accepted wordmark + animation v2

Status: **design draft, no implementation**  
Date: 2026-11-26  
Companion: `docs/CUSTOS_CHARTER_V0.md`, `docs/CUSTOS_PLAN.md`

The accepted letterform is the tight four-row `Custos` wordmark. There is **no
small diamond before the word**: the large diamond belongs to the center of the
Custos window.

This file is pure ASCII. No Unicode box drawing, block glyphs, `◇`, or scanline.

## Accepted wordmark

```text
  ___         _
 / __|_  _ __| |_ ___ ___
| (__| || (_-<  _/ _ (_-<
 \___|\_,_/__/\__\___/__/
```

## Animation principle

The wordmark is an identity, not a loading spinner. The letters never morph,
rotate, or bounce. Motion is split into three moments:

1. **boot** — a print head writes the word left to right (more frames, not a
   single flash);
2. **idle** — static letters, a slow baseline carrier, and a soft glow breath;
3. **critical** — a short multi-frame jitter, then static. No scanline.

---

## 1. Boot: 8-frame print head

A vertical `|` print head sweeps left to right. Each step is ~70–80 ms; the
whole reveal is ~600–700 ms, then a 120 ms hold.

### Frame 0 — empty

```text
|
|
|
|
```

### Frame 1 — cut 4

```text
  __|
 / _|
| (_|
 \__|
```

### Frame 2 — cut 8

```text
  ___   |
 / __|_ |
| (__| ||
 \___|\_|
```

### Frame 3 — cut 12

```text
  ___       |
 / __|_  _ _|
| (__| || (_|
 \___|\_,_/_|
```

### Frame 4 — cut 16

```text
  ___         _ |
 / __|_  _ __| ||
| (__| || (_-<  |
 \___|\_,_/__/\_|
```

### Frame 5 — cut 20

```text
  ___         _     |
 / __|_  _ __| |_ __|
| (__| || (_-<  _/ _|
 \___|\_,_/__/\__\__|
```

### Frame 6 — cut 24

```text
  ___         _         |
 / __|_  _ __| |_ ___ __|
| (__| || (_-<  _/ _ (_-|
 \___|\_,_/__/\__\___/__|
```

### Frame 7 — cut 28, cursor at the end

```text
  ___         _             |
 / __|_  _ __| |_ ___ ___   |
| (__| || (_-<  _/ _ (_-<   |
 \___|\_,_/__/\__\___/__/   |
```

### Frame 8 — complete, cursor gone

```text
  ___         _
 / __|_  _ __| |_ ___ ___
| (__| || (_-<  _/ _ (_-<
 \___|\_,_/__/\__\___/__/
```

The reveal is a mask; the final frame is always the exact accepted wordmark.
If boot is interrupted, show frame 8, not a half-written logo.

---

## 2. Idle: baseline carrier + glow

The wordmark is static. A 27-character baseline carries a 3-character `===`
segment from left to right every 6 seconds. A soft glow breathes ±8% over the
same 6-second cycle, offset by 3 seconds.

Carrier frame A:

```text
  ___         _
 / __|_  _ __| |_ ___ ___
| (__| || (_-<  _/ _ (_-<
 \___|\_,_/__/\__\___/__/
 ===------------------------
```

Carrier frame B:

```text
  ___         _
 / __|_  _ __| |_ ___ ___
| (__| || (_-<  _/ _ (_-<
 \___|\_,_/__/\__\___/__/
 --------===----------------
```

Carrier frame C:

```text
  ___         _
 / __|_  _ __| |_ ___ ___
| (__| || (_-<  _/ _ (_-<
 \___|\_,_/__/\__\___/__/
 ----------------===--------
```

Carrier frame D:

```text
  ___         _
 / __|_  _ __| |_ ___ ___
| (__| || (_-<  _/ _ (_-<
 \___|\_,_/__/\__\___/__/
 ------------------------===
```

Rules:

- the carrier is a **signal line**, not a progress bar; it never has a border,
  percentage, or label;
- the wordmark never moves during idle;
- `WATCH` shortens the carrier loop to 4s; `DEGRADED` to 3s;
- `CRITICAL` pauses the carrier during the jitter, then resumes at 2s;
- `QUARANTINED` and `SIGNAL LOST` stop the carrier and the glow.

This replaces the blinking cursor. If the carrier feels like too much, the
fallback is **glow only**: same wordmark, no character motion, ±8% breathing.

---

## 3. CRITICAL: multi-frame jitter

The jitter is a short controlled fracture, not a loop. 7 frames at 10 fps
(~700 ms), then static. At most two cycles per incident.

### Frame 1 — normal

```text
  ___         _
 / __|_  _ __| |_ ___ ___
| (__| || (_-<  _/ _ (_-<
 \___|\_,_/__/\__\___/__/
```

### Frame 2 — all rows right 1

```text
   ___         _
  / __|_  _ __| |_ ___ ___
 | (__| || (_-<  _/ _ (_-<
  \___|\_,_/__/\__\___/__/
```

### Frame 3 — all rows left 1

```text
 ___         _
/ __|_  _ __| |_ ___ ___
| (__| || (_-<  _/ _ (_-<
\___|\_,_/__/\__\___/__/
```

### Frame 4 — row 2 right 2, row 3 left 1

```text
  ___         _
   / __|_  _ __| |_ ___ ___
| (__| || (_-<  _/ _ (_-<
 \___|\_,_/__/\__\___/__/
```

### Frame 5 — one substituted character

```text
  ___         _
 / __|_  _ __| |_ ___ ___
| (__| || (_#<  _/ _ (_-<
 \___|\_,_/__/\__\___/__/
```

### Frame 6 — tail cursor

```text
  ___         _
 / __|_  _ __| |_ ___ ___
| (__| || (_-<  _/ _ (_-<
 \___|\_,_/__/\__\___/__/#
```

### Frame 7 — normal, hold

```text
  ___         _
 / __|_  _ __| |_ ___ ___
| (__| || (_-<  _/ _ (_-<
 \___|\_,_/__/\__\___/__/
```

Do not add a scanline. The jitter is the entire critical animation.

---

## 4. State timing

| State | Boot | Idle | Critical |
|---|---|---|---|
| `HEALTHY` | 8-frame print head | carrier 6s + glow | — |
| `WATCH` | same | carrier 4s + glow | — |
| `DEGRADED` | same | carrier 3s + glow | — |
| `CRITICAL` | same | carrier paused during jitter, then 2s | 7-frame jitter, max 2 cycles |
| `QUARANTINED` | static frame | none | none |
| `SIGNAL LOST` | static frame | none | none |

Reduce Motion: static wordmark, no carrier, no glow, no jitter. State color is
the only difference.

## Implementation note

Render as a `Text` layer with a fixed-width monospaced font and explicit
`lineSpacing`. The boot reveal is a width mask over the final string; the idle
carrier is a separate baseline row; the critical jitter is a precomputed list of
six alternate strings. The wordmark is `accessibilityHidden`; the status word
and log carry the accessible meaning.
