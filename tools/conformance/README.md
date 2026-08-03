# VT conformance — esctest

Sampa (native) is scored against [esctest2](https://github.com/ThomasDickey/esctest2),
which reads the rendered screen back over the PTY to verify behaviour. That read-back
relies on **DECRQCRA** (request checksum of a rectangular area), which the native VT
layer implements (`crates/sampa-native/src/main.rs`: `DecrqcraScanner` + `compute_decrqcra`).

## Running

```bash
cargo build --release
tools/conformance/run-esctest.sh                 # full suite (headless, own Xvfb)
tools/conformance/run-esctest.sh --include CUP   # a subset by test-name regex
```

The runner fetches a pinned esctest2, starts a private Xvfb, launches
`sampa --title esctestrun -e python3 esctest.py …` (so esctest runs *inside* Sampa and
talks to it purely over the PTY), waits for the summary, and cleans up. It selects
`--xterm-checksum 334` — the raw (non-negated) 16-bit codepoint-sum convention, empty
cells counted as space (0x20) — which is exactly what `compute_decrqcra` replies with.

## Baseline

Native engine = `alacritty_terminal` 0.26:

```
*** 251 tests passed, 45 known bugs, 272 TESTS FAILED ***
```

**Release gate:** the pass count must not drop below this baseline.

### Fix log (45 → 49 → 251)

- **Color queries resolved against the live table (+202).** OSC 4/10/11 *set* writes to
  alacritty's `colors` table; the query emits `ColorRequest(idx, fmt)`. We used to reply
  with a *fixed* palette value, so the reply's bytes never matched what esctest wrote —
  desyncing reads suite-wide. Now the reply is resolved at drain time from
  `term.colors()[idx]` (falling back to the palette default), so a queried color reports
  the app-set value. This was the dominant desync source; fixing it recovered far more
  than the color tests alone.
- **DECSTR (soft reset) synthesized (+4).** vte only dispatches `CSI $p`/`?$p` (DECRQM)
  for `CSI p`, so `CSI ! p` (DECSTR) was **silently ignored** — origin mode, scroll
  region, etc. leaked across tests (`reset()` runs DECSTR before each). The scanner now
  detects `CSI ! p` and injects the soft-reset state (`DECSTR_RESET`). Verified: after
  `DECSTBM 6;11` + `DECOM` + `DECSTR`, `CUP(3,6)` reports `ESC[3;6R` (was `ESC[8;6R`).

## What the baseline does and does not mean

DECRQCRA itself is correct — verified two ways:

- Unit tests (`decrqcra_scanner_detects_request`, `decrqcra_checksum_matches_sum`):
  `"AB"` over a 1×2 rect → `DCS 1 !~ 0083 ST`; a blank 1×2 rect → `…0040…` (2×0x20).
- A direct PTY probe of the built binary: after `CUP(3,6)` the cursor-position report
  is `ESC[3;6R` and the size report is `ESC[8;24;80t` — both correct, even after
  replaying esctest's full 76-command `reset()`.

With the color-query and DECSTR desyncs fixed, the pass count reached **251** (origin
Path B / xterm.js is ~305). The remaining ~272 are **genuine feature gaps in the
`alacritty_terminal` engine** relative to xterm — each a distinct piece of work:

```
28 XtermWinops · 24 DECRQM · 15 DECSET · 15 DECSED · 11 DECRQSS · 11 DECDSR
10 DECSEL · 8 DECCRA · 8 BS · 22 color edge-cases (special/dynamic/change) …
```

### Correctness notes (no esctest delta)

- **`CSI 8;h;w t` (XTWINOPS resize) is now honored** — the scanner detects it and resizes
  the grid + PTY (the engine ignored it). +0 on esctest (the rest of XtermWinops probes
  iconify/maximize/position/state, which need real window manipulation and aren't
  feasible headless), but it's a real feature for apps that resize via escape.

## Follow-up (to raise the baseline) — roughly by leverage

1. **XtermWinops reports (partial)** — the position/size/state reports it queries
   (`CSI 11/13/14/19 t`); the manipulation ops (iconify/maximize) can't pass headless.
2. **DECRQM extended modes (24)** — cover modes esctest probes that alacritty reports as
   "not recognized"; several are legitimate "known bugs".
3. **Selective erase (DECSED/DECSEL, 25)** — DECSCA protected attributes.
4. **DECRQSS (11)** — status-string replies (`DCS 1 $ r … ST`) for SGR/DECSTBM/etc.
5. **DECDSR (11)** — the device-status reports it expects.
6. Re-run per group (`--include XtermWinops`, …) and move the gate up as each lands.
