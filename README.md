# sampa2_graphics_terminal

A graphical terminal for Linux with an in-app command palette, a live man-page panel,
and a safe "preview as you type" pane — the **native, Rust-only** build (DESIGN Path C).

This is a *renderer swap* of the origin project
[`sampa_graphics_terminal`](https://github.com/mpbarbosa/sampa_graphics_terminal)
(Tauri + xterm.js): it reuses that project's seven headless crates unchanged and
replaces the webview frontend with a native `winit` + `wgpu` + `cosmic-text` renderer
driving an `alacritty_terminal` VT engine.

## Features

Beyond being a fast, real terminal (truecolor, images, tabs, scrollback — nvim/tmux/btop
all run), Sampa adds keyboard-driven helpers. All are **read-only** and honour
**insert-never-run** — anything that produces a command composes it at the prompt for you to
run, never on its own.

- **Enhance views — `Ctrl+Shift+D`**, dispatched on the command you've typed
  ([overview](docs/spec-enhance-views.md)):
  - `cd` → a **directory tree picker** → composes `cd <path>`
  - `du` → a **disk-usage treemap** (squarify + zoom) → composes `cd <path>`
  - `free` → a **memory gauge** (RAM/swap segmented bars) — display-only
  - `df` → a **disk-free gauge** (one bar per filesystem, fullest-first) — display-only
  - `ping <host>` → a **latency chart** (per-packet RTT sparkline + loss ticks) — display-only
  - `uptime` → a **load gauge** (1/5/15-min load bars scaled to CPU cores) — display-only
  - `netstat` / `ss` → a **connections table** (open sockets, state-coloured, listening-first) — display-only
  - *anything else* → the **`ps` output enhancement** (quiet columns / bars + sort / grouped
    inspector, by width). `Ctrl+Shift+I` decorates `ps` output **in place** in the scrollback.
- **AI (opt-in, one network surface):** `Ctrl+Shift+A` suggest a command · `Ctrl+Shift+X`
  explain the typed command · `Ctrl+Shift+G` screenshot the window and ask Claude to review it.
- **Split panes:** `Ctrl+Shift+R` split vertically · `Ctrl+Shift+O` cycle focus.
- **Also:** command palette (`Ctrl+Shift+P`), live man-page panel (`Ctrl+Shift+M`, which
  shows a **`gh`, `cargo`, `npm`, or `docker` command cheat-sheet** when the typed command is
  `gh`/`cargo`/`npm`/`docker`),
  preview-as-you-type (`Ctrl+Shift+E`), search (`Ctrl+Shift+F`), tabs, and a help overlay
  (`Ctrl+Shift+?`) that lists every binding.

## Status

**N0–N4 shipped & verified**, packaged (`.deb`/AppImage/`.rpm`), and released (see the
[GitHub releases](https://github.com/mpbarbosa/sampa2_graphics_terminal/releases)). The
native window is a real terminal — truecolor, cell backgrounds, cursors, keyboard/mouse,
selection + clipboard, 10k-line scrollback, inline images, tabs, and split panes — verified
against nvim/tmux/btop and the `esctest` conformance suite. The signature helpers above
(palette, man, preview, the enhance views, AI overlays) are all built. Remaining: the N5/N6
long tail (graphics/links/i18n/a11y edges, VT conformance, perf) — see
[docs/PLAN.md](docs/PLAN.md).

```bash
cargo run                       # native window running your $SHELL
cargo run -- --smoke            # headless cross-repo wiring + PTY round-trip check
cargo run -- --capture out.png  # offscreen render of a color demo (no display needed)
cargo run --release -- --bench  # VT ingest throughput benchmark (docs/perf.md)
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

- [docs/spec-enhance-views.md](docs/spec-enhance-views.md) — the `Ctrl+Shift+D` command
  visualisers (`ps` / `cd` / `du` / `free` / `df` / `ping` / `uptime`), with links to each view's spec.
- [docs/PLAN.md](docs/PLAN.md) — the phased development plan (N0–N6, v1).
- [docs/perf.md](docs/perf.md) — the VT ingest throughput benchmark (`--bench`) + baseline.
- [docs/rust-only-feasibility.md](docs/rust-only-feasibility.md) — why Path C is a
  renderer swap, not a rewrite.
- [docs/adr/](docs/adr/) — decisions: [0002](docs/adr/0002-core-code-sharing.md)
  (core code sharing), [0003](docs/adr/0003-native-stack.md) (native stack).
