# Feasibility — reimplementing Sampa in Rust only

> **Provenance.** This assessment was written in the original
> [`sampa_graphics_terminal`](https://github.com/mpbarbosa/sampa_graphics_terminal)
> repo (the Tauri + xterm.js implementation) and moved here, since **this repo is
> where the Rust-only build lives**. File and section references below —
> `DESIGN.md §…`, `crates/*`, `src/main.ts`, `src-tauri/`, `tools/conformance/` —
> point at that original repo unless noted. Its
> [DESIGN.md](https://github.com/mpbarbosa/sampa_graphics_terminal/blob/main/docs/DESIGN.md)
> is the source of the layer/§ numbering.
>
> **Cross-repo note.** Because the Rust build is a *separate* repo, the "reuse every
> `crates/*` line unchanged" plan below means consuming those seven headless crates
> **across repos** (git dependency, crates.io, or an extracted shared `sampa-core`
> repo) rather than an in-place renderer swap.

**Question:** can Sampa be rebuilt using only Rust, dropping the TypeScript/webview
frontend?

**Short answer:** Yes, and the architecture was deliberately designed to allow it —
this is exactly **Path C ("Fully native")** in DESIGN.md §4.2. It is a
*renderer swap, not a rewrite*: the entire headless core already in Rust survives
unchanged; what gets reimplemented is the ~1,000-line webview frontend and everything
xterm.js currently does for free. That "for free" is the whole cost — it is large.

---

## 1. Where the language boundary is today

| Layer | Language | LOC | Reusable as-is in a Rust-only build? |
|-------|----------|-----|--------------------------------------|
| Core services (`crates/pty-core`, `config`, `cli`, `shellint`, `palette`, `man`, `preview`) | **Rust**, GUI-free | ~2,560 | **Yes — unchanged.** None import Tauri/webkit/wry (verified). |
| App shell (`src-tauri/`) | Rust (Tauri) | part of above | Partly — the IPC bridge is replaced by direct function calls; the session/config/watch logic ports over. |
| Frontend (`src/main.ts`, `index.html`) | **TypeScript/HTML** | ~1,034 | **No — reimplemented in Rust.** |
| VT emulation + rendering | **xterm.js + 5 addons** (fit, webgl, image, search, web-links) | (external, ~tens of kLOC) | **No — replaced by a Rust VT engine + renderer.** |

The frontend looks small (1,034 lines) only because it *delegates* the hard parts to
xterm.js: the VT parser and grid, glyph rasterization and the WebGL atlas, font
fallback, Unicode width, selection, scrollback rendering, and — via addons — sixel/iTerm
image decode, incremental search, and hyperlink handling. Reimplementing in Rust means
owning all of that.

The design already anticipated this split. From DESIGN.md §4: a **renderer-agnostic
core** behind a thin frontend, with a seam whose *shape* is identical whether the
frontend is a webview (IPC) or native (function calls). The seven headless crates are
the durable asset precisely so "the webview renderer can be swapped for a native GPU
renderer later without rewriting the terminal."

## 2. What "Rust only" requires building

Everything below currently comes from the webview/xterm.js and would move into Rust
(the DESIGN.md §20 references already name the crates for each):

1. **VT engine** (replaces xterm.js emulation) — the parser, grid, modes, scrollback.
   Don't write from scratch: build on **`alacritty_terminal`** or **`wezterm-term`**
   (libraries), or `vte`/`libvterm`. This is the single biggest item and the one
   xterm.js otherwise gives for free with battle-tested conformance.
2. **GPU renderer + glyph atlas** — **`wgpu`** (or GL/`softbuffer`) with a custom
   cached glyph atlas; coalesce to one draw per vsync (DESIGN.md §4.3).
3. **Text stack** — shaping/rasterization/fallback via **`cosmic-text`** / **`swash`**
   + **`harfbuzz`** + **`fontconfig`**/FreeType. Owns Unicode width, CJK, emoji,
   ligatures, Nerd-Font glyphs — all currently the browser's job.
4. **Windowing + event loop** — **`winit`** (or `tao`, or gtk-rs). Tabs, DPI, resize.
5. **Input** — keyboard→bytes (DECCKM, keypad, **CSI-u/`modifyOtherKeys`**, kitty
   protocol), mouse (SGR 1006), **IME / dead keys / compose** (hook IBus/fcitx on
   Linux — hard), clipboard (**`arboard`**/`wl-clipboard`) incl. the OSC 52 policy.
6. **Feature reimplementation** (currently DOM/addons):
   - sixel + iTerm image decode & blit (replaces `addon-image`);
   - incremental search overlay (replaces `addon-search`);
   - plain + OSC 8 hyperlink detection and click-to-open (replaces `addon-web-links`);
   - the **man / palette / preview panels** and the paste-confirm/OSC-52 modals — the
     *services* stay in Rust already; only their **UI** (today HTML/CSS) is redrawn;
   - the `§13` escape-hardening handlers and **DECRQCRA** that today live in `main.ts`
     move into the native VT layer.
7. **Accessibility** — the webview gives a screen-reader tree for free; native must add
   one (e.g. **AccessKit**) or accept a regression.

## 3. What stays exactly as-is

All seven `crates/*` — PTY lifecycle, config model + validation, the CLI parser, the
OSC 7/133 scanner, `$PATH` palette enumeration, `man` rendering, and the security-
critical **preview allowlist gate** — are already headless Rust with unit tests and no
GUI imports. They plug into a native frontend through the same seam, unchanged. That is
~2,560 lines and the project's most valuable, already-conformant logic (including the
`typed_rm_never_deletes_the_file` security boundary). The **esctest/vttest** harness in
`tools/conformance/` also carries over and becomes *more* valuable — it's how you'd prove
a from-scratch VT engine reaches parity.

## 4. Trade-offs

**Gains**
- One language end-to-end; no TS build, no npm supply chain, no base64 IPC boundary
  (PTY bytes → parser becomes a function call, not an encoded event).
- Smallest binary (~5 MB vs ~10–20 MB today; DESIGN.md §4.2) and the **best** latency/
  throughput ceiling — no webview compositor in the paint path.
- Full control of the render loop, damage tracking, and frame pacing.

**Costs / risks**
- **VT conformance becomes yours to own.** xterm.js ships in VS Code and is correct on
  day one; a native engine must earn its esctest/vttest numbers. Mitigated by building
  on `alacritty_terminal`/`wezterm-term` rather than from scratch.
- **The i18n text stack is the hard part on Linux** — IME/compose, font fallback, emoji,
  complex scripts. The webview delivers these for free; native means wiring IBus/fcitx,
  fontconfig, and shaping yourself. Historically where native terminals sink time.
- Lose several "for free" webview goodies: OSC-8 link affordances, clipboard/IME
  plumbing, the a11y tree, HTML/CSS for panels and modals.
- Months of work to reach current feature parity, for a product that already works.

## 5. Recommended approach (if pursued)

1. **Keep the seam; consume the core cross-repo.** This repo's native frontend
   (winit + wgpu + cosmic-text) depends on the origin repo's seven headless crates via
   a git/crates.io/shared-repo dependency and drives them through the same command/event
   shape the webview used.
2. **Reuse, don't rewrite, the VT engine** — adopt `alacritty_terminal` or
   `wezterm-term` so conformance starts high, not at zero.
3. **Track parity against the origin project**, measured by its `tools/conformance/`
   esctest baseline (≥305) + the §17 real-app matrix (vim/tmux/htop/…). Ship when green.
4. Defer image protocols, accessibility, and exotic input to a second pass — they're the
   long tail, not the MVP.

**Verdict:** technically feasible and explicitly on the roadmap as a *later* option, not
a blocker. It buys binary size, latency headroom, and single-language simplicity at the
price of owning VT conformance and the Linux text/IME stack. For most goals the original
Path B (Rust core + xterm.js) is the better cost/benefit; go Rust-only only if native
performance or a webview-free footprint is a hard requirement — and when you do, treat it
as a renderer swap behind the existing seam, reusing every `crates/*` line and the
conformance harness.
