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
*** 49 tests passed, 13 known bugs, 506 TESTS FAILED ***
```

**Release gate:** the pass count must not drop below this baseline.

### Fix log

- **DECSTR (soft reset) synthesized.** vte only dispatches `CSI $p`/`?$p` (DECRQM) for
  `CSI p`, so `CSI ! p` (DECSTR) was **silently ignored** — origin mode, scroll region,
  etc. leaked across esctest tests (`reset()` calls DECSTR before every test), cascading
  failures. The scanner now detects `CSI ! p` and injects the soft-reset state itself
  (`DECSTR_RESET`). Verified: after `DECSTBM 6;11` + `DECOM` + `DECSTR`, `CUP(3,6)`
  reports `ESC[3;6R` (was `ESC[8;6R`). +4 tests, and a real correctness fix for any app
  that soft-resets.

## What the baseline does and does not mean

DECRQCRA itself is correct — verified two ways:

- Unit tests (`decrqcra_scanner_detects_request`, `decrqcra_checksum_matches_sum`):
  `"AB"` over a 1×2 rect → `DCS 1 !~ 0083 ST`; a blank 1×2 rect → `…0040…` (2×0x20).
- A direct PTY probe of the built binary: after `CUP(3,6)` the cursor-position report
  is `ESC[3;6R` and the size report is `ESC[8;24;80t` — both correct, even after
  replaying esctest's full 76-command `reset()`.

So basic sequences are right, and DECRQCRA/CPR/size replies are correct. After the
DECSTR fix, the remaining ~506 are dominated by **genuine feature gaps in the
`alacritty_terminal` engine** relative to xterm — not a shared cascade and not a
rendering fault:

```
44 DECRQM · 28 XtermWinops · 25 DECSET · 18 DECSED · 14 DECRQSS
13×3 color queries · 11 DECSTR · 11 DECSEL · 11 DECDSR · 10 SCORC · 10 DECCRA …
```

The origin (Path B, xterm.js) reached ~305 because xterm.js implements many of these
xterm-specific behaviours; `alacritty_terminal` implements fewer, so the native build
must add them on top of the engine. Each group below is a distinct piece of work, not a
one-line format tweak.

## Follow-up (to raise the baseline) — roughly by leverage

1. **Color queries (39)** — track OSC 4/10/11 *set* colors and report the *current*
   value on query (today `palette_rgb` returns fixed defaults, ignoring app-set colors).
   Needs the reply path to read the live color table.
2. **DECRQM extended modes (44)** — cover the modes esctest probes that alacritty
   reports as "not recognized"; several are legitimate "known bugs".
3. **XtermWinops (28)** — real resize on `CSI 8;h;w t`, plus the position/size reports.
4. **Selective erase (DECSED/DECSEL, 29)** — DECSCA protected attributes.
5. **DECRQSS (14)** — status-string replies (`DCS 1 $ r … ST`) for SGR/DECSTBM/etc.
6. Re-run per group (`--include DECRQM`, …) and move the gate up as each lands.
