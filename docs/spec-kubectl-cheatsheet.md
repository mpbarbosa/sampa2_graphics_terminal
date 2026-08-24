# Spec — `kubectl` command cheat-sheet

- **Status:** implemented (reference: Tauri+xterm.js build — `crates/kubectlhelp`
  (`sampa-kubectlhelp`), the `kubectl_help` / `kubectl_help_raw` bridge commands, and
  `showKubectlHelp` in `src/main.ts`, rendered in the man panel).
- **Applies to:** showing a grouped list of kubectl subcommands in the man panel when the typed
  command is `kubectl`. Language-agnostic behavioral contract so any frontend behaves identically.

## 1. Purpose

`kubectl` has dozens of subcommands spread across many groups and a long `--help`; discovering
what it can do means scrolling. Like the `gh`, `cargo`, `npm`, and `docker` cheat-sheets, Sampa
surfaces the command list in the **man panel**: when the command on the line is `kubectl`, the
panel shows a grouped, aligned cheat-sheet instead of a man page. Informational; nothing is
composed or run.

## 2. Trigger

- Bound to the **man-panel shortcut** — `keybindings.toggle_man`, default **`Ctrl+Shift+M`** —
  **not** the enhance shortcut. The man panel detects the command from the tracked keystrokes
  (`tab.typed` / OSC 133), gated to real `$PATH` commands (`kubectl` qualifies).
- When the detected command is **`kubectl`**, the frontend routes to the cheat-sheet instead of
  `render_man`; any other command still shows its man page. Works on both the man panel's
  keystroke auto-update and the `Ctrl+Shift+M` toggle.
- **Drill-in by subcommand path** (shared with the other cheat-sheets via `subcommandPath()`):
  the leading subcommand-like tokens after `kubectl` select the level. kubectl's **cobra**
  subcommands have their own `Available Commands:` section, so `kubectl config` drills into
  config's commands, `kubectl rollout` into rollout's, and so on.
- **Leaf fallback.** A path with no command section is a **leaf** (`kubectl get`, `kubectl apply`,
  `kubectl describe`, …). Rather than blank the panel, the frontend then shows that command's
  **own `--help`** (usage + options + examples) as plain text. So every `kubectl` subcommand
  surfaces something useful.
- Dismissed the same way as the man panel (its ✕ / toggling off).

## 3. Data

- The emulator runs `kubectl <path…> --help` (read-only; **local — no cluster contact, no
  network**, instant). No shell; each subcommand token is a lone argv and flag-shaped args are
  dropped.
- The core parses the output into `KubectlCommand { name, desc, section }` entries. kubectl's
  help is docker-shaped — grouped sections with `  <name>  <description>` rows — but its section
  headers carry qualifiers, so they don't end in exactly `Commands:`: the top-level help uses
  `Basic Commands (Beginner):`, `Deploy Commands:`, `Other Commands:`, … and its subcommands
  (via cobra) use `Available Commands:`. The header rule is therefore: a non-indented line that
  ends in `:` and mentions "command". The stored `section` is the header minus its trailing
  colon. Non-command sections (Usage, Options, Examples) end the current section and are ignored.
  **Fails safe:** no entries → `None` (a leaf, or non-kubectl text).

## 4. Display

- Rendered into the man panel's `<pre>` as an aligned cheat-sheet: each section header (upper-
  cased), then `  <name padded>  <description>` rows so names line up. Title: `kubectl —
  commands` (or `kubectl <path> — help` for a leaf's raw help). Text reaches the DOM via
  `textContent` only.

## 5. Architecture mapping

- **`crates/kubectlhelp` (`sampa-kubectlhelp`)** — headless parse core.
  `parse_kubectl_help(output) -> Option<Vec<KubectlCommand>>`, fail-safe-to-`None`, mirroring
  `sampa-dockerhelp` and the other help cores. Pure `std` + serde — **no shell, no Tauri**.
  Tested against representative samples (the top-level grouped format and a cobra
  `Available Commands:` subcommand); kubectl is not installed on the dev machine, so validation
  is against samples rather than live output.
- **Bridge** — `kubectl_help(args)` runs `kubectl <path…> --help` and returns the parsed
  entries; `kubectl_help_raw(args)` returns the same command's raw help text (C0-stripped) for
  the leaf fallback. Both share a `run_kubectl_help` → `run_help_cmd` helper with the
  gh/cargo/npm/docker pairs.
- **Frontend** — `showKubectlHelp` formats the grouped/aligned command list; on a leaf (no list)
  it falls back to `showKubectlHelpRaw` (raw `kubectl … — help`). `showMan` routes `kubectl` to
  it; the subcommand path is read via the shared `subcommandPath()`.

## 6. Relationship to existing docs

Fifth sibling of `spec-gh-cheatsheet.md`, `spec-cargo-cheatsheet.md`, `spec-npm-cheatsheet.md`,
and `spec-docker-cheatsheet.md` — another decorator on the **man** shortcut, reusing their
drill-in + leaf-fallback shape and the shared `run_help_cmd` / `subcommandPath()` plumbing.
kubectl is the grouped variant closest to docker, differing only in the qualified section
headers (`Basic Commands (Beginner):`, `Available Commands:`) that motivate the "contains
command" header rule.
