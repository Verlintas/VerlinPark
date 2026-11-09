# ADR-0128: Lazy workspace browser, bounded content search, and readable tabs

- **Status:** Accepted
- **Date:** 2026-11-10
- **Owners:** macOS UI, Workspace RPC
- **Supersedes:** none
- **Related:** ADR-0126

## Context

The Preview file surface originally called `workspace.tree`, which walked the
entire worktree and serialized every entry before SwiftUI built a recursive
tree. Large repositories became slow to open and the UI rendered branches that
were never expanded. The Files filter was name-only, so finding a symbol or a
configuration value required leaving the app. File tabs also used an 8pt glyph
with an 18pt hit target, which is hard to click on a dense glass surface.

## Decision

### 1. List one directory level on demand

`workspace.list` resolves the worktree root plus a relative directory, rejects
absolute paths, `..`, symlinks, and `.git`, then reads one directory level with:

- directories first, case-insensitive name order;
- a default page of 500 and a hard page cap of 2,000;
- a 20,000-entry safety cap per list;
- `offset`, `total`, `has_more`, and `truncated`.

The Swift browser stores `workspaceDirectories[path]` and only fetches a child
when a folder is expanded. Collapsing a folder hides its state without deleting
the path’s cache; Refresh clears the cache. The root is loaded once per selected
session. This keeps memory and network proportional to what the user opened.

### 2. Name search and content search are separate modes

`workspace.search` remains a bounded name/path walk (default 300, hard cap
1,000). `workspace.grep` is a literal, bounded text search:

- 1–200 character query, no NUL;
- optional relative directory scope;
- case-insensitive by default, explicit case-sensitive option;
- skips `.git`, symlinks, files over 2 MiB, NUL-containing files, and non-UTF-8
  files;
- default 200 matches, hard cap 1,000, with `files_scanned` and `truncated`.

The query is never compiled as a regex. This avoids ReDoS and makes the latency
bound a function of the tree size and file cap, not a hostile pattern.

The Files panel exposes a compact Names/Contents switch. Contents results are
grouped by path, show the 1-based line and preview, and open the file at the
matching line. Both modes debounce 220ms and are cancellation-aware.

### 3. The editor has real line navigation

`WorkspaceCodeEditor` wraps `NSTextView` in an `NSScrollView` with a custom
line-number ruler. It preserves the SwiftUI text binding, exposes editable and
read-only states, supports find, and scrolls/selects a requested line using a
monotonically increasing reveal token so clicking the same match twice still
works. The ruler draws only visible line fragments and shows a logical line
number once per line, including when a line wraps.

### 4. Close affordances are deliberately generous

`VPCloseButton` uses a bold 10–12pt glyph, a visible circular surface, a
24–28pt hit target, hover growth, and an accessibility label. It replaces the
tiny tab-close and Collab-inspector close glyphs. Dense UI is not a reason to
make destructive actions hard to hit.

## Consequences

- Opening a large repository no longer walks or serializes the whole tree.
- Content search is literal, not semantic and not regex. Users who need regex
  can use a terminal; the UI stays predictable and safe.
- The daemon still reads up to 2 MiB per candidate file. The hard result cap
  stops the walk early, but a repository full of large text files can still do
  noticeable I/O; a future index can replace the scan without changing RPC.
- Each open folder costs one round trip. That is intentional: it keeps the
  first paint fast and lets the user stop before reading an enormous subtree.
- File editing still requires a clean SHA-256 conflict check and a path jail;
  this ADR does not change the write contract.

## Alternatives considered

- **Keep the full tree and filter client-side.** Rejected: the expensive part is
  the initial daemon walk and payload, not only SwiftUI rendering.
- **Persist a content index in SQLite.** Deferred: it requires invalidation,
  worktree change tracking, and a migration story. The bounded scan is a
  reliable first step.
- **Use regex by default.** Rejected for safety and predictability.
- **Use SwiftUI `TextEditor` and a scroll anchor.** Rejected: `TextEditor` does
  not expose scroll position or line ranges reliably on macOS 15.
