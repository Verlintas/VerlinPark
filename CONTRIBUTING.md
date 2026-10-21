# Contributing to VerlinPark

VerlinPark is currently in the **design phase**. The most useful contributions right now are:

- critique of the product thesis and multi-agent organization design
- counterexamples / failure modes from running agent teams on real repositories
- benchmark tasks and fixture repositories
- ADR proposals for the open questions in [docs/DECISIONS.md](docs/DECISIONS.md)
- once the first code lands: Rust core, SwiftUI app, tests, docs

## Ground rules

- Be direct and technical. Argument from evidence beats argument from taste.
- Keep the scope honest. v0.1 is a focused multi-agent department release, not a general IDE.
- Every behavior change needs a test or a reproducible scenario.
- Security and permission changes need a threat-model note.
- No vendor lock-in: new providers/tools should fit the adapter/registry model.

## Development setup (planned)

The first milestone will add a `justfile` and a build guide. Expected toolchain:

- macOS 15+ on Apple silicon
- Xcode 27+ / Swift 6.4+
- Rust stable (`rust-toolchain.toml`)
- [XcodeGen](https://github.com/yonaskolb/XcodeGen)
- [`just`](https://github.com/casey/just)
- Git

Planned commands:

```bash
just bootstrap
just build
just test
just run
```

## Architecture decisions

- Read [docs/DECISIONS.md](docs/DECISIONS.md).
- New decisions use [docs/adr/0000-template.md](docs/adr/0000-template.md).
- Superseding an accepted decision requires a new ADR; do not rewrite history.

## Pull requests

- Keep PRs small and focused.
- Describe the problem, the approach, and the verification.
- UI changes: include a screenshot or short recording.
- Agent/runtime changes: include the event trace or test scenario.
- Do not include secrets, API keys, private repository contents, or personal data.

## Commit style

Prefer conventional, scoped commits:

```text
feat(core): validate task write scopes
fix(daemon): cancel tool process groups on session cancel
docs(plan): clarify v0.2 scope
test(scheduler): cover deadlock detection
```

## License

By contributing, you agree that your contributions are licensed under **GPL-3.0-only**, the same license as the project. There is no CLA.

## Security

Do not open a public issue for a vulnerability. Contact the maintainer privately (see the GitHub profile) with:

- affected version/commit
- reproduction steps
- impact
- suggested fix, if any

## Conduct

Be decent. Harassment, discrimination, and bad-faith disruption are not welcome. Technical disagreement is.
