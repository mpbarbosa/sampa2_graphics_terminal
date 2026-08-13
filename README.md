# sampa2_graphics_terminal

A graphical terminal for Linux with an in-app command palette, a live man-page panel,
and a safe "preview as you type" pane — the **native, Rust-only** build (DESIGN Path C).

This is a *renderer swap* of the origin project
[`sampa_graphics_terminal`](https://github.com/mpbarbosa/sampa_graphics_terminal)
(Tauri + xterm.js): it reuses that project's seven headless crates unchanged and
replaces the webview frontend with a native `winit` + `wgpu` + `cosmic-text` renderer
driving an `alacritty_terminal` VT engine.

## Status

**N0 (proof of life) — shipped & verified.** A native window runs a live shell:
`pty-core` → `alacritty_terminal` → `glyphon` render, with resize and keyboard input.

Colors, bold/dim/inverse, truecolor, cell backgrounds, and a block cursor render
(N1 color pass); keyboard/mouse/selection/scrollback are in progress.

```bash
cargo run                       # native window running your $SHELL
cargo run -- --smoke            # headless cross-repo wiring + PTY round-trip check
cargo run -- --capture out.png  # offscreen render of a color demo (no display needed)
cargo test                      # unit tests (VT seam, colors, keyboard, mouse, selection, scrollback)
cargo test -- --ignored         # app-matrix + ^C/resize smokes against real programs (PTY→VT)
```

## Install

Build a self-contained `.deb` and install it as a normal terminal (on `$PATH`, in the app
menu, registered as an `x-terminal-emulator` alternative):

```bash
./packaging/build-deb.sh
sudo apt install ./target/sampa2_*.deb
```

Or grab a single-file, no-install **AppImage**:

```bash
./packaging/build-appimage.sh
./target/sampa2-*-x86_64.AppImage
```

An `.rpm` (`./packaging/build-rpm.sh`) is available too. See
[packaging/README.md](packaging/README.md) for details (icons, man page, deps).

## Docs

- [docs/PLAN.md](docs/PLAN.md) — the phased development plan (N0–N6, v1).
- [docs/rust-only-feasibility.md](docs/rust-only-feasibility.md) — why Path C is a
  renderer swap, not a rewrite.
- [docs/adr/](docs/adr/) — decisions: [0002](docs/adr/0002-core-code-sharing.md)
  (core code sharing), [0003](docs/adr/0003-native-stack.md) (native stack).
