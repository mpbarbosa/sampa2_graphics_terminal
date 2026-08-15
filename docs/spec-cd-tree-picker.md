# Spec — `cd` directory tree picker

- **Status (this native/Path C build):** mirrored from the `sampa_graphics_terminal`
  reference build, where it is **implemented** (`crates/fsnav`, the `list_dirs` bridge, the
  `#cdtree` overlay). The headless `sampa-fsnav` core is **not yet wired** into `sampa-native`
  (a git-pinned-dep follow-up, as done for `ps-decorate`/`ai`), and there is no native tree
  UI yet. Carried here as the behavioral contract; the file/keybinding references describe
  the reference (Tauri) build.
- **Applies to:** choosing a `cd` argument from a navigable directory tree, opened from the
  keyboard. Language-agnostic behavioral contract so any frontend (webview or native Rust)
  behaves identically.

## 1. Purpose

Typing a `cd` path blind — remembering names, tab-completing one segment at a time — is
friction the terminal can remove. When the user has typed `cd` and asks for it, Sampa shows
a **directory tree rooted at the current working directory**; the user navigates it and
picks a directory, and its path is composed as the `cd` argument at the prompt. As
everywhere else in Sampa, the result is **inserted, never executed** (DESIGN.md §10.1 / §13):
the user's own shell runs it when they press Enter.

## 2. Trigger

- Bound to the **same shortcut as the `ps` output decorator** — `keybindings.enhance_ps`,
  default **`Ctrl+Shift+E`** — which is *overloaded*: the frontend dispatches on the typed
  command. If the first token of the tracked command line (`tab.typed`, keystroke-derived —
  not grid-scraped) is **`cd`**, the tree picker opens; otherwise the `ps` decorator runs.
- The command line source is the keystroke tracker, so autosuggestions/redraws never affect
  detection (the man panel / preview use the same source).
- The picker is a modal overlay; **Esc**, its **✕**, or a **backdrop click** dismiss it,
  returning focus to the terminal.

## 3. The tree

- **Rooted at the session cwd** (obtained the same way the cwd-aware features get it: OSC 7
  if present, else `/proc/<pid>/cwd`). Navigation descends *into* subdirectories; there is
  no traversal above the root (it is "the current folder" tree).
- **Lazily expanded** — only the root's immediate subdirectories are listed up front; a
  node's children are fetched the first time it is expanded. This keeps a deep or wide tree
  cheap and responsive.
- **Directories only.** Files are omitted (a `cd` target is always a directory). Symlinks
  that point at directories are followed so they are navigable. Entries are sorted
  case-insensitively by name; hidden (`.`-prefixed) directories are included.
- Best-effort: an unreadable or missing directory contributes nothing (no error dialog) —
  the node simply has no children.

## 4. Navigation & selection

| Key | Action |
|---|---|
| `↑` `↓` / `j` `k` | Move the selection among visible rows |
| `→` / `l` | Expand the selected directory (loads its children on first expand) |
| `←` / `h` | Collapse the selected directory |
| `Enter` / double-click | **Choose** — compose `cd <path>` at the prompt and close |
| `Esc` / ✕ / backdrop | Cancel |

A single click selects a row and toggles its expansion; double-click chooses it.

## 5. Choosing — insert, never run

On choose, the picker composes the command at the prompt and **never appends a newline**:

- The path is expressed **relative to the root cwd** (e.g. `src/app`) for a compact
  argument; a path not under the root falls back to absolute.
- It is **shell-quoted** when it contains whitespace or shell-significant characters
  (single-quoted, with embedded single quotes escaped).
- The frontend **replaces the current line**: it erases the tracked keystrokes (so a partial
  `cd foo` the user had typed is cleared) and writes `cd <path> `.

The user reviews the composed command and runs it themselves. Nothing is executed by the
picker — the same insert-never-run boundary the command palette and AI suggester honor.

## 6. Architecture mapping

- **`crates/fsnav` (`sampa-fsnav`)** — headless, read-only navigation core. `list_subdirs(path)`
  returns the immediate subdirectories; `relativize(root, child)` yields the compact
  argument. Pure `std::fs` behind serde-able types — **no shell, no Tauri** (same rule as
  `pty-core`/`config`/`preview`/`ps-decorate`). Tested against temp-dir fixtures.
- **Bridge** — `list_dirs(path)` wraps the core (read-only; a path from the frontend is used
  only for `read_dir`, never shelled, so nothing is interpolated into a shell). Called once
  on the cwd and again per expanded node.
- **Frontend** — the `#cdtree` overlay owns the tree state (lazy children cache, expanded
  flags, the flattened visible list for `↑↓`) and the compose-and-insert step. Directory
  names reach the DOM via `textContent` only (never `innerHTML`).

## 7. Relationship to existing docs

Peer to `spec-command-palette-search.md`, `spec-help-overlay.md`, and
`spec-ps-output-enhancement.md`. It reuses two established boundaries: the **keystroke-derived
command detection** (man/preview/explain) and the **insert-never-run** rule for composing at
the prompt. It shares the `Ctrl+Shift+E` shortcut with the `ps` decorator by dispatching on
the typed command; giving it a dedicated keybinding later is a one-line change.
