# Spec — `npm` command cheat-sheet

- **Status:** implemented (reference: Tauri+xterm.js build — `crates/npmhelp` (`sampa-npmhelp`),
  the `npm_help` / `npm_help_raw` bridge commands, and `showNpmHelp` in `src/main.ts`, rendered
  in the man panel).
- **Applies to:** showing the list of npm subcommands in the man panel when the typed command
  is `npm`. Language-agnostic behavioral contract so any frontend behaves identically.

## 1. Purpose

`npm` has many subcommands and a paged `--help`; discovering what it can do means scrolling the
pager. Like the `gh` and `cargo` cheat-sheets, Sampa surfaces the command list in the **man
panel**: when the command on the line is `npm`, the panel shows the subcommand names instead of
a man page. Informational; nothing is composed or run.

## 2. Trigger

- Bound to the **man-panel shortcut** — `keybindings.toggle_man`, default **`Ctrl+Shift+M`** —
  **not** the enhance shortcut. The man panel detects the command from the tracked keystrokes
  (`tab.typed` / OSC 133), gated to real `$PATH` commands (`npm` qualifies).
- When the detected command is **`npm`**, the frontend routes to the cheat-sheet instead of
  `render_man`; any other command still shows its man page. Works on both the man panel's
  keystroke auto-update and the `Ctrl+Shift+M` toggle.
- **Drill-in by subcommand path** (shared with the gh/cargo cheat-sheets via `subcommandPath()`):
  the leading subcommand-like tokens after `npm` select the level. npm's top-level `--help` is
  the only one with a command list, so in practice `npm` shows the list and `npm <sub>` is a
  leaf (see below).
- **Leaf fallback.** A path with no `All commands:` list is a **leaf** (`npm install`,
  `npm run`, `npm test`, …). Rather than blank the panel, the frontend then shows that command's
  **own `--help`** (usage + options) as plain text. So every `npm` subcommand surfaces something.
- Dismissed the same way as the man panel (its ✕ / toggling off).

## 3. Data

- The emulator runs `npm <path…> --help` (read-only, **local — no network**, instant). No shell;
  each subcommand token is a lone argv and flag-shaped args are dropped.
- The core parses the output into command **names only** — npm's help shape differs from gh's
  and cargo's: it carries **no per-command descriptions**. An `All commands:` header (a
  non-indented line) opens the section, and the indented, comma-separated names that follow are
  collected. A blank line right after the header does **not** end the section (npm prints one) —
  the section ends at the next non-indented header. Each token is kept only if it looks like a
  command name (lowercase letters, digits, dashes — e.g. `install-ci-test`). **Fails safe:** no
  names → `None` (a leaf, an npm too old for this format, or non-npm text).

## 4. Display

- Rendered into the man panel's `<pre>`: a `COMMANDS` header, then the names laid out in
  **padded columns** sized to fit a nominal panel width (there are no descriptions to align, so
  a flat list would waste space). Title: `npm — commands` (or `npm <path> — help` for a leaf's
  raw help). Text reaches the DOM via `textContent` only.

## 5. Architecture mapping

- **`crates/npmhelp` (`sampa-npmhelp`)** — headless parse core. `parse_npm_help(output) ->
  Option<Vec<String>>`, fail-safe-to-`None`, mirroring `sampa-ghhelp` / `sampa-cargohelp`. Pure
  `std` + serde — **no shell, no Tauri**. Tested against a sample and real `npm --help`.
- **Bridge** — `npm_help(args)` runs `npm <path…> --help` and returns the parsed names;
  `npm_help_raw(args)` returns the same command's raw help text (C0-stripped) for the leaf
  fallback. Both share a `run_npm_help` → `run_help_cmd` helper with the gh/cargo pairs.
- **Frontend** — `showNpmHelp` lays the names out in columns; on a leaf (no list) it falls back
  to `showNpmHelpRaw` (raw `npm … — help`). `showMan` routes `npm` to it; the subcommand path is
  read via the shared `subcommandPath()`.

## 6. Relationship to existing docs

Third sibling of `spec-gh-cheatsheet.md` and `spec-cargo-cheatsheet.md` — another decorator on
the **man** shortcut, reusing their drill-in + leaf-fallback shape and the shared
`run_help_cmd` / `subcommandPath()` plumbing. The distinguishing detail is that npm exposes no
per-command descriptions, so the view is a names-only column layout. The same man-panel override
could still cover other subcommand-rich CLIs with weak man pages (docker, kubectl) via their
`--help` output.
