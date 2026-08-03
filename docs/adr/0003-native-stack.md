# ADR 0003 — Native stack selection (Path C)

- **Status:** Accepted (2026-08-02)
- **Context:** [PLAN.md §2](../PLAN.md) [Decision] rows for the native build — VT
  engine, windowing/event loop, GPU/present, text stack, clipboard, accessibility.
  This resolves them before N0 so the proof-of-life spike has a fixed target.

## Decision

Build the native frontend on this stack:

| Concern | Choice | Alternatives rejected |
|---|---|---|
| **VT engine** (Layer 2) | **`alacritty_terminal`** | `wezterm-term`, `vte`+own grid, from-scratch |
| **Windowing / event loop** (Layer 4) | **`winit`** | `tao`, gtk4-rs |
| **GPU / present** (Layer 3) | **`wgpu`** | raw GL, `softbuffer` |
| **Text: shape/raster/fallback** (Layer 3) | **`cosmic-text`** | `swash`+`harfbuzz`+`fontconfig` by hand |
| **Clipboard** | **`arboard`** (+ X11 PRIMARY) | `wl-clipboard` shell-out |
| **Accessibility** (N5) | **`AccessKit`** | accept a regression |

## Why

- **`alacritty_terminal`** — mature parser + grid + scrollback that powers Alacritty;
  conformance starts high, not at zero. The feasibility note calls this out as the
  single biggest item and says to reuse, not write, the VT engine
  ([rust-only-feasibility.md §2/§5](../rust-only-feasibility.md)). It owns the DEC
  ANSI state machine, modes, alt-screen, and mouse encoding we'd otherwise re-derive.
- **`winit`** — covers X11 + Wayland, DPI, resize, and IME events from one API, and
  lets us set **WM_CLASS at runtime** — the `--class` flag the origin's `tao` build
  could not implement (ROADMAP M3 note). Pairs with `wgpu`'s surface model.
- **`wgpu`** — portable GPU abstraction; the renderer draws cells as instanced
  textured quads from a glyph atlas plus a background-fill pass, coalesced to one draw
  per vsync (DESIGN.md §4.3, §7.2).
- **`cosmic-text`** — layout + shaping + `fontdb` + `swash` rasterization in one crate;
  owns Unicode width, CJK, emoji, ligatures, and **font fallback**. This is the part
  the webview gave free and where native terminals historically sink time
  (feasibility §4) — a single high-leverage dependency beats hand-wiring harfbuzz +
  freetype + fontconfig.
- **`arboard`** — cross-platform clipboard; X11 PRIMARY (middle-click paste) handled
  alongside. Honors the OSC-52 write gate that stays authoritative in the core.
- **`AccessKit`** — the webview supplied an a11y tree free; native must add one.
  Deferred to N5 with the rest of the long tail.

## Consequences

- N0 targets exactly this set; the workspace pins these as the frontend's direct deps.
- **The seam collapses:** with `alacritty_terminal` in-process, Layer 2 moves *into*
  the core and the frontend calls it directly — no IPC, no base64, no port. We keep
  the DESIGN.md §9 command/event *shape* only as an in-process API boundary so the
  core stays headless (ADR 0002 — the native VT/grid glue is Path-C-only code, not
  part of the shared crates).
- **Escape-hardening + DECRQCRA** that lived in Path B's `main.ts` move into this VT
  layer (N5, DESIGN.md §13).
- Each choice is a revisitable seam: `alacritty_terminal`→`wezterm-term`, `winit`→gtk4,
  `wgpu`→GL are swaps behind Layer boundaries, not rewrites.
