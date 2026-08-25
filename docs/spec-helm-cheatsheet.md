# Spec — `helm` command cheat-sheet

- **Status:** implemented (reference: Tauri+xterm.js build — `crates/helmhelp` (`sampa-helmhelp`),
  the `helm_help` / `helm_help_raw` bridge commands, and `showHelmHelp` in `src/main.ts`,
  rendered in the man panel).
- **Applies to:** showing the list of helm subcommands in the man panel when the typed command
  is `helm`. Language-agnostic behavioral contract so any frontend behaves identically.

## 1. Purpose

`helm` (the Kubernetes package manager) has many subcommands; discovering what it can do means
reading `helm --help`. Like the `gh`, `cargo`, `npm`, `docker`, and `kubectl` cheat-sheets, Sampa
surfaces the command list in the **man panel**: when the command on the line is `helm`, the panel
shows an aligned cheat-sheet instead of a man page. Informational; nothing is composed or run.

## 2. Trigger

- Bound to the **man-panel shortcut** — `keybindings.toggle_man`, default **`Ctrl+Shift+M`** —
  **not** the enhance shortcut. The man panel detects the command from the tracked keystrokes
  (`tab.typed` / OSC 133), gated to real `$PATH` commands (`helm` qualifies).
- When the detected command is **`helm`**, the frontend routes to the cheat-sheet instead of
  `render_man`; any other command still shows its man page. Works on both the man panel's
  keystroke auto-update and the `Ctrl+Shift+M` toggle.
- **Drill-in by subcommand path** (shared with the other cheat-sheets via `subcommandPath()`):
  the leading subcommand-like tokens after `helm` select the level. helm is pure cobra, so its
  container subcommands have their own `Available Commands:` — `helm repo` drills into repo's
  commands, `helm get` into get's, and so on.
- **Leaf fallback.** A path with no command section is a **leaf** (`helm install`, `helm upgrade`,
  `helm template`, …). Rather than blank the panel, the frontend then shows that command's **own
  `--help`** (usage + flags) as plain text. So every `helm` subcommand surfaces something useful.
- Dismissed the same way as the man panel (its ✕ / toggling off).

## 3. Data

- The emulator runs `helm <path…> --help` (read-only; **local — no cluster contact, no
  network**, instant). No shell; each subcommand token is a lone argv and flag-shaped args are
  dropped.
- The core parses the output into `HelmCommand { name, desc, section }` entries. helm is a
  pure-cobra CLI: its help — at the top level and for every subcommand — lists commands under a
  single `Available Commands:` header with `  <name>  <description>` rows (the same shape as a
  kubectl subcommand). The header rule is a non-indented line ending in `:` that mentions
  "command"; the stored `section` is the header minus its trailing colon. Non-command sections
  (Flags, Usage, Examples) end the current section and are ignored. **Fails safe:** no entries →
  `None` (a leaf, or non-helm text).

## 4. Display

- Rendered into the man panel's `<pre>` as an aligned cheat-sheet: each section header (upper-
  cased), then `  <name padded>  <description>` rows so names line up. Title: `helm — commands`
  (or `helm <path> — help` for a leaf's raw help). Text reaches the DOM via `textContent` only.

## 5. Architecture mapping

- **`crates/helmhelp` (`sampa-helmhelp`)** — headless parse core. `parse_helm_help(output) ->
  Option<Vec<HelmCommand>>`, fail-safe-to-`None`, mirroring `sampa-kubectlhelp` and the other
  help cores. Pure `std` + serde — **no shell, no Tauri**. Tested against representative samples
  (top-level and a subcommand's `Available Commands:`); helm is not installed on the dev machine,
  so validation is against samples rather than live output.
- **Bridge** — `helm_help(args)` runs `helm <path…> --help` and returns the parsed entries;
  `helm_help_raw(args)` returns the same command's raw help text (C0-stripped) for the leaf
  fallback. Both share a `run_helm_help` → `run_help_cmd` helper with the gh/cargo/npm/docker/
  kubectl pairs.
- **Frontend** — `showHelmHelp` formats the grouped/aligned command list; on a leaf (no list) it
  falls back to `showHelmHelpRaw` (raw `helm … — help`). `showMan` routes `helm` to it; the
  subcommand path is read via the shared `subcommandPath()`.

## 6. Relationship to existing docs

Sixth sibling of `spec-gh-cheatsheet.md`, `spec-cargo-cheatsheet.md`, `spec-npm-cheatsheet.md`,
`spec-docker-cheatsheet.md`, and `spec-kubectl-cheatsheet.md` — another decorator on the **man**
shortcut, reusing their drill-in + leaf-fallback shape and the shared `run_help_cmd` /
`subcommandPath()` plumbing. helm is the pure-cobra variant: a single `Available Commands:`
section at every level, closest to kubectl's subcommand shape.
