# ADR-0117: Runtime English / Simplified Chinese localization

- **Status:** Accepted
- **Date:** 2026-11-08

## Context

The native client was English-only. The user needs at least English and
Simplified Chinese, switchable without restarting the app. A large part of the
UI is static SwiftUI text, but section titles, palette labels, status cells, and
component parameters are computed `String`s. Translation must cover both paths
without introducing a second source of truth or a daemon protocol dependency.

## Decision

1. **Two app languages.** `AppLanguage` defines `en` and `zh-Hans`.
   `LanguageStore` is an `@Observable` object persisted in
   `UserDefaults` under `verlinpark.language`; it is injected into both the
   workspace and the Settings scene.
2. **Static literals use the system localization pipeline.** SwiftUI
   `Text`/`Button`/`Section`/`TextField` literals resolve through the active
   `.environment(\.locale)` and the bundled
   `zh-Hans.lproj/Localizable.strings` table. The root and Settings content use
   a language-scoped identity so a switch re-renders the tree immediately.
3. **Computed strings use one helper.** `L10n.t` and `L10n.format` read the same
   table explicitly for enum titles, palette metadata, status labels, and
   component parameters. Missing keys fall back to the English source key.
4. **Two entry points.** Settings gains a General section with language cards;
   the top chrome gains a globe menu that switches EN / 中文 anywhere in the
   workspace. Both mutate the same store.
5. **Scope is explicit.** Shell navigation, Settings, Studio, Stage, Manage
   Dash, command palette, search, notifications, shortcuts, and new-session
   surfaces are localized. Daemon-generated messages, model/provider names,
   repository content, and long technical diagnostics may remain English.
   Untranslated keys intentionally fall back rather than showing raw keys.
6. **Resources are versioned with the app.** The Chinese table lives in
   `Resources/zh-Hans.lproj/Localizable.strings`; no network or daemon
   translation service is involved.

## Consequences

- Language changes apply immediately and persist across launches.
- Static and computed labels stay aligned by using the same table.
- The translation table is a maintenance surface; new UI strings must add a
  Chinese entry when they should be localized.
- English remains the source language, so a missing translation is still
  readable.
- The runtime does not translate agent output, repository text, or provider
  errors; those are data, not UI chrome.

## Alternatives considered

- **Restart-only locale switching:** violates the expected in-app language
  switch and is hostile during a long run.
- **Translate every string at call sites:** duplicates keys and makes fallback
  behavior inconsistent.
- **Only localize Settings:** insufficient; navigation and Stage must switch
  together.
- **Daemon-side translation:** the daemon owns runtime state, not presentation
  language; it would couple protocol output to UI preferences.
- **Machine translation at runtime:** nondeterministic, requires network, and
  cannot be reviewed.
