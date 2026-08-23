# Enhance views — the `Ctrl+Shift+D` command visualisers

Sampa turns a handful of everyday commands into **visualisations opened from the keyboard**,
instead of re-reading their raw text output. They share one shortcut and one rule.

## One shortcut, dispatched on what you typed

Press **`Ctrl+Shift+D`** (`keybindings.enhance_ps`). The **first token of the command line
you've typed** (keystroke-derived — autosuggestions never affect it) selects the view:

| Typed | View | Shows | Composes a command? |
|---|---|---|---|
| `cd` | **directory tree picker** | a lazily-expanded tree of the cwd's subdirectories | ✅ `cd <path>` on Enter |
| `du` | **disk-usage treemap** | a squarified treemap of the cwd (area ∝ size), zoomable | ✅ `cd <path>` on Enter |
| `free` | **memory gauge** | proportional RAM/swap segmented bars with a legend | ❌ display-only |
| `df` | **disk-free gauge** | one segmented bar per filesystem (used / reserved / free), fullest-first | ❌ display-only |
| `ping` | **latency chart** | a per-packet RTT sparkline (height ∝ RTT, banded) + loss ticks + summary | ❌ display-only |
| `uptime` | **load gauge** | three load bars (1/5/15 min) scaled to CPU cores, banded by load÷cores | ❌ display-only |
| `netstat` / `ss` | **connections table** | open sockets as a state-coloured table (listening-first) + a summary | ❌ display-only |
| *anything else* (incl. `ps`) | **ps output enhancement** | the last `ps aux`, decorated: quiet columns / bars + live sort / grouped inspector (by width) | `k` → `kill <pid>` in the inspector |

Each is a **modal overlay**: `Esc` (and its view-specific keys) dismisses it and returns
focus to the terminal.

## The rules they share

- **Read-only.** Every view either runs a read-only command (`du -k`, `free -k`, `df -k`, a
  bounded `ping`, `uptime`) or reads the scrollback/cwd. Nothing runs on your behalf.
- **Insert-never-run.** The views that produce a command (`cd`, `du` → `cd <path>`; the ps
  inspector → `kill <pid>`) **compose it at the prompt and never press Enter** — you review
  and run it yourself. The `free` gauge has no command to compose, so it's purely
  informational. This is the same boundary the command palette and AI overlays honour.
- **Fail-safe.** A malformed or missing target (no `ps aux` in the buffer, an unreadable
  directory, a `du` that overruns its 6 s budget) shows a one-line message, never garbage.
- **Width-gated (ps only).** `[enhance] ps = off | quiet | bars | inspector` picks the ps
  level; below the width thresholds it steps down (`resolve_level`).

## A companion toggle

**`Ctrl+Shift+I`** — *in-place* ps colouring: decorate `ps aux` output **where it sits in
the scrollback** (heat-coloured `%CPU`/`%MEM`, elided zeros, dimmed kernel threads) instead
of in a panel. A per-frame, non-destructive transform — copy/paste stays byte-identical.

## The per-view contracts

- [spec-ps-output-enhancement.md](spec-ps-output-enhancement.md) — the ps levels (1a/1b/1c)
  and [spec-ps-native-1a.md](spec-ps-native-1a.md) — the native panel + in-place render.
- [spec-cd-tree-picker.md](spec-cd-tree-picker.md) — the `cd` tree picker.
- [spec-du-treemap.md](spec-du-treemap.md) — the `du` treemap (squarify + zoom).
- [spec-free-gauge.md](spec-free-gauge.md) — the `free` memory gauge.
- [spec-df-gauge.md](spec-df-gauge.md) — the `df` disk-free gauge.
- [spec-ping-chart.md](spec-ping-chart.md) — the `ping` latency chart.
- [spec-load-gauge.md](spec-load-gauge.md) — the `uptime` load gauge.
- [spec-netstat-table.md](spec-netstat-table.md) — the `netstat`/`ss` connections table.

Related overlays that share the insert-never-run rule but not the `Ctrl+Shift+D` dispatch:
the [AI suggester/explainer](spec-ai-overlay.md) (`Ctrl+Shift+A` / `Ctrl+Shift+X`) and the
[command palette](spec-command-palette-search.md) (`Ctrl+Shift+P`).
