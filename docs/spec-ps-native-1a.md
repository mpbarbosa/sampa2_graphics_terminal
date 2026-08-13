# Spec — native `ps` enhancement, level 1a (Path C)

> **Scope.** The native (winit/wgpu) implementation of **level 1a** ("quiet columns") from
> [spec-ps-output-enhancement.md](spec-ps-output-enhancement.md). The behavioral contract,
> the gate, and the transforms live in that doc; this spec is only the **native render
> slice** — how the wgpu build triggers, scrapes, decorates, and draws it. Levels **1b**
> (bars + live sort) and **1c** (inspector) are explicitly out of scope here (§8).

- **Status:** implemented. `Action::EnhancePs` on `Ctrl+Shift+D` scrapes the buffer and
  renders the decorated `Quiet` model as a bottom overlay panel. The headless core
  (`sampa-ps-decorate`, pinned in `sampa-native/Cargo.toml` per ADR 0002) provides the
  parse + decorate; the native side adds the trigger, scrape, and render.
- **Reuses:** the overlay-panel machinery of the man/preview panels (`PanelView` + rich
  body spans), the keybinding `Action`/`ACTIONS` table, and — for precise block bounds —
  the OSC 133 scanner recovered in the preview work.

## 1. What 1a is (recap)

Decorate the last `ps aux` output: fold bracketed kernel threads into one summary line,
elide exact-zero `%CPU`/`%MEM` to `–`, format `RSS` as `K/M/G`, drop `VSZ`, and colour the
`%CPU`/`%MEM` columns by band. **Text and colour only — no interaction, no bars.** It is
read-only: the enhancement never runs anything and never mutates the shell or scrollback.

## 2. Trigger model (keypress, not auto-detect)

This slice is **manual**: the user runs `ps aux`, then presses a key to decorate the last
output. This sidesteps live block-boundary detection during output and matches the shared
config's intent (`keybindings.enhance_ps` — *"decorate the last ps output"*).

- New `Action::EnhancePs`, default chord **`Ctrl+Shift+D`** ("decorate").
  - **Not `Ctrl+Shift+E`** — the shared `sampa-config` defaults `enhance_ps` there, but that
    chord is already the native build's `toggle_preview`. The native `ACTIONS` default must
    differ; document the clash so a user's `[keybindings] enhance_ps` override is a
    deliberate choice.
- The overlay is modal (captures keys while open), like man/preview; **Esc** or re-pressing
  the chord closes it and the terminal returns to normal.

## 3. Pipeline

On the trigger:

```
1. level = ps_decorate::resolve_level(cfg.enhance.ps, cols, &thresholds)
      → Off  ⇒ do nothing (raw stays; optional one-line "ps enhancement off / too narrow")
2. text  = scrape_last_output()                         // §4
3. quiet = ps_decorate::decorate_scrollback(&text)      // Option<Quiet>
      → None ⇒ do nothing (last output wasn't a `ps aux` block — fail safe to raw)
4. render the Quiet model as an overlay panel           // §5
```

`resolve_level` and `decorate_scrollback` are pure core calls; steps 2 and 4 are the only
new native code. `WidthThresholds` come from `[enhance]` (defaults `min_width = 80`,
`min_width_bars = 100`, `min_width_inspector = 120`); for 1a only `min_width` gates.

## 4. Scraping the last output

`decorate_scrollback` tolerates leading/trailing noise (the prompt, the typed command, the
next prompt) and locates the `USER PID %CPU …` header itself — so the scrape just has to
**contain** the block.

- **MVP:** scrape the terminal text from the top of scrollback history down to the cursor
  (or a bounded tail, e.g. the last 500 logical lines — `ps aux` is ~300 rows, so the
  header may be well above the visible screen; a visible-screen-only scrape would miss it).
  Read it from the alacritty grid including history lines, joined by `\n` (the existing
  `Snapshot::to_text` reads only the visible rows — this slice adds a history-inclusive
  read).
- **Last-block selection:** `decorate_scrollback` reads top-down from the *first* header it
  sees, so the scrape is trimmed to the last `ps aux` header (scan bottom-up for the most
  recent `HeaderKind::Aux`) before decoding — otherwise an older `ps` in history would win.
- **tty-truncation repair (found in implementation):** `ps aux` truncates COMMAND to the
  terminal width when writing to a tty, so a long kernel thread (`[kworker/R-rcu_gp]`) loses
  its closing `]` in the scrape — and the core's `is_kernel_command` needs both brackets, so
  hundreds of kernel threads would stay unfolded (at width 80, only ~1 in 4 keep their `]`).
  `repair_truncated_kernel` restores the `]` the terminal cut on any row whose COMMAND opens
  with `[`, so folding is correct (217 vs 57 on a test box). Kept native-side (no core
  re-pin); safe because only a bracketed COMMAND triggers it and folded rows aren't drawn.
- **Enhancement (later, cheap):** bound the scrape with **OSC 133** — the last `133;C`
  (command output start) to `133;D`/cursor delimits exactly the last command's output, so
  the decorator sees only that block and never an earlier `ps`. The recovered OSC 133
  scanner already tracks the prompt markers; extend it to record the output-start line.

## 5. Rendering (overlay panel, this slice)

Draw the decorated `Quiet` as a **bottom overlay panel** — the same primitive as
man/preview (`PanelView { title, body, body_spans }`), *not* an in-place replacement of the
scrollback cells (that is the follow-up in §9). Rationale: the panel path is proven and
low-risk; it delivers the 1a value (findable signal, folded kernel noise, real units) while
the harder in-place-region rendering is staged separately.

**Header line:** `ps aux — <N> processes · <K> kernel threads folded   ·  Esc`.

**Columns** (`ps_decorate::QUIET_COLUMNS` = `PID USER %CPU %MEM RSS START COMMAND`): render
as an aligned monospace table (right-align `PID`/`%CPU`/`%MEM`/`RSS`, left-align the rest),
one `QuietRow` per line, `COMMAND` truncated with a single-char ellipsis to the panel width.

**Per-cell colour** — the core leaves colouring to the frontend and carries `cpu_val` /
`mem_val` on each `QuietRow` for exactly this. Apply the spec §7 band to the `%CPU`/`%MEM`
cells (redundant with the elided `–`, never hue-alone):

| Band (value) | Colour |
|---|---|
| `– ` (elided zero) | muted / bright-black |
| `< 1%` | default fg |
| `1–5%` | green |
| `5–10%` | yellow |
| `> 10%` | red |

Add a `fn heat_band(v: f32) -> [u8;3]` helper (the core intentionally omits it). The other
cells render in the default foreground; the folded-kernel summary
(`Quiet::kernel_summary()`) renders as a final muted row.

**Degradation** (spec §8): `NO_COLOR` or a mono theme → drop the band, keep elision/units/
fold. Panel narrower than the columns → truncate `COMMAND` harder; never error.

## 6. Config & keybinding

- `[enhance] ps = "quiet"` (default) already parses via `sampa-config`. For 1a, `quiet`,
  `bars`, and `inspector` all render as the quiet panel (bars/inspector are later slices);
  `off` disables the trigger.
- Add `(Action::EnhancePs, "enhance_ps", "Enhance last ps output", "Ctrl+Shift+D")` to the
  native `ACTIONS` table; the existing `[keybindings]` hand-parser picks up overrides.

## 7. State & lifecycle

Add to `App`, mirroring the man panel: `ps_on: bool`, `ps_quiet: Option<Quiet>`. On trigger,
run the pipeline; on success set `ps_on = true` + store the model; Esc/toggle clears it. The
render path adds a `ps_on` branch to the panel selection (mutually exclusive with man/
preview/AI, like the others). No background thread — the scrape + decorate are synchronous
and fast (string parsing of a few hundred lines).

## 8. Non-goals (this slice)

- **In-place** decoration of the scrollback region — this slice renders a panel; replacing
  the actual `ps` output cells and freezing back is §9 follow-up work.
- **1b** signal bars, denominators, live `c`/`m`/`p` sort, page readout.
- **1c** grouping, two-pane inspector, `/` filter, `k` → `kill` insert-never-run.
- `ps -ef` decoration (core recognises `HeaderKind::Ef` but 1a only decorates `Aux`).

## 9. Follow-ups

1. **True in-place render** — replace the `ps` block's grid rows with the decorated model
   (fold hides rows, summary line inserted), bounded by OSC 133, freezing back to static
   text at the next prompt. This is the harder renderer change the parent spec §6 describes.
2. **1b bars** — `ps_decorate::bars_for(&quiet)` already yields the block-glyph bars; add the
   sort keys + page readout.
3. **1c inspector** — `group_rows` / `parse_enrich` are ready; add the two-pane overlay and
   the `k` → `kill <pid>` insert-never-run action.

## 10. Verification

- Unit: the core's parse/decorate is already tested; native side adds a `heat_band` test and
  a scrape-bounds test.
- In-app (Xephyr): run `ps aux`, press `Ctrl+Shift+D` → the panel shows folded kernel count,
  elided zeros, `K/M/G` sizes, and the colour bands; a non-`ps` last command → no panel
  (fail-safe); `[enhance] ps = off` → no panel.
