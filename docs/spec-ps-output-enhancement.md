# Spec — `ps(1)` output enhancement

- **Status (this native/Path C build):** the headless **core is available** — the origin
  repo's `sampa-ps-decorate` crate is wired in as a git-pinned dependency of
  `sampa-native` (per ADR 0002), and the `[enhance]` config flows through the shared
  `sampa-config`. **All three levels (1a quiet columns, 1b signal bars + live sort, 1c
  the two-pane inspector) ship end-to-end in the origin (Tauri) build**, and their headless
  logic — the parser, `bar`/`bars_for`, `classify`/`group_rows`, `parse_enrich` — is
  available here through the pinned dependency. The **native panel that renders the
  decorated model in the wgpu/glyphon UI is Path C's follow-up** — not built yet. This doc is mirrored from the origin repo as the behavioral contract; the
  §9 "App shell" / frontend references describe that build.
- **Applies to:** a TTY-only presentation layer that decorates unmodified `ps` output in
  place. Language-agnostic behavioral contract, so any frontend (webview or native Rust)
  behaves identically.
- **Architecture fit:** a new signature feature in the mold of the man panel / preview /
  palette — a headless parse-and-decorate **core** behind a thin frontend, honoring the
  same activation gate and the **insert-never-run** boundary (DESIGN.md §10.1 / §13).

## 1. Purpose & problem

A default `ps aux` on an idle desktop prints ~300 rows, the first ~55 of which are kernel
threads. In the reference capture (`ps -aux | more`), **all 55 read `0.0` in both `%CPU`
and `%MEM`** — 110 identical cells occupying the two most visually prominent columns —
while the one row that carries signal (PID 1, `RSS 19376`) is typographically identical to
54 rows that will never matter.

Three consequences compound:

1. The columns a developer scans first (`%CPU`, `%MEM`) carry no information on page one —
   the reason `aux` is typed instead of `-ef` is uniformly zero there.
2. The one live row looks like every dead one.
3. Paged through `more`, the user advances past every kernel thread to reach their own
   processes, with no indication of how far that is (`--More--` communicates only "not
   finished").

Secondary defects in the same output: `VSZ`/`RSS` are unitless kilobyte integers; `START`
renders as `ago11` under a non-English locale (neither a date nor a duration); `COMMAND`
truncates at the terminal edge with no ellipsis, so a cut path is indistinguishable from a
short one.

## 2. Goals & non-goals

| Goal | Non-goal |
|---|---|
| Make the rows that carry signal findable without reading digits. | Replacing `ps` with a bespoke command — users type what they know. |
| Put the user's own processes above the fold. | Hiding data — every fold is reversible and states its count. |
| Preserve **byte-identical** output for every non-interactive consumer. | Patching procps-ng or shipping a shell alias. |
| Degrade to plain text when colour or width is unavailable. | A general-purpose table renderer for arbitrary commands (later). |

## 3. Activation model (the gate)

The emulator recognises the output by matching the **first line** against known `ps`
header signatures (`USER PID %CPU %MEM VSZ RSS TTY STAT START TIME COMMAND` and the `-ef`
variant), then parses subsequent lines by the column offsets that header establishes. All
of the following must hold, or the stream passes through unmodified:

- **stdout is a TTY.** If stdout is a pipe or file the emulator never sees it and nothing
  changes — `ps aux | grep node` and every script keep working byte-for-byte.
- **The header matches exactly**, including field order. An unrecognised header → raw
  passthrough.
- **Terminal width** ≥ 80 cols for 1a, ≥ 100 for 1b, ≥ 120 for 1c. Below threshold, fall
  back one level.
- **A malformed row aborts enhancement for the whole block** and reprints the raw bytes.
  Partial parsing is never displayed.
- **Level** is set by `enhance.ps = off | quiet | bars | inspector` in config, defaulting
  to `quiet`.

This is deliberately the same shape as `sampa_preview::classify` (DESIGN.md §13): the gate
lives in the core, is authoritative there, and fails safe to raw output.

## 4. Direction 1a — Quiet columns (default)

Text and colour only. No box drawing, no interaction. Safe over SSH, safe in script
captures, safe on a serial console.

| Transform | Rule | Rationale |
|---|---|---|
| Zero elision | `value == 0.0` → `–` at ~45% foreground luminance | Exact zero is the absence of a measurement, not a measurement. |
| Kernel fold | `cmd` matches `/^\[.*\]$/` → collapse to one summary line | Restores the first screen to the processes the user asked about. |
| Size units | `RSS` kB → K/M/G, one decimal, right-aligned | Removes a division from every comparison. |
| Start-time locale | `START` → `"11 Aug"`, or `"14:22"` if today | Fixes the `ago11` class of locale artefact. |
| Truncation | `COMMAND` overflow → single-character ellipsis | A cut path becomes visibly cut. |

`VSZ` is **dropped from the default column set** — it is virtual address space, routinely
two orders of magnitude larger than resident memory, and answers no question asked at the
prompt. `ps aux --vsz` restores it.

The kernel-fold summary line states its own count and how to reveal it, e.g.
`… 47 kernel threads hidden (0.0% cpu, 0.0% mem) — ps aux --kernel to show`.

## 5. Direction 1b — Signal bars

Everything in 1a, plus **magnitude encoded as length**. The number stops being the unit of
comparison; the bar is. A 0.4% row is visibly shorter than a 12% row with no digit read —
which is what makes a 300-row table scannable rather than readable.

- Bars use block-element glyphs (U+2588 and the eighth-block series U+258F–U+2589) so the
  output **survives copy/paste as text**. Each bar is 8 cells wide, scaled against the
  **column maximum in the current result set**, never against 100 (a linear 0–100 scale
  renders every bar empty on an idle machine).
- **Denominators in the header**: `CPU 32.7% of 800%` tells the reader percentages are
  per-core-summed and that eight cores exist — without it a 700% row looks like a bug.
- **A position readout replaces `--More--`**: `10–18 of 62 · 47 kernel folded` with a
  proportional scrollbar.
- **Live sort**: `c` / `m` / `p` re-sort by CPU / memory / PID without re-running the
  command. Toggling the kernel fold reveals threads inline and updates the count, the page
  bar, and the row set together.

## 6. Direction 1c — Two-pane inspector

The emulator promotes the printed table to a **selectable region in place**. Scrollback
above and below stays ordinary text; only this command's block becomes interactive, and it
**freezes back to static text once the next prompt is submitted**.

- **Grouping.** Rows cluster by provenance — Dev, Browser, Desktop, System, Shell, Kernel —
  derived from the process tree and executable path, each with a CPU and RSS subtotal. A
  flat list of 300 becomes six things a developer recognises; the subtotal answers "what is
  my machine actually doing" at the group level.
- **Detail pane.** Selecting a row shows PPID, full state, thread count, TTY, absolute
  start time with elapsed duration, and the untruncated command line — the fields a
  developer otherwise reaches for a second command to get.

| Key | Action |
|---|---|
| `↑ ↓` / `j k` | Move selection |
| `← →` / `h l` | Collapse / expand a group |
| `/` | Incremental filter on command and user |
| `y` / `Y` | Copy PID / copy full command line |
| `k` | **Signal** — writes `kill <pid>` to the prompt, **never executes it** |
| `q` / `Esc` | Freeze the block back to static text |

The signal action is deliberately **not a confirmation dialog**. Composing the command at
the prompt leaves the user's own shell as the executor, keeps the action in history, and
preserves the mental model that the terminal runs what you type. This is the exact
insert-never-run boundary the command palette and AI suggester already honor (§13).

## 7. Colour

One ramp, applied to the CPU and MEM columns, thresholds **relative to the current result
set** rather than absolute. Colour is redundant with bar length (1b/1c) and with position
(1a) — no state is ever communicated by hue alone.

| Band | ANSI | Meaning |
|---|---|---|
| exact zero | bright black | no measurement |
| below 1% | default foreground | present, unremarkable |
| 1–5% | green | working normally |
| 5–10% | yellow | worth noticing |
| above 10% | red | the answer to the question |
| kernel row | blue | not yours |

## 8. Degradation

Each condition falls back one level and **never errors**:

- `NO_COLOR` set or a monochrome terminal → drop the ramp, keep zero elision, units, and
  folding.
- Width below a level's threshold → step down to the next level.
- A copy or `Ctrl-S` save of the buffer → yields the **enhanced text**, bars intact as
  block glyphs, no escape sequences.
- `enhance.ps = off` → restores the raw stream everywhere.

## 9. Architecture mapping

Proposed shape, consistent with the renderer-agnostic seam (DESIGN.md §4):

- **`crates/ps-decorate` (headless core, GUI-free)** — header signature match + column-offset
  parser + the 1a/1b transforms (zero elision, unit format, kernel fold, sort, bar
  computation). Pure and unit-testable against captured `ps` fixtures; no Tauri imports,
  same rule as `pty-core` / `preview`. The canvas prototype's `deco()` / `heat()` / `fmt()`
  logic is a working reference for these transforms.
- **Config** — an `[enhance]` block with `ps = "quiet"` (default), plus the width
  thresholds and the `--kernel` / `--vsz` escape-hatch flags recognised on the parsed
  command line.
- **Bridge** — the output pump already runs the OSC scanner per session (`crates/shellint`);
  the `ps` detector rides the same byte stream. Detection is on the emitted table block,
  gated on the §3 conditions.
- **Frontend** — renders 1a as decorated text; 1b/1c add the interactive overlay (sort keys,
  page readout, inspector panes) in `src/main.ts`, the only layer that knows about the
  webview. All parsing/decoration stays in the core.

## 10. Open questions (unresolved in draft 1)

1. Should the kernel fold apply to `root`-owned userspace daemons too, or only bracketed
   kernel threads? Folding daemons hides real memory consumers; not folding leaves ~40 rows
   of noise on a server.
2. Does the inspector re-poll `/proc` while it holds focus, or show a frozen snapshot? Live
   values contradict `ps` being point-in-time; a stale pane invites a wrong `kill`.
3. Group derivation needs a rule for containers and flatpaks, where the executable path does
   not indicate provenance.
4. Which command is enhanced next? `df`, `ls -l`, and `free` share the units and
   zero-noise problems and could reuse the same parse-and-decorate layer.

## 11. Relationship to existing docs

This is a **new signature-feature spec**, peer to `spec-command-palette-search.md` and
`spec-help-overlay.md`. It reuses three boundaries already established in DESIGN.md: the
TTY-gated activation model (like `preview`), the fail-safe-to-raw parse discipline, and the
insert-never-run rule for the `k` signal action. It should slot into ROADMAP.md as a
post-M5 signature feature, gated behind `enhance.ps` and defaulting to the SSH-safe `quiet`
level.
