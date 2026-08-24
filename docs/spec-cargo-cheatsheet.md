# Spec — `cargo` command cheat-sheet

- **Status:** implemented (reference: Tauri+xterm.js build — `crates/cargohelp`
  (`sampa-cargohelp`), the `cargo_help` / `cargo_help_raw` bridge commands, and `showCargoHelp`
  in `src/main.ts`, rendered in the man panel).
- **Applies to:** showing a list of Cargo subcommands in the man panel when the typed command
  is `cargo`. Language-agnostic behavioral contract so any frontend behaves identically.

## 1. Purpose

`cargo` has many subcommands and a dense `man cargo`; discovering what it can do usually means
reading `cargo --help`. Like the `gh` cheat-sheet, Sampa surfaces that in the **man panel**:
when the command on the line is `cargo`, the panel shows an aligned command list instead of a
man page. Informational; nothing is composed or run.

## 2. Trigger

- Bound to the **man-panel shortcut** — `keybindings.toggle_man`, default **`Ctrl+Shift+M`** —
  **not** the enhance shortcut. The man panel detects the command from the tracked keystrokes
  (`tab.typed` / OSC 133), gated to real `$PATH` commands (`cargo` qualifies).
- When the detected command is **`cargo`**, the frontend routes to the cheat-sheet instead of
  `render_man`; any other command still shows its man page. Works on both the man panel's
  keystroke auto-update and the `Ctrl+Shift+M` toggle.
- **Drill-in by subcommand path** (shared with the gh cheat-sheet via `subcommandPath()`): the
  leading subcommand-like tokens after `cargo` (stopping at the first flag) select the level.
  Core cargo subcommands are mostly one level deep, so in practice `cargo` shows the command
  list and `cargo <sub>` is a leaf (see below).
- **Leaf fallback.** A path with no `Commands:` section is a **leaf** (`cargo build`,
  `cargo test`, `cargo run`, …). Rather than blank the panel, the frontend then shows that
  command's **own `--help`** (usage + options) as plain text. So every `cargo` subcommand
  surfaces something useful.
- Dismissed the same way as the man panel (its ✕ / toggling off).

## 3. Data

- The emulator runs `cargo <path…> --help` (read-only, **local — no network**, instant) with
  `LC_ALL=C` so the `Commands:` header is stable across locales. No shell; each subcommand
  token is a lone argv and flag-shaped args are dropped.
- The core parses the output into `CargoCommand { name, desc }` entries. cargo's help shape
  differs from gh's: a single non-indented `Commands:` header opens the section, and each
  indented row is `  <name>[, <alias>]   <description>` — the command (plus optional short
  alias, as printed, e.g. `build, b`), then a run of two-or-more spaces, then the description.
  The trailing `...  See all commands with --list` continuation row and the Options/Usage
  sections are ignored. **Fails safe:** no entries → `None` (a leaf, or non-cargo text).

## 4. Display

- Rendered into the man panel's `<pre>` as an aligned list: a `COMMANDS` header, then
  `  <name padded>  <description>` rows so names line up. Title: `cargo — commands` (or
  `cargo <path> — help` for a leaf's raw help). Text reaches the DOM via `textContent` only.

## 5. Architecture mapping

- **`crates/cargohelp` (`sampa-cargohelp`)** — headless parse core. `parse_cargo_help(output)
  -> Option<Vec<CargoCommand>>`, fail-safe-to-`None`, mirroring `sampa-ghhelp` and the other
  cores. Pure `std` + serde — **no shell, no Tauri**. Tested against a sample and real
  `cargo --help`.
- **Bridge** — `cargo_help(args)` runs `cargo <path…> --help` and returns the parsed entries;
  `cargo_help_raw(args)` returns the same command's raw help text (C0-stripped) for the leaf
  fallback. Both share a `run_cargo_help` → `run_help_cmd` helper with the gh pair.
- **Frontend** — `showCargoHelp` formats the aligned command list; on a leaf (no list) it
  falls back to `showCargoHelpRaw` (raw `cargo … — help`). `showMan` routes `cargo` to it;
  the subcommand path is read via the shared `subcommandPath()`.

## 6. Relationship to existing docs

Sibling of `spec-gh-cheatsheet.md` — the second decorator to overload the **man** shortcut
rather than the enhance shortcut, and it reuses that spec's drill-in + leaf-fallback shape and
the shared `run_help_cmd` / `subcommandPath()` plumbing. The same man-panel override could
later cover other subcommand-rich CLIs with weak man pages (docker, kubectl, npm) via their
`--help` output.
