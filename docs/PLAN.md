# Development Plan — Sampa Rust-only Graphical Terminal (Path C)

This repo is the **native, Rust-only** build of Sampa — DESIGN.md **§4.2 Path C
("Fully native")**. It is a **renderer swap, not a rewrite**: the seven headless
crates already proven in the origin repo
([`sampa_graphics_terminal`](https://github.com/mpbarbosa/sampa_graphics_terminal))
are reused unchanged; what we build here is everything `xterm.js` + the WebKit
webview currently provide "for free."

> Source docs: [`rust-only-feasibility.md`](rust-only-feasibility.md) (the go/no-go
> and cost analysis), and the origin repo's `docs/DESIGN.md` (layer/§ numbering) and
> `docs/ROADMAP.md` (Path B milestones M0–M5). `§n` below points at that DESIGN.md.

Legend: ✅ done · 🔨 in progress · ⬜ not started.

---

## 1. What we reuse vs. what we build

**Reused unchanged (cross-repo dependency) — the durable asset, ~2,560 LOC, tested:**

| Crate | Role | Fits the native build because |
|---|---|---|
| `pty-core` | spawn `$SHELL`, pump bytes, resize→SIGWINCH, reap, `/proc` cwd | already an event-channel API (`Sender<PtyEvent>`); no GUI imports |
| `sampa-config` | TOML model + validation + live-reload | already has `Font`/`Colors`/`Window`/`Cursor`/`Rendering` sections |
| `sampa-cli` | argv parser (`-e`, `--working-directory`, `--hold`, `--class`…) | infallible, GUI-free |
| `sampa-shellint` | incremental OSC 7 (cwd) + OSC 133 (prompt marks) scanner | byte-stream scanner; feeds man/preview command detection |
| `sampa-palette` | `$PATH` executable enumeration | pure logic |
| `sampa-man` | `man -P cat` + nroff/ANSI strip, validated | pure logic |
| `sampa-preview` | **authoritative** read-only allowlist gate + sandboxed run | the security boundary; `typed_rm_never_deletes_the_file` test carries over |

**Built here (was xterm.js + webview):**

1. **VT engine** — parser + grid + modes + scrollback (replaces xterm.js emulation).
2. **GPU renderer + glyph atlas** — one draw per vsync, damage-tracked.
3. **Text stack** — shaping, rasterization, Unicode width, font fallback.
4. **Windowing + event loop** — window, tabs, DPI, resize.
5. **Input** — keyboard→bytes, mouse, IME/compose, clipboard.
6. **Native feature UIs** — palette overlay, man panel, preview panel, paste/OSC-52 modals (the *services* stay in Rust; only their UI is redrawn).
7. **Accessibility tree** — the webview gave this free; native must add it.
8. **DECRQCRA + escape-hardening** that lived in `main.ts` move into the VT layer.

---

## 2. [Decision] points — settle before N0

These are the design gates. Recommendations in **bold**; record each as an ADR
(`docs/adr/`) when resolved.

| Decision | Options | Recommendation |
|---|---|---|
| **VT engine** | `alacritty_terminal` · `wezterm-term` · `vte`+own grid · from scratch | **`alacritty_terminal`** — mature grid+parser, powers Alacritty, conformance starts high not at zero (feasibility §5.2) |
| **Windowing / event loop** | `winit` · `tao` · gtk4-rs | **`winit`** — X11+Wayland, DPI, IME events; unblocks the runtime `--class`/WM_CLASS the `tao` build couldn't set |
| **GPU / present** | `wgpu` · raw GL · `softbuffer` | **`wgpu`** — portable, instanced glyph quads + bg pass |
| **Text stack** | `cosmic-text` · `swash`+`harfbuzz`+`fontconfig` | **`cosmic-text`** — layout+shaping+fontdb+swash in one; owns width/fallback/emoji |
| **Clipboard** | `arboard` · `wl-clipboard` shell-out | **`arboard`** (+ X11 PRIMARY handling) |
| **Accessibility** | `AccessKit` · accept regression | **`AccessKit`** (defer to N5) |
| **Core consumption** | git dep · crates.io · extract shared `sampa-core` repo | **git dep (pinned rev) now**; defer the shared `sampa-core` repo — the common surface is a fixed ~1,875 LOC that won't grow and shrinks to <10% of this build by v1. See [ADR 0002](adr/0002-core-code-sharing.md); revisit at end of N1/N2 only if both repos co-edit the crates. |

**Architectural consequence of going native:** the IPC seam **disappears**. In Path
B the seam carried raw bytes to xterm.js; here Layer 2 (VT) moves *into* the core and
the frontend calls it directly. Keep the DESIGN.md §9 command/event *shape* as an
in-process API boundary so the core stays headless and unit-testable — but there is
no base64, no IPC, no port.

---

## 3. Milestones

Native milestones **N0–N6** mirror the origin M0–M5 but are scoped to the renderer
swap. Each ends demoable; each lands its own tests (§17). Parity is measured against
the origin's `tools/conformance/` esctest baseline (**≥305**) and the §17 real-app
matrix — that harness carries over and becomes *more* valuable here, since the VT
engine is now ours to prove.

### N0 — Foundations + native echo ✅ **shipped**
*Validates: cross-repo core wiring, winit+wgpu, VT engine, the collapsed seam.*

- ✅ Cargo workspace (`crates/sampa-native`, bin `sampa`); depends on the seven crates
  via **path deps** (ADR 0002) — clean build here. `sampa --smoke` is a headless
  wiring check (config defaults, 4,461 `$PATH` exes, PTY round-trip through `shellint`).
- ✅ `winit` 0.30 window + `wgpu` 30 surface; clear + present each frame.
- ✅ Spawn `$SHELL` via `pty-core`; reader thread feeds `PtyEvent::Output` into an
  `alacritty_terminal` `Term` behind a `Mutex`, waking the loop via an `EventLoopProxy`.
- ✅ `cosmic-text`/`glyphon` renders the grid as monospace text (colors/cursor → N1).
- ✅ Resize → recompute cols/rows → `Term::resize` + `pty.resize(...)` → SIGWINCH.
- ✅ Best-effort keyboard→bytes (Enter/Backspace/Esc/Tab/arrows/Home/End, Ctrl-letters).

> **Verified live:** a native window runs zsh; with `SAMPA_AUTORUN='echo SEAM_OK'` the
> command runs and the grid dump shows prompt → command → `SEAM_OK` → next prompt,
> proving PTY → VT → render. Unit test `parser_writes_reach_the_grid` covers the VT
> seam deterministically. No webview in the binary.

**Exit:** ✅ met.

### N1 — The "real terminal" contract (§3) 🔨  *(the correctness sink — §19)*
*Mostly input encoding; budget the most time here.*

- ✅ **Renderer color + cursor pass.** A solid-quad wgpu pipeline draws per-cell
  **background colors** and a **block cursor**, aligned to the measured monospace
  advance; `glyphon` rich-text draws per-cell **foreground colors** with bold/italic;
  a built-in 256-color palette resolves named/indexed/**truecolor**, honoring OSC
  4/10/11 overrides; DIM/INVERSE/HIDDEN handled; sRGB-correct. Verified by 6 unit
  tests (red/truecolor/indexed/inverse/cursor resolution) **and an offscreen PNG**
  (`sampa --capture`, a headless CI screenshot) showing all of the above painting.
- ✅ **Keyboard→bytes (§8.1):** printable, C0 controls (Ctrl-letter/Space/`[\]^_`),
  **Alt=Meta** (ESC prefix), arrows/Home/End honoring **DECCKM**, editing keys
  (Ins/Del/PgUp/PgDn), **F1–F12**, back-tab, and xterm **modifier encoding**
  (`CSI 1;<mod> <fin>` / `CSI <code>;<mod> ~`). 5 unit tests + a DECCKM-tracking test.
  ⬜ still: **kitty keyboard protocol**, keypad-application numpad, IME/compose,
  **bracketed paste** (2004, lands with clipboard).
- ✅ **Mouse (§8.2):** **SGR 1006** (+ X10 fallback) reporting for press/release/drag/
  wheel when the app enables a mouse mode (1000/1002/1003); button + Shift/Alt/Ctrl
  modifier bits; **Shift** forces local selection over app grab. 3 unit tests.
- ✅ **Selection + clipboard (§8.3, first pass):** click-drag **Simple** selection via
  `alacritty_terminal::Selection`, highlighted in the render; **Ctrl-Shift-C** /
  auto-copy-on-release and **Ctrl-Shift-V** / **middle-click** paste via `arboard`;
  **bracketed paste** (2004) wrapping with embedded-`ESC[201~` stripping (§13). PNG +
  membership test. ⬜ still: word/line/block granularity, X11 PRIMARY split,
  multi-line paste confirm.
- ✅ **Scrollback scrolling** (10k-line history): **wheel** (3 lines/notch) and
  **Shift+PageUp/PageDown**; display-offset-aware rendering (negative lines = history);
  typing snaps to the live prompt; disabled on the alt screen. Unit test covers the
  offset shift.
- ✅ **Coalesced redraw → one draw per frame** (DESIGN §4.3): output bursts mark the
  window dirty via `request_redraw`; winit delivers a single `RedrawRequested` per
  frame, so a flood parses eagerly but renders at most once per frame — intermediate
  frames dropped, never intermediate state. ⬜ still: *per-cell* damage tracking (today
  each frame rebuilds the glyph buffer); explicit vsync frame-pacing/benchmarks.
- ✅ **Underline / strikethrough** decorations — drawn as thin quads over the glyphs
  (all underline variants collapse to a single underline for now); unit test + PNG.
- 🔨 **App-matrix + contract smokes (§17, §3)** — headless harnesses (`#[ignore]`,
  `cargo test -- --ignored`) run real programs through real PTYs:
  - `app_matrix_smoke` — rendering: **echo, ls (color), seq (wrap+scrollback),
    python, vim (alt-screen)** all green; htop/tmux/neovim skipped (not installed).
  - `ctrl_c_sends_sigint` — ✅ typed **^C terminates `sleep`** (line-discipline SIGINT).
  - `resize_reaches_child` — ✅ **`pty.resize` → child sees `30 100`** (TIOCSWINSZ/SIGWINCH).
  ⬜ still: install + smoke htop/tmux/neovim/less/weechat/emacs; **live** resize-reflow
  and Ctrl-Z job control driven at the keyboard in the GUI window.

**Exit — M1 app matrix (§17):** vim, neovim, tmux, htop, less, git log, ipython each
render without corruption, respond to resize, honor Ctrl-C/Ctrl-Z; clean exit. *(Render
+ SIGINT + SIGWINCH contracts verified headlessly for the installed subset; live GUI
interactive pass + wider program coverage outstanding.)*

### N2 — Comfort: config-driven renderer (§7.3, §11) ⬜
- Consume `sampa-config` live-reload: themes (16 ANSI + fg/bg/cursor/selection,
  truecolor, OSC 4/10/11), fonts + **fontconfig fallback**, line height, cursor
  shape/blink, **ligatures toggle** (default off), visual/audible bell.
- **Tabs** on `pty-core`'s session table.
- **Search overlay** — now owned natively (replaces `addon-search`): incremental
  match + highlight over scrollback.

**Exit:** a config edit hot-reloads theme/font/cursor; a Powerline + truecolor prompt
renders; tabs run independent shells; search highlights scrollback matches.

### N3 — Linux citizen (§12, §16) ⬜
- Wire `sampa-cli`: `-e`/`--`, `--working-directory`, `--title`, `--hold`, `--login`,
  `--config`, and **`--class`/WM_CLASS set at runtime** (native win, unlike the origin's
  `tao` limitation).
- `.desktop` (`TerminalEmulator` category), `update-alternatives` for
  `x-terminal-emulator`, `xdg-terminal-exec`.
- Packaging: AppImage + `.deb` + `.rpm` — now a **single self-contained ~5 MB binary**
  with **no webkit runtime dependency** (a real gain over Path B's SONAME deps). No
  Flatpak (origin [ADR 0001] applies — sandbox fights a host-shell terminal).

**Exit:** a clean-VM `.deb` install launches, registers as a terminal alternative,
runs the host shell; AppImage runs with no install; WM_CLASS verified via `xprop`.

### N4 — Signature features, natively drawn (§10) ⬜
Services already exist; only the UI is new. Draw panels/overlays in the wgpu scene.

- **Palette** (`sampa-palette`): `Ctrl-Shift-P` overlay, fuzzy filter, Enter
  **inserts** `"<cmd> "` — never auto-runs.
- **Man panel** (`sampa-man`): command detected from typed keystrokes +
  `sampa-shellint` OSC-133 prompt boundaries (robust, not grid-scraping);
  **collapses** for keywords/no-man.
- **Preview panel** (`sampa-preview`): the authoritative gate is untouched; debounce
  ~550 ms, **clear on Enter**. Re-assert the filesystem-verified `rm`-is-refused test.
- OSC 7/133 already handled by `sampa-shellint`; ship the opt-in zsh/bash hooks.

**Exit (§17):** palette inserts (not runs); man opens/closes correctly; preview
refuses writes (file provably untouched) and clears on Enter.

### N5 — Graphics, links, i18n, a11y 🔨  *(the native long tail — §19)*
- 🔨 **Images:** ✅ **iTerm2 inline images (OSC 1337)** end-to-end — an OSC scanner
  extracts the payload (chunk-split-safe), `image` decodes it, and a wgpu **textured
  pipeline composites** it into the scene at the cursor; **§13 OOM caps** (max dims,
  source bytes, in-flight OSC bytes, live-image count with oldest-evicted). 3 unit tests
  + PNG. ⬜ still: **sixel** + **kitty** protocols; precise **scroll-out lifecycle**
  (anchored to an absolute line — drifts if the grid scrolls after insert).
- 🔨 **OSC 8 hyperlinks:** ✅ OSC-8 tracked (via `alacritty_terminal`), rendered
  **underlined**, **Ctrl+click-to-open** with the target shown in the title and a strict
  **http/https scheme gate** (§13 — never `file:`/`javascript:`); auto-opens nothing.
  ✅ **plain-URL detection** too (`url_at`: whitespace token under the cursor → extract
  http/https, trim punctuation) so links work without OSC-8. 3 unit tests + PNG.
  ⬜ still: an in-window confirm/preview overlay (vs. title), hover affordance.
- **IME / dead keys / compose:** wire IBus/fcitx via winit IME events — *historically
  where native terminals sink time* (feasibility §4). CJK/emoji width from the text
  stack.
- **Accessibility:** an **AccessKit** tree (the webview gave one free).
- 🔨 **Escape hardening in the VT layer (§13)** — a real `EventProxy` listener
  (replacing `VoidListener` for the live terminal) funnels VT events to the main loop:
  - ✅ **query replies routed** — DA/DSR/DECRQSS `PtyWrite` and OSC 4/10/11 color
    requests now answer the app (a correctness win — `VoidListener` silently dropped
    them); color replies use fixed palette values, never attacker input.
  - ✅ **OSC-52 write gate** — surfaced as `ClipboardStore`, **default-deny**
    (`SAMPA_OSC52=allow` to permit); **OSC-52 reads dropped** (no clipboard
    exfiltration to the PTY).
  - ✅ **title (OSC 0/2) sanitized** — control chars stripped, capped at 256.
  - ✅ **synchronous, in-order reply path** — replies (DA/DSR/DECRQSS/color, DECRQCRA)
    are written to the PTY by the parser thread in stream order via a shared
    `Arc<Mutex<PtyHandle>>`, not routed through the UI loop (a query must be answered
    before the next byte; verified: CPR after `CUP(3,6)` returns `ESC[3;6R`).
  - ✅ **DECRQCRA** (`CSI … * y`) — an incremental scanner splits the output stream so
    the rectangular-area checksum sees the exact grid state, replying
    `DCS Pid !~ HHHH ST` (raw 16-bit codepoint sum, empty = 0x20 — the
    `--xterm-checksum 334` convention). 6 unit tests (title/DA/OSC-52 ×2, scanner, checksum).
  - 6 unit tests. ⬜ still: interactive OSC-52 **consent modal** (vs. env toggle); bell.

**Exit:** sixel renders within caps, oversized rejected not OOM; links need a click
and show target; a CJK/emoji/compose input test passes; a screen reader sees the grid.

### N6 — Conformance, perf, v1 (§14, §17) 🔨
- 🔨 **esctest:** harness wired ([tools/conformance/](../tools/conformance/README.md)) —
  fetches pinned esctest2, runs headless under Xvfb against the native binary via
  `sampa -e python3 esctest.py`, scores with `--xterm-checksum 334`. **Baseline: 251
  passed / 45 known-bug / 272 failed** (gate: don't regress) — up from 45 by fixing two
  suite-wide desyncs: **color queries** now resolve against the live color table (+202),
  **DECSTR** soft-reset synthesized (+4). DECRQCRA correct (unit-tested + PTY-probed:
  `CUP(3,6)`→`ESC[3;6R`). Remaining gap to the origin's ~305 is genuine xterm feature
  coverage in `alacritty_terminal` (window-ops, selective erase, DECRQSS/DECDSR, extended
  DECRQM) — ranked roadmap in [tools/conformance/](../tools/conformance/README.md).
- **vttest** manual smoke; **real-app matrix** green (add mc, ssh, weechat, emacs -nw,
  fzf, truecolor, sixel, CJK/emoji).
- **Perf:** the native ceiling is the payoff — `cat 50MB` throughput, typometer
  added-input-latency **< one frame** (now measurable without a webview compositor in
  the paint path), 100k-line scrollback memory. Trend in CI.

**v1:** app matrix green · esctest threshold met · latency/throughput targets met ·
signature-feature tests green · config reference + docs complete.

---

## 4. Sequencing

```
N0 ──► N1 ──► N2 ──► N3 ──► N4 ──► N5 ──► N6/v1
 │      │              │       ▲
 │      │ scrollback    │ config  │ OSC7/133 (shellint) already
 │      └─► reused by tabs (N2)   │ makes man/preview robust
 └─ collapsed seam: VT moves into the core; no IPC
```

- **N1 dominates the calendar** — keyboard/mouse encoding is the correctness sink.
- **N5 is the open-ended native cost** — IME + shaping + fallback + a11y, each
  substantial; that is the price of dropping the webview.
- The **[Decision] points (§2)** are gates, not phases — resolve + ADR them before N0.

## 5. Risks (Path-C-specific, from feasibility §4)

- **VT conformance is now ours to own** → mitigated by building on
  `alacritty_terminal`, not from scratch; gate releases on the esctest baseline.
- **The Linux i18n/text stack is the hard part** (IME/compose/fallback/emoji) — the
  webview delivered it free; deferred to N5 deliberately, but do not underestimate it.
- **Lost webview freebies:** a11y tree, clipboard/IME plumbing, OSC-8 affordances,
  HTML/CSS panels — each re-earned above.
- **Two-repo drift** on the shared crates → the extract-`sampa-core` decision (§2).

## 6. First actions

1. Resolve + ADR the §2 [Decision]s (VT engine, windowing, text stack, core
   consumption).
2. Scaffold the Cargo workspace; add git deps on the seven crates; confirm a clean
   build in this repo.
3. Build **N0** — winit+wgpu window driving `pty-core` through `alacritty_terminal`.
   That single spike de-risks the whole seam.

> **Verdict (feasibility §5):** technically feasible and explicitly on the roadmap.
> Go native for binary size, latency headroom, and a webview-free footprint — and
> treat it exactly as a **renderer swap behind the existing seam**, reusing every
> `crates/*` line and the conformance harness.
