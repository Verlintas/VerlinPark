# ADR-0130: Project skill packs with explicit activation and tool narrowing

- **Status:** Accepted
- **Date:** 2026-11-10
- **Owners:** Runtime, Prompting, macOS UI
- **Related:** ADR-0024, ADR-0119, ADR-0121, ADR-0129

## Context

Role prompts and profiles describe *who* an agent is, but teams repeatedly need
reusable ways of working: a release review, a security checklist, a research
brief format, or a department-specific definition of done. Copying that text
into every session prompt is brittle and invisible. SourceWeft's capability
manifest review suggested a narrower useful unit: a project-scoped instruction
pack that an agent can discover and activate.

A skill must not become executable code, a hidden model route, or a permission
grant. It also must not silently replace the role's tool policy or the project's
approval policy.

## Decision

### 1. A skill is data with a validated contract

`Skill` stores project id, name, slug, description, when-to-use, version,
instructions, optional tool allow/deny lists, optional role/profile filters,
enabled state, source (`user`/`import`), SHA-256, and timestamps.

`SKILL.md` import supports YAML frontmatter with `name`, `description`,
`when_to_use`, `version`, `tools`/`allowed-tools`, disallowed tools, `profiles`,
and `roles`. The body becomes instructions. Frontmatter parsing is bounded by a
128 KiB file cap and validated name/description/instruction lengths; malformed
YAML fails explicitly instead of partially importing.

### 2. Skills are project-scoped and persisted in the project manifest

CRUD and enable/disable are RPCs (`skill.list/create/update/import/set_enabled/
delete`), CLI commands, and Settings → Skills. The project manifest includes the
full skill list so `db-rebuild` can restore them. Changes use the same
best-effort manifest write pattern as task intake: a manifest failure does not
claim the SQLite mutation failed.

### 3. Discovery is a compact catalog; activation is explicit

Only enabled skills that match the agent's role and profile appear in the
system prompt catalog (name, version, when-to-use, short description). The full
instructions are loaded only by calling:

```
use_skill(name)
```

The active skill replaces the previous one for the current run. The tool returns
the instructions plus the effective allow/deny policy.

### 4. Tool allow/deny can only narrow

A skill's tool policy is applied to:

1. the tool schema list on the next provider turn; and
2. every tool execution immediately (so a model cannot call a hidden or
   hallucinated name).

Deny always wins. A non-empty allow list restricts the tools to that list;
`use_skill` itself remains available so the agent can switch. A skill cannot
grant a tool that the role/profile does not already have. The permission broker,
approval grants, sandbox, budgets, and write scope remain unchanged.

This is not a security boundary against a malicious project author: a skill is
prompt text and policy metadata approved by the local user, like any other
project configuration. It is a deterministic narrowing layer over the existing
role policy.

### 5. UI is an explicit library, not hidden magic

Settings → Skills lists all skills with enabled state, version, source, role/
profile filters, tool policy, and instructions. Users can create, edit,
enable/disable, delete, or import a project-relative `SKILL.md`. The Stage
composer offers a wand menu that inserts a prompt asking the agent to use a
selected skill; the agent still activates it through `use_skill`.

## Consequences

- Reusable ways of working become inspectable project data.
- Imported skills are not executed and cannot add tools or bypass approvals.
- One active skill avoids ambiguous allow-list intersections; switching is
  explicit and audited by the normal tool-call stream.
- Skills consume context only when activated; the catalog is bounded to 24
  entries / 4,000 characters.
- `SKILL.md` is a supported import format, but VerlinPark does not claim full
  compatibility with every external skill ecosystem.

## Alternatives considered

- **Inject every enabled skill into the system prompt.** Rejected: unbounded
  context and prompt drift.
- **Allow multiple active skills with unioned tool allow lists.** Rejected:
  widening semantics are hard to reason about and could bypass a stricter skill.
- **Execute skill scripts.** Rejected: arbitrary code execution belongs behind
  the existing shell/tool sandbox and approval path, not a prompt manifest.
- **Put skills in the repository only.** Deferred: project-relative import is
  supported, while the daemon-owned projection makes enable/disable and rebuild
  independent of the checkout.
