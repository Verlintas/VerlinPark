# 0233 — Non-destructive provider reconnect

- Status: Accepted
- Date: 2026-11-23
- Scope: macOS client provider handoff on daemon reconnect, provider ownership, stale client defaults, regression tests
- Depends on: ADR-0001 (SwiftUI client / Rust daemon), ADR-0003 (manual model and budget configuration), ADR-0232 (operator channel live-run hardening)
- Plan: [V1_FINAL_PLAN.md](../V1_FINAL_PLAN.md)

## Context

The Rust daemon owns live runtime configuration; the macOS app is a client. On
every successful connection the app re-applies its stored provider settings so
that a Keychain rotation, role-model change, team-capacity change, or approval
mode can reach the daemon.

That automatic handoff was not bounded by provider identity. If the app still
had an older local provider in `UserDefaults` (for example a localhost endpoint
with a model name), a reconnect could replace a daemon that had been configured
outside the app — by environment variables, another client, or an operator CLI
— with the stale local endpoint. A keyless localhost provider bypasses the
Keychain requirement, so the overwrite could succeed silently and the next run
would fail against an endpoint the operator never chose.

## Decision

An automatic reconnect applies the stored provider only when one of these is
true:

1. the daemon reports `configured: false`; or
2. the stored provider already matches the daemon's reported provider kind,
   normalized base URL, and model.

A mismatch is an explicit operator decision. The client leaves the daemon
provider unchanged and shows a status message naming the daemon's current
provider/model; the Settings save action remains the only path that replaces a
different provider. The pure decision lives in
`ProviderSyncRules` (`apps/macos/VerlinPark/Core/ProviderSyncRules.swift`).

Normalization is deliberately conservative:

- trim whitespace and compare provider/model case-insensitively;
- lowercase URLs and remove trailing `/` characters;
- require all three daemon fields (provider, base URL, model) to be present;
  missing metadata fails closed and does not authorize an overwrite.

The same rule applies to automatic reconnects and to the app's convenience
actions that call the same path (`applyOrgSettings`, `applyApprovalMode`). An
explicit provider save still calls `agent.configure` directly and can replace
the daemon provider.

## Reasons

- The daemon is the source of truth for a live run; a client reconnect must not
  silently change the provider under an active or operator-configured daemon.
- Re-applying an identical provider is still useful and remains allowed, so
  Keychain rotation and cap/approval changes keep working on reconnect.
- Treating incomplete daemon metadata as a mismatch is fail-closed. A visible
  "save to replace" message is better than a guessed overwrite.
- The decision is a small pure function, testable without a daemon, Keychain,
  or network.

## Consequences

- Positive: a daemon launched with environment provider settings survives app
  reconnects; stale local defaults can no longer redirect a run; the behavior
  is unit-tested.
- Negative: a user who edits app defaults by hand (outside Settings) must save
  once before the daemon follows; convenience actions can report that capacity
  or approval mode was saved but not applied while the daemon provider differs.
- Follow-up: the Settings surface should make the "daemon uses another
  provider" state explicit next to the save action. This round only guarantees
  the non-destructive backend/client decision and status text.

## Tests

`apps/macos/VerlinParkTests/RuntimeReliabilityTests.swift` covers: an
unconfigured daemon accepts stored settings; an equivalent endpoint with a
trailing slash is idempotently re-applied; a different base URL, model, or
incomplete daemon identity is not overwritten. The live check launched the app
with localhost `qwen2.5-coder` defaults against a daemon configured with
DeepSeek and confirmed `provider-test` still returned `deepseek-flash`.

## Alternatives

- Apply only on the first connection after app launch: rejected because a
  transient daemon restart could still clobber a manually configured provider.
- Always prompt the operator on mismatch: rejected for automatic reconnects;
  the Settings save action is already the explicit decision point.
- Compare only the model name: rejected because two providers can advertise the
  same model name while pointing at different endpoints.
- Persist the daemon provider into client defaults on connect: rejected because
  it would make stale settings look authoritative and still overwrite after a
  later external change.
