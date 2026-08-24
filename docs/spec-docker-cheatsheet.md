# Spec — `docker` command cheat-sheet

- **Status:** implemented (reference: Tauri+xterm.js build — `crates/dockerhelp`
  (`sampa-dockerhelp`), the `docker_help` / `docker_help_raw` bridge commands, and
  `showDockerHelp` in `src/main.ts`, rendered in the man panel).
- **Applies to:** showing a grouped list of Docker subcommands in the man panel when the typed
  command is `docker`. Language-agnostic behavioral contract so any frontend behaves identically.

## 1. Purpose

`docker` has dozens of subcommands spread across several groups and a long `--help`; discovering
what it can do means scrolling. Like the `gh`, `cargo`, and `npm` cheat-sheets, Sampa surfaces
the command list in the **man panel**: when the command on the line is `docker`, the panel shows
a grouped, aligned cheat-sheet instead of a man page. Informational; nothing is composed or run.

## 2. Trigger

- Bound to the **man-panel shortcut** — `keybindings.toggle_man`, default **`Ctrl+Shift+M`** —
  **not** the enhance shortcut. The man panel detects the command from the tracked keystrokes
  (`tab.typed` / OSC 133), gated to real `$PATH` commands (`docker` qualifies).
- When the detected command is **`docker`**, the frontend routes to the cheat-sheet instead of
  `render_man`; any other command still shows its man page. Works on both the man panel's
  keystroke auto-update and the `Ctrl+Shift+M` toggle.
- **Drill-in by subcommand path** (shared with the other cheat-sheets via `subcommandPath()`):
  the leading subcommand-like tokens after `docker` select the level. docker's **management**
  subcommands have their own `Commands:` section, so `docker container` drills into container's
  commands, `docker image` into image's, and so on.
- **Leaf fallback.** A path with no `… Commands:` section is a **leaf** (`docker run`,
  `docker ps`, `docker build`, …). Rather than blank the panel, the frontend then shows that
  command's **own `--help`** (usage + options) as plain text. So every `docker` subcommand
  surfaces something useful.
- Dismissed the same way as the man panel (its ✕ / toggling off).

## 3. Data

- The emulator runs `docker <path…> --help` (read-only; **pure CLI — no daemon contact, no
  network**, instant). No shell; each subcommand token is a lone argv and flag-shaped args are
  dropped.
- The core parses the output into `DockerCommand { name, desc, section }` entries. docker's help
  shape blends gh's and cargo's: it **groups** commands under several `… Commands:` headers
  (`Common Commands:`, `Management Commands:`, `Swarm Commands:`, `Commands:`) like gh, but each
  row is `  <name>  <description>` (space-separated) like cargo. The stored `section` is the
  header minus its trailing colon. Plugin commands print a trailing `*` (`buildx*`, `compose*`)
  — the star is stripped from the stored name. Non-command sections (Global Options, Usage) end
  the current section and are ignored. **Fails safe:** no entries → `None` (a leaf, or non-docker
  text).

## 4. Display

- Rendered into the man panel's `<pre>` as an aligned cheat-sheet: each section header (upper-
  cased), then `  <name padded>  <description>` rows so names line up. Title: `docker —
  commands` (or `docker <path> — help` for a leaf's raw help). Text reaches the DOM via
  `textContent` only.

## 5. Architecture mapping

- **`crates/dockerhelp` (`sampa-dockerhelp`)** — headless parse core. `parse_docker_help(output)
  -> Option<Vec<DockerCommand>>`, fail-safe-to-`None`, mirroring `sampa-ghhelp` /
  `sampa-cargohelp` / `sampa-npmhelp`. Pure `std` + serde — **no shell, no Tauri**. Tested
  against a sample and real `docker --help`.
- **Bridge** — `docker_help(args)` runs `docker <path…> --help` and returns the parsed entries;
  `docker_help_raw(args)` returns the same command's raw help text (C0-stripped) for the leaf
  fallback. Both share a `run_docker_help` → `run_help_cmd` helper with the gh/cargo/npm pairs.
- **Frontend** — `showDockerHelp` formats the grouped/aligned command list; on a leaf (no list)
  it falls back to `showDockerHelpRaw` (raw `docker … — help`). `showMan` routes `docker` to it;
  the subcommand path is read via the shared `subcommandPath()`.

## 6. Relationship to existing docs

Fourth sibling of `spec-gh-cheatsheet.md`, `spec-cargo-cheatsheet.md`, and
`spec-npm-cheatsheet.md` — another decorator on the **man** shortcut, reusing their drill-in +
leaf-fallback shape and the shared `run_help_cmd` / `subcommandPath()` plumbing. docker is the
grouped variant (multiple `… Commands:` sections) with cargo-style space-separated rows. The
same man-panel override could still cover other subcommand-rich CLIs with weak man pages
(kubectl, …) via their `--help` output.
