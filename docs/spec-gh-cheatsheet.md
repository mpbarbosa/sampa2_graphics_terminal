# Spec — `gh` command cheat-sheet

- **Status (this native/Path C build):** **implemented.** Typing `gh` and pressing
  `Ctrl+Shift+M` opens the man panel on a **cheat-sheet** instead of a man page: the emulator
  runs a read-only `gh --help`, parses it with `sampa_ghhelp::parse_gh_help`, and renders each
  `<SECTION> COMMANDS` header followed by name-aligned `  <name>  <desc>` rows
  (`gh_cheatsheet_lines`), titled `gh — commands`, scrollable like any man page. Any other
  command still shows its man page. Fails safe: `gh` missing or no commands → a one-line
  message. (The native build routes on the `Ctrl+Shift+M` toggle; it has no live man
  keystroke auto-update.) Mirrored below as the language-agnostic contract; the `showGhHelp`
  file references describe the reference (Tauri) build.
- **Applies to:** showing a grouped list of GitHub CLI (`gh`) commands in the man panel when
  the typed command is `gh`. Language-agnostic behavioral contract so any frontend behaves
  identically.

## 1. Purpose

`gh` has dozens of subcommands and no useful `man gh`; discovering what it can do means
reading `gh --help`. Sampa surfaces that in the **man panel**: when the command on the line
is `gh`, the panel shows a grouped, aligned cheat-sheet of gh's commands instead of a man
page. Informational; nothing is composed or run.

## 2. Trigger

- Bound to the **man-panel shortcut** — `keybindings.toggle_man`, default **`Ctrl+Shift+M`**
  — **not** the enhance shortcut (`Ctrl+Shift+E`). The man panel detects the command from the
  tracked keystrokes (`tab.typed` / OSC 133), gated to real `$PATH` commands (`gh` qualifies).
- When the detected command is **`gh`**, the frontend routes to the cheat-sheet instead of
  `render_man`; any other command still shows its man page. This works on both the man
  panel's keystroke auto-update and the `Ctrl+Shift+M` toggle.
- **Drill-in by subcommand.** The frontend reads the **subcommand path** from the typed line
  — the leading subcommand-like tokens after `gh` (stopping at the first flag) — so `gh`
  shows the top-level commands and **`gh repo` shows `gh repo`'s commands** (`gh repo --help`).
  A path that has no `… COMMANDS` sections (a leaf like `gh repo view`) parses to nothing and
  the panel simply doesn't show a list.
- Dismissed the same way as the man panel (its ✕ / toggling off).

## 3. Data

- The emulator runs `gh --help` (read-only, **local — no network**, instant). No shell.
- The core parses the output into `GhCommand { name, desc, section }` entries: under each
  non-indented ALL-CAPS `… COMMANDS` header (CORE, GITHUB ACTIONS, ALIAS, ADDITIONAL, …),
  the indented `name: description` lines. Non-command sections (USAGE, FLAGS, LEARN MORE)
  end the current section and are ignored. **Fails safe:** no entries → nothing (the man
  panel hides / falls through).

## 4. Display

- Rendered into the man panel's `<pre>` as an aligned cheat-sheet: each `… COMMANDS` section
  header, then `  <name padded>  <description>` rows, so names line up. Title: `gh —
  commands`. Text reaches the DOM via `textContent` only.

## 5. Architecture mapping

- **`crates/ghhelp` (`sampa-ghhelp`)** — headless parse core. `parse_gh_help(output) ->
  Vec<GhCommand>`, fail-safe-to-`None`, mirroring the other decorator cores. Pure `std` +
  serde — **no shell, no Tauri**. Tested against sample and real `gh --help`.
- **Bridge** — `gh_help()` runs `gh --help` and returns the parsed entries.
- **Frontend** — `showGhHelp` formats the grouped/aligned text and shows it in the man panel;
  `showMan` routes `gh` to it.

## 6. Relationship to existing docs

Sits alongside the man panel (`spec` covered in DESIGN.md §10.2) and the `Ctrl+Shift+E`
decorator family (`spec-ps-output-enhancement.md`, `-cd-tree-picker`, `-du-treemap`,
`-free-gauge`, `-ping-chart`, `-df-gauge`, `-load-gauge`) — but it is the first decorator to
overload the **man** shortcut rather than the enhance shortcut. The same man-panel override
could later cover other subcommand-rich CLIs with weak man pages (docker, cargo, kubectl) via
their `--help` output.
