# ADR-0100: Calibrate context estimates with provider usage

- **Status:** Accepted
- **Date:** 2026-11-08

## Context

ADR-0098 estimates context from text shape (4 ASCII characters per token,
1.5 tokens per non-ASCII character). That is closer than the conservative
bytes/2 budget reservation, but it is not a tokenizer. Tool schemas, provider
message formatting, and CJK-heavy prompts can make the real input count larger
than the heuristic. Compacting too late is the dangerous failure mode because
the provider rejects the request or truncates it.

Every provider call may return exact input/output usage. That measurement was
already written to the usage ledger, but the context manager did not use it.

## Decision

1. **Measure every usable top-level call.** After a top-level provider call that
   reports usage, emit `context.measured` with the raw heuristic estimate,
   actual input/output tokens, raw ratio, applied calibration, sample count,
   model, and active message count. Calls without usage do not emit it.
2. **Scale upward only.** Calibration is an EWMA of `actual / estimated`
   samples clamped to at least 1.0. The applied multiplier is clamped to
   `[1.0, 2.0]`. A provider that reports fewer tokens than the heuristic never
   makes compaction later than it already was; underestimates are corrected,
   overestimates are tolerated.
3. **Persist and reload per session/model.** The measurement event is the
   source of truth. A run loads the latest measurement for its model and uses
   the stored multiplier; within a run, each measurement updates an in-memory
   EWMA. A model change ignores measurements from the previous model.
4. **Apply consistently.** The multiplier is used for:
   - active-context estimates in `context.usage`;
   - compaction threshold and hard-limit comparisons;
   - the summary transcript budget (divided by the multiplier, because the
     heuristic may undercount that request too).
   `context.compacted` records the multiplier used for its before/after values.
   Manual compaction loads the same calibration state.
5. **Expose it.** `session.context` / `vp context` include `last_measurement`;
   the Stage context popover shows estimated vs actual input, ratio, sample
   count, and the applied multiplier. A ratio above 1.1 is highlighted because
   it means the heuristic is undercounting.
6. **Child agents do not calibrate.** Context management remains a top-level
   concern; Worker/Labour context is bounded by their task prompts and the
   session/plan budgets.

## Consequences

- Estimates become more accurate without bundling tokenizer model files or
  provider-specific token counting.
- Compaction can happen earlier on providers whose real input is higher than
  the heuristic; that is intentional safety headroom.
- The multiplier is per session/model and persisted through events, so replay,
  export, and CLI inspection can see why a threshold was scaled.
- Providers that omit usage leave the multiplier at 1.0; the heuristic remains
  the fallback.
- The raw ratio can be distorted by provider caching or accounting differences;
  clamping and upward-only blending prevent a single sample from moving the
  threshold down. The factor is not an exact tokenizer and is capped at 2.0.
- One `context.measured` event is added per top-level provider call that reports
  usage. The UI event buffer has been widened so diagnostics do not evict
  visible conversation events.

## Alternatives considered

- **Bundle exact tokenizers:** large per-provider dependency surface, model
  revisions drift, and local endpoints still differ.
- **Trust provider usage only after the call:** too late to protect the current
  request; it cannot prevent overflow.
- **Scale both up and down:** would let efficient-tokenizer providers use more
  of the window, but a low sample from caching or accounting quirks could cause
  under-compaction. Upward-only is the safe default.
- **Global calibration across models:** rejected; tokenizers and provider
  framing differ, so measurements are keyed by model.
