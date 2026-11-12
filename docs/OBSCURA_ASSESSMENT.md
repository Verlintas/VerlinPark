# Obscura 评估：Rust headless browser for AI agents

> **Status:** assessment only, not implemented.
> **Date:** 2026-11-12
> **Upstream:** <https://github.com/h4ckf0r0day/obscura> (Apache-2.0, v0.2.6 at review time)
> **Decision:** useful as an optional **external-web** browser backend; not a
> replacement for the WKWebView localhost/file browser, and not part of the
> cache-lossless round.

## 1. What it is

Obscura is an independent headless browser engine written in Rust:

- embeds V8 and executes page JavaScript, without shipping Chromium;
- speaks the Chrome DevTools Protocol (CDP), so Puppeteer/Playwright can connect;
- has first-class CLI commands (`fetch`, `scrape`, `serve`, `mcp`);
- renders CSS/paint for screenshots, full-page capture, screencast, and PDF;
- exposes an MCP server over stdio or HTTP with navigate/snapshot/click/fill/
  type/press/select/evaluate/wait/network/console/close tools;
- blocks private/internal IPs by default (SSRF protection), with an explicit
  `--allow-private-network` escape hatch for local dev servers;
- optionally builds a stealth mode (fingerprint randomization, tracker
  blocking); the default build does not enable it;
- is a single ~70 MiB binary with a claimed ~30 MB runtime footprint.

It is not a Chromium drop-in in the compatibility sense. The upstream README
explicitly says long-tail CSS, some Web APIs, media playback, compositor
effects, and platform font rasterization can differ. Treat its rendering as an
independent engine, not as proof of Chrome parity.

## 2. Why it matters for VerlinPark

VerlinPark already has a `browser` tool, but its contract is deliberately narrow:

- daemon-launched WKWebView helper;
- localhost HTTP(S) and worktree `file:` URLs only;
- DOM-derived snapshots, viewport screenshots, console/evaluate;
- one daemon-wide `gui` lease shared with native `computer`;
- Seatbelt-confined writes and no external navigation.

That is the right tool for previewing the app under development. It is not a
general web-research or scraping engine. `fetch_url` covers static pages but
cannot run a JS-heavy SPA, click through a flow, or capture a rendered PDF.

Obscura fills exactly that gap:

| Capability | Current `browser` | `fetch_url` | Obscura |
|---|---|---|---|
| localhost / worktree page | yes | no | yes (explicit private-network flag) |
| external JS-heavy page | no | limited | yes |
| click/fill/type/select | yes (local only) | no | yes |
| screenshots / PDF | viewport only | no | full page / PDF |
| CDP clients | no | no | yes |
| MCP server | no | no | yes |
| network/console inspection | limited | no | yes |
| GUI/TCC required | yes (WKWebView) | no | no (headless) |
| current sandbox/lease model | yes | n/a | must be added |

The most valuable properties are headless operation (no logged-in GUI session,
no TCC) and CDP/MCP compatibility. A research Worker could scrape documentation
or validate a web flow without taking the single `gui` lease away from native
computer control.

## 3. Security and product constraints

Obscura must not become an unmediated external network path. Any integration
has to preserve the existing gates:

1. **Project policy first.** External navigation is opt-in per project and
   approval-classified. `ShellPolicy network = deny` must refuse Obscura
   entirely; it is not a local-only helper.
2. **No stealth by default.** Stealth/anti-detect mode can violate site terms
   and is not needed for documentation/research. Expose it only as an explicit
   operator choice, never as a default.
3. **SSRF stays on.** Keep private-network blocking on except for the existing
   localhost preview path, which should continue to use WKWebView rather than
   Obscura.
4. **One browser lease.** If Obscura is exposed through the `browser` tool, it
   must acquire the same daemon-wide `gui`/browser lease, with bounded FIFO wait
   and stale-generation failure. A separate unleased MCP server would bypass
   that invariant.
5. **Process confinement.** Launch the binary as a supervised child with its own
   process group, bounded output, timeout/kill escalation, a scratch HOME, and
   the Seatbelt write sandbox where possible. Network access is the point of the
   tool, so the policy must say so explicitly.
6. **Artifact caps.** Screenshots/PDFs are artifacts, not inline bytes. Reuse
   the existing 5 MiB binary artifact cap and content-addressed storage.
7. **No credential injection.** Do not feed provider keys, Git tokens, or
   Keychain secrets into the browser environment. A future profile/cookie
   feature needs its own ADR and secret-handling design.
8. **Trusted binary.** Like stdio ACP agents, the Obscura executable is trusted
   user software. It is not a network/credential sandbox, and its V8/BoringSSL
   supply chain is outside our control. Pin versions and verify release
   checksums if it is adopted.

## 4. Integration options

### Option A — configure `obscura mcp` as a project MCP server (fastest spike)

Add the stdio MCP server as an explicit project MCP entry and let research
Workers call `browser_navigate`, `browser_snapshot`, etc. through the existing
generic `mcp` tool.

Pros: no core code, immediate evaluation, reuses MCP transport and approvals.
Cons: bypasses the `browser` tool's localhost-only contract and the `gui` lease;
the MCP server is a trusted external process, not a sandboxed helper. This is
acceptable only as a clearly labeled, project-scoped, opt-in experiment.

### Option B — native `browser_engine = wkwebview | obscura` backend

Keep the `browser` tool API, add an engine selector, and launch `obscura serve`
under the daemon's process supervisor. Drive it over CDP for navigate/snapshot/
click/type/screenshot/PDF. Keep WKWebView as the default for localhost/worktree
pages; use Obscura only for approved external origins.

Pros: preserves one tool contract, one lease, one approval path, and artifact
handling. Cons: a real module (CDP client, process lifecycle, policy, tests,
ADR), not a config toggle.

### Option C — replace WKWebView

Not recommended. WKWebView is the correct engine for previewing the macOS app
and local pages, has native macOS integration, and already has the GUI lease and
TCC model. Obscura's independent rendering is a compatibility risk for that use
case.

## 5. Recommendation

**Useful, but not now.** Adopt Obscura as an optional external-web backend after
the cache-lossless round, not as part of it.

Concrete next step when scheduled:

1. Write an ADR for `browser_engine` and external-origin policy.
2. Implement Option B behind a project flag, default off.
3. Reuse the `gui` lease, approval risk classification, artifact caps, process
   supervision, and `ShellPolicy network` gate.
4. Add a live smoke test against a JS-heavy public page and a local fixture;
   verify screenshots/PDF and CDP behavior on macOS Apple Silicon.
5. Keep stealth off by default and document that rendering parity is not
   guaranteed.

## 6. Verification status

The upstream README, repository structure, release assets, and MCP/CDP surface
were reviewed. A local macOS binary smoke test was attempted, but the GitHub
release CDN throttled the 64 MiB asset to roughly 12 KB/s and the download timed
out; no Obscura binary was executed or added to the repo. Claims about memory,
startup, and rendering above are upstream claims, not independently measured
here.
