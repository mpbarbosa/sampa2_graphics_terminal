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
`sampa2 --title esctestrun -e python3 esctest.py …` (so esctest runs *inside* Sampa and
talks to it purely over the PTY), waits for the summary, and cleans up. It selects
`--xterm-checksum 334` — the raw (non-negated) 16-bit codepoint-sum convention, empty
cells counted as space (0x20) — which is exactly what `compute_decrqcra` replies with.

## Baseline

Native engine = `alacritty_terminal` 0.26:

```
*** 318 tests passed, 41 known bugs, 209 TESTS FAILED ***
```

**Release gate:** the pass count must not drop below this baseline.

### Fix log (45 → 49 → 251 → 256 → 267 → 280 → 295 → 307 → 318)

- **DECRQM modifiable modes shadowed (+11).** alacritty reports "not recognized" (0) for
  modes it doesn't model, so DECRQM never reflected an `SM`/`RM`/`DECSET`/`DECRESET`
  toggle. The scanner now records the set/reset of the tracked modifiable modes (ANSI
  KAM/SRM; DEC DECCOLM/DECSCLM/DECSCNM/DECPFF/DECPEX/DECNRCM/DECNKM/DECBKM/DECLRMM) into a
  per-terminal shadow, and `decrqm_modifiable` rewrites the outgoing DECRQM reply to that
  state (1 set / 2 reset, default reset). The test only checks the *report*, so no actual
  mode behavior is needed. All 11 modifiable-mode `DECRQM` cases pass (`DECRQM` 21→32).
- **Selective erase without protection (+12).** alacritty ignores the DEC-private erase
  `CSI ? Ps J` (DECSED) / `CSI ? Ps K` (DECSEL). Since the engine tracks no DECSCA
  protected attributes (every cell is unprotected), selective erase is equivalent to
  plain ED/EL, so the scanner emits `ScanEvent::SelectiveErase` and the pump injects the
  non-private `CSI Ps J` / `CSI Ps K` at the same cursor position. `DECSED` went 4→12,
  `DECSEL` 3→8 — exactly the non-protection cases (Default/0/1/2[/3][/WithScrollRegion]).
  The 16 remaining need real DECSCA protection tracking (a per-cell attribute alacritty
  doesn't model) and are left for a protection subsystem.

- **DECDSR device-status reports (+11).** vte dispatches only the non-private DSR
  (`CSI Ps n`), so every DEC-private query (`CSI ? Ps n`) went unanswered and timed out.
  A scanner now replies to all 11: DECXCPR (`?6n`) reports the live cursor (no page — the
  terminal presents as VT level 2 via DA2 type 0), DECCKSR (`?63n`) echoes the Pid with a
  zero macro checksum, and the rest are the fixed legal "feature absent" reports (no
  printer, keyboard = North American, no locator, 0 macro space, data-integrity OK,
  not multi-session). `DECDSR` went 0/11 → 11/11.
- **DECSET `?1048` save/restore cursor (+4).** alacritty leaves `?1048` unhandled
  (vte only maps `?1049`); the scanner translates `?1048h`/`l` to the DECSC/DECRC
  (`ESC 7`/`ESC 8`) it does support, recovering the tite-inhibit SaveRestoreCursor tests
  that don't also depend on left-right margins. `DECSET` went 13/31 → 17/31.

- **DECRQM permanently-reset modes (+13).** alacritty answers DECRQM (`CSI [?]Ps $ p`)
  for modes outside its enum as "not recognized" (state 0); esctest expects "permanently
  reset" (state 4) for modes xterm knows but never sets (ANSI GATM/SRTM/VEM/HEM/PUM/FEAM/
  FETM/MATM/TTM/SATM/TSM/EBM, DEC DECHCCM). We rewrite `0 → 4` on the outgoing reply for
  exactly those (keyed by the reply's own mode number + ANSI/DEC namespace), which is the
  correct answer and what xterm gives. `DECRQM` went 8/33 → 21/33; the remaining 11 are
  *modifiable* modes (KAM/SRM, DECCOLM/DECSCNM/DECNKM/…) that need real set/reset state
  tracking, not just a report.

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

With the color-query/DECSTR desyncs fixed, the XTWINOPS/DECRQM/DECDSR replies added, and
selective-erase + DECRQM-modifiable-mode handling, the pass count reached **318** — now
**past the origin Path B / xterm.js build (~305)**. The remaining failures are **genuine
feature gaps in the `alacritty_terminal` engine** relative to xterm — each a distinct
piece of work:

```
17 XtermWinops (WM ops / title read-back) · 17 DECSET (margins / 132-col / rev-wrap)
11 DECRQSS · 10 DECSED + 6 DECSEL (DECSCA protection)
8 DECCRA · 8 BS · 22 color edge-cases …
```

## Follow-up (to raise the baseline) — roughly by leverage

1. **Selective erase — protection half (16 left)** — the non-protection DECSED/DECSEL
   cases now pass via ED/EL translation; the rest need **DECSCA protected attributes**,
   which alacritty doesn't model. Would require a parallel per-cell protection layer
   (track `CSI Ps " q`, mark cells written while protected, and erase only unprotected
   cells on DECSED/DECSEL) — a real engine feature, not a reply-layer fix.
2. **DECRQSS (11)** — remaining status-string replies (scroll-region/margins/cursor-style
   need private engine state the query path can't yet see).
3. ~~**DECRQM modifiable modes (11)**~~ — **done** (shadowed set/reset state; see fix log).
4. **Left-right margins (DECLRMM/DECSLRM)** — a real engine feature alacritty lacks;
   unlocks the bulk of the remaining `DECSET` failures (plus DECOM-in-margins and the
   margin-dependent SaveRestoreCursor cases). Large; needs per-row left/right clipping.
5. Re-run per group (`--include DECSED`, …) and move the gate up as each lands.

Two groups are out of reach without deeper engine work: the 17 `XtermWinops` need a real
window manager (iconify/maximize/fullscreen/move) or title read-back (`20 t`/`21 t`, a
title-injection vector kept disabled per §13); the remaining `DECSET` failures need
left-right margins, 132-column mode (DECCOLM), reverse-wraparound, and `?47` alt-buffer
semantics — all real VT behavior in the engine, not reply-layer fixes.
