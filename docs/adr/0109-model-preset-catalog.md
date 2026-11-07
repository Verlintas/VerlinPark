# ADR-0109: Model preset catalog

- **Status:** Accepted
- **Date:** 2026-11-08

## Context

Role model configuration repeats model name, context window, and input/output
prices. Users switching between local and hosted models retype the same values
and can accidentally mix a model with the wrong context window or price.
Provider credentials and base URLs are separate and must not be duplicated into
presets.

## Decision

1. **Metadata only.** `ModelPreset` stores name, provider, model, optional
   context window, optional input/output USD prices, notes, and timestamps.
   It stores no API key, base URL, or credential reference.
2. **Provider-scoped application.** A preset can be applied only when its
   provider matches the currently configured provider. Applying fills role
   model/context/price fields; it does not change the provider, base URL, API
   key, budgets, or routing.
3. **Client-side application.** The daemon stores/validates presets. Applying a
   preset is a UI edit of the role-model form; the user must Save & Connect on
   the Providers tab to persist it through the existing provider configuration
   path. This avoids a second hidden configuration source.
4. **CRUD.** SQLite `model_presets`, RPC `model.preset.list/create/update/delete`,
   CLI `vp model-presets` / `model-preset-*`, and a Settings → Providers modal.
5. **Validation.** Names/models are non-empty and length-bounded, providers are
   `openai-compatible` or `anthropic`, context windows are at most 10M, prices
   must be finite and 0–100,000, and notes are at most 2,000 characters.
6. **No hidden routing.** Presets are user-defined metadata; the runtime still
   resolves role models only from the configured role models/base model.

## Consequences

- Users can reuse model metadata without copying secrets or base URLs.
- A preset from another provider is rejected rather than silently applied.
- Preset edits do not take effect until provider settings are saved, which is
  explicit and matches the existing persistence path.
- The catalog is global to the daemon, not project-specific.
- Deleting a preset never changes current role configuration.

## Alternatives considered

- **Store API keys in presets:** duplicates Keychain responsibilities and
  increases secret-handling risk.
- **Apply presets server-side:** would create a second config mutation path and
  could be overwritten by the app's existing provider persistence.
- **Seed built-in model presets:** risks stale/hard-coded model metadata; users
  create their own.
