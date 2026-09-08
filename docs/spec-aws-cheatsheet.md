# Spec — `aws` command cheat-sheet

- **Status:** implemented (reference: Tauri+xterm.js build — `crates/awshelp` (`sampa-awshelp`),
  the `aws_help` / `aws_help_raw` bridge commands, and `showAwsHelp` in `src/main.ts`, rendered
  in the man panel).
- **Applies to:** showing the list of AWS CLI services/subcommands in the man panel when the
  typed command is `aws`. Language-agnostic behavioral contract so any frontend behaves
  identically.

## 1. Purpose

The AWS CLI has hundreds of services and a long man-page `help`; discovering what it can do
means scrolling the pager. Like the `gh`, `cargo`, `npm`, `docker`, `kubectl`, and `helm`
cheat-sheets, Sampa surfaces the name list in the **man panel**: when the command on the line is
`aws`, the panel shows the service/command names instead of a man page. Informational; nothing is
composed or run.

## 2. Trigger

- Bound to the **man-panel shortcut** — `keybindings.toggle_man`, default **`Ctrl+Shift+M`** —
  **not** the enhance shortcut. The man panel detects the command from the tracked keystrokes
  (`tab.typed` / OSC 133), gated to real `$PATH` commands (`aws` qualifies).
- When the detected command is **`aws`**, the frontend routes to the cheat-sheet instead of
  `render_man`; any other command still shows its man page. Works on both the man panel's
  keystroke auto-update and the `Ctrl+Shift+M` toggle.
- **Drill-in by subcommand path** (shared with the other cheat-sheets via `subcommandPath()`):
  the leading subcommand-like tokens after `aws` select the level. `aws` shows `AVAILABLE
  SERVICES`; a service drills into its `AVAILABLE COMMANDS` — `aws s3` shows s3's commands
  (`cp`, `ls`, `sync`, …), and so on.
- **Leaf fallback.** A path with no `AVAILABLE …` list is a **leaf** (`aws s3 ls`,
  `aws ec2 describe-instances`, …). Rather than blank the panel, the frontend then shows that
  command's **own help** (description + options + examples) as plain text. So every `aws`
  subcommand surfaces something useful.
- Dismissed the same way as the man panel (its ✕ / toggling off).

## 3. Data

- The emulator runs `aws <path…> help` — note aws uses a `help` **pseudo-subcommand**, not
  `--help` (which errors). It renders **bundled docs locally — no credentials, no network**. The
  pager is forced off (`AWS_PAGER`/`PAGER`/`MANPAGER`) so the groff-rendered page is dumped to
  stdout instead of blocking on an interactive pager. No shell; each subcommand token is a lone
  argv and flag-shaped args are dropped.
- aws differs from the other CLIs in three ways (hence a separate core):
  1. Its help is a **groff-rendered man page** — the text carries ANSI SGR sequences and/or
     backspace-overstrike bold/underline, which the core's `strip()` removes first.
  2. It lists names as **`o <name>` bullets** (one per line, blank lines between) under an
     `AVAILABLE SERVICES` / `AVAILABLE COMMANDS` header, with **no per-command descriptions** —
     so the core returns names only.
  3. The command form is `aws <path…> help` (the bridge's concern).
- `parse_aws_help(output)` strips the decoration, finds the `AVAILABLE …` section, and collects
  the bullet names. **Fails safe:** no names → `None` (a leaf, or non-aws text).

## 4. Display

- Rendered into the man panel's `<pre>`: a `COMMANDS` header, then the names laid out in
  **padded columns** sized to a nominal panel width (there are no descriptions to align). Title:
  `aws — commands` (or `aws <path> — help` for a leaf's raw help). Text reaches the DOM via
  `textContent` only.

## 5. Architecture mapping

- **`crates/awshelp` (`sampa-awshelp`)** — headless parse core. `strip(input) -> String` (ANSI
  CSI + backspace-overstrike + other C0 removal) and `parse_aws_help(output) ->
  Option<Vec<String>>`, fail-safe-to-`None`, mirroring `sampa-npmhelp`. Pure `std` — **no shell,
  no Tauri**. Tested against ANSI/overstrike samples and validated on real `aws help` /
  `aws s3 help`.
- **Bridge** — `aws_help(args)` runs `aws <path…> help` (via a dedicated `run_aws_help` that
  appends `help`, forces the pager off, and drops flag-shaped args) and returns the parsed
  names; `aws_help_raw(args)` returns the same command's `strip`-cleaned help text for the leaf
  fallback.
- **Frontend** — `showAwsHelp` lays the names out in columns; on a leaf (no list) it falls back
  to `showAwsHelpRaw` (raw `aws … — help`). `showMan` routes `aws` to it; the subcommand path is
  read via the shared `subcommandPath()`.

## 6. Relationship to existing docs

Seventh sibling of the CLI cheat-sheet specs (`spec-gh-cheatsheet.md`,
`spec-cargo-cheatsheet.md`, `spec-npm-cheatsheet.md`, `spec-docker-cheatsheet.md`,
`spec-kubectl-cheatsheet.md`, `spec-helm-cheatsheet.md`) — another decorator on the **man**
shortcut, reusing their drill-in + leaf-fallback shape and the shared `subcommandPath()`
plumbing. aws is the outlier: a groff man-page source (needing ANSI/overstrike stripping), a
bullet name-list (like npm, names-only), and a `help` pseudo-subcommand instead of `--help` — so
it has its own core and its own bridge runner rather than the shared `run_help_cmd`.
