# CUSTOS — accepted wordmark + diamond mark

Status: **design draft, no implementation**  
Date: 2026-11-26  
Companion: `docs/CUSTOS_CHARTER_V0.md`, `docs/CUSTOS_PLAN.md`

The accepted letterform is the tight four-row `Custos` wordmark. This file keeps
that wordmark unchanged and studies only the **drawn diamond mark** that goes to
its left. Everything here is pure ASCII; no Unicode box drawing, block glyphs,
or `◇` characters.

## Accepted wordmark

```text
  ___         _
 / __|_  _ __| |_ ___ ___
| (__| || (_-<  _/ _ (_-<
 \___|\_,_/__/\__\___/__/
```

## Why the first diamond was too steep

A single `/` or `\` occupies one character cell, and a terminal cell is roughly
twice as tall as it is wide. A one-column shift per row therefore looks like a
steep spike, not a 45-degree edge. A diamond with roughly 90-degree corners
needs a **two-column shift per row**. The `#`/`+` points make that step explicit:

```text
narrow slash (too steep)        wide step (about 45 degrees)

       /\                              #
      /  \                           #   #
     /    \                        #       #
      \  /                           #   #
       \/                              #
```

## Diamond marks

### 1. WIDE HASH (recommended)

One character per point, two-column steps. Reads as a diamond, stays light, and
matches the thin wordmark.

```text
    #
  #   #
#       #
  #   #
    #
```

Lockup with the accepted wordmark:

```text
    #
  #   #       ___         _
#       #    / __|_  _ __| |_ ___ ___
  #   #     | (__| || (_-<  _/ _ (_-<
    #        \___|\_,_/__/\__\___/__/
```

### 2. WIDE PLUS

Same geometry, `+` points. Lighter and more instrument-like; good when the
diamond should not compete with the wordmark.

```text
    +
  +   +
+       +
  +   +
    +
```

Lockup:

```text
    +
  +   +       ___         _
+       +    / __|_  _ __| |_ ___ ___
  +   +     | (__| || (_-<  _/ _ (_-<
    +        \___|\_,_/__/\__\___/__/
```

### 3. WIDE DOUBLE

Two characters per point. Heavier, more "sealed plate"; best for boot or a large
title card, not the compact header.

```text
    ##
  ##  ##
##      ##
  ##  ##
    ##
```

Lockup:

```text
    ##
  ##  ##       ___         _
##      ##    / __|_  _ __| |_ ___ ___
  ##  ##     | (__| || (_-<  _/ _ (_-<
    ##        \___|\_,_/__/\__\___/__/
```

### 4. NARROW SLASH (reference only)

The original `/\` mark. It is clean, but too steep for a "90-degree" diamond.
Kept here only as a comparison.

```text
   /\
  /  \
 /    \
  \  /
   \/
```

## Placement

- Diamond always to the **left** of `Custos`, never above it.
- The wordmark is four rows; the diamond is five rows. Align the wordmark's
  first row with the diamond's second row so the diamond has one row of air
  above and one row below.
- Keep at least two spaces between the diamond's widest row and the wordmark.
- Do not add a second diamond or a second frame around the wordmark.

## Animation

One motion language only:

- **Core pulse:** the top and bottom `#`/`+` points switch to `*` for one frame,
  then back. The letters do not move.
- **Scan:** a single `-` row crosses the wordmark once per 4 seconds. It does
  not replace a letter row.
- **Point travel:** for WIDE PLUS only, a `*` moves between the four points.
  Do not use this on WIDE HASH.

Recommended: core pulse on WIDE HASH, scan on WIDE DOUBLE, no animation on WIDE
PLUS (it is the quiet variant).

## State variants

Keep the letterforms identical. Change only ink/glow:

- `HEALTHY`: ice cyan.
- `WATCH`: amber.
- `DEGRADED`: amber-red.
- `CRITICAL`: red; the diamond points may pulse once per second.
- `QUARANTINED`: near-black fill with a red rim; no pulse.
- `SIGNAL LOST`: dim grey; no animation.

## Recommendation

- **Primary:** WIDE HASH. It solves the angle, stays legible at small sizes,
  and matches the accepted wordmark.
- **Quiet/secondary:** WIDE PLUS.
- **Boot/large title:** WIDE DOUBLE.
- **Do not use:** the narrow slash diamond as the primary mark.

## Implementation note

Render as a `Text` layer with a fixed-width monospaced font and explicit
`lineSpacing`. Measure the diamond and wordmark in the same font; if a glyph is
missing, fall back to `#`/`+`/`-`. The wordmark is `accessibilityHidden`; the
status word and log carry the accessible meaning.
