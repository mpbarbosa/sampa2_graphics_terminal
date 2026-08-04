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
*** 267 tests passed, 43 known bugs, 258 TESTS FAILED ***
```

**Release gate:** the pass count must not drop below this baseline.

### Fix log (45 → 49 → 251 → 256 → 267)

- **XTWINOPS size/state reports + pixel/DECSLPP resize (+11).** vte only dispatches the
  char-size winop (`CSI 18 t`); the pixel/report queries the resize tests use as helpers
  (`GetDisplaySize` → `CSI 19 t`, `GetWindowSizePixels` → `14 t`, `GetScreenSizePixels`
  → `15 t`, `GetCharSizePixels` → `16 t`, window state/position → `11 t`/`13 t`) all
  timed out, so every `XtermWinops` test failed at its first probe. A scanner now answers
  those reports from the live grid + fixed cell/display metrics (self-consistent:
  text-area px == chars × cell px), resizes on `CSI 4 t` (pixels) and `CSI Ps t` DECSLPP
  in addition to `CSI 8 t`, and distinguishes an **omitted** dimension (keep) from an
  explicit **0** (maximize to the display). `XtermWinops` went 0/28 → 11/28; the
  remaining 17 need a real window manager (iconify/maximize/fullscreen/move) or title
  read-back (`20 t`/`21 t`, a title-injection vector left disabled per §13).

- **DECRQSS status strings (+5).** `DCS $q <Pt> ST` is unhandled by the engine; a DCS
  scanner now extracts the query and replies `DCS 1 $r <value> <Pt> ST`: SGR (`m`)
  reconstructed from the pen (`cursor.template`), DECSCL (`"p`) fixed at `64;1`, and the
  `+q`/`*}`/`$}`/`*x`/`"q` settings reported at their defaults; unsupported → `0$r`.
  Scroll-region/margins/cursor-style queries (`r`/`s`/` q`) need private engine state
  and still report invalid.

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

With the color-query/DECSTR desyncs fixed and the XTWINOPS reports added, the pass count
reached **267** (origin Path B / xterm.js is ~305). The remaining failures are **genuine
feature gaps in the `alacritty_terminal` engine** relative to xterm — each a distinct
piece of work:

```
24 DECRQM · 17 XtermWinops (WM ops / title read-back) · 15 DECSET · 15 DECSED
11 DECRQSS · 11 DECDSR · 10 DECSEL · 8 DECCRA · 8 BS · 22 color edge-cases …
```

## Follow-up (to raise the baseline) — roughly by leverage

1. **DECRQM extended modes (24)** — cover modes esctest probes that alacritty reports as
   "not recognized"; several are legitimate "known bugs".
2. **DECSET (15)** — private-mode set/reset esctest exercises that the engine drops.
3. **Selective erase (DECSED/DECSEL, 25)** — DECSCA protected attributes.
4. **DECRQSS (11)** — remaining status-string replies (scroll-region/margins/cursor-style
   need private engine state the query path can't yet see).
5. **DECDSR (11)** — the device-status reports it expects.
6. Re-run per group (`--include DECRQM`, …) and move the gate up as each lands.

The 17 `XtermWinops` still failing are out of reach headless: window-manager ops
(iconify/maximize/fullscreen/move) and title read-back (`20 t`/`21 t`, a title-injection
vector kept disabled per §13).
