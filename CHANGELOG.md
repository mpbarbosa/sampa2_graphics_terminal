# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/), and this project adheres to
[Semantic Versioning](https://semver.org/). Entries are derived from
[Conventional Commits](https://www.conventionalcommits.org/).

## [0.8.0] - 2026-08-15

Escape-hardening for the two paths where the terminal reaches outside the sandbox — the
system clipboard and the web browser — each now gated by an in-window consent modal. An
OSC-52 clipboard write prompts before it lands (allow once / allow this session / deny), and
Ctrl+clicking a hyperlink shows the real target before it opens (the visible OSC-8 text can
differ from the URI), with a pointing-hand cursor on Ctrl+hover. Packaging also stops
shipping stale binaries — `build-deb.sh` always rebuilds and verifies the version.

### Features

- Interactive OSC-52 clipboard consent modal (N5 escape hardening)
- Hyperlink open-confirm modal + Ctrl+hover hand cursor (N5)

### Bug Fixes

- **packaging:** always rebuild + verify the binary version in build-deb.sh

## [0.7.0] - 2026-08-15

The `df` disk-free gauge and `ping` latency chart land natively — completing the six
enhance-shortcut views (`ps` / `cd` / `du` / `free` / `df` / `ping`). Type `df` for
proportional per-filesystem bars (fullest-first, banded by use%), or `ping <host>` for a
per-packet latency sparkline (with loss ticks and a min/avg/max/mdev summary), and press the
enhance chord. Both are display-only.

### Features

- Native df disk-free gauge + ping latency chart
- Mirror df gauge spec + wire sampa-dfdec into the native build
- Mirror ping chart spec + wire sampa-pingdec into the native build

### Documentation

- Consolidate the enhance-views feature set

## [0.6.0] - 2026-08-15

AI window analysis — press Ctrl+Shift+G to screenshot the terminal window and have Claude
review what's on screen.

### Features

- **native:** AI window analysis — screenshot → Claude review (Ctrl+Shift+G)

## [0.5.0] - 2026-08-15

The `free` memory gauge lands natively — completing the four enhance-shortcut views (`ps` /
`cd` / `du` / `free`). Type `free` and press the enhance chord for proportional RAM/swap
segmented bars.

### Features

- Native free memory gauge
- Mirror free gauge spec + wire sampa-freemem into the native build

## [0.4.0] - 2026-08-15

Vertical split panes land in the native build — `Ctrl+Shift+R` splits a shell to the right,
`Ctrl+Shift+O` cycles focus — built on a viewport refactor that keeps single-pane rendering
pixel-identical.

### Features

- Native vertical split panes (N2)

### Refactor

- **splits:** Render the grid through a horizontal viewport

## [0.3.0] - 2026-08-15

The `du` disk-usage treemap lands natively — a squarified layout with click-zoom,
breadcrumb navigation, and `cd`-on-select — completing the three enhance-shortcut
decorators (`cd` picker / `ps` enhancement / `du` treemap).

### Features

- Native du disk-usage treemap
- Mirror du treemap spec + wire the sampa-dumap core into the native build

### Miscellaneous Tasks

- Pin sampa-preview at the du-drop rev (ADR 0002)

## [0.2.0] - 2026-08-14

First tagged release. Consolidates all work since the `0.1.0` development baseline —
the native (Path C) terminal grew from the N0 echo spike into a config-driven,
packaged emulator with the signature overlays (palette, man, preview, ps enhancement,
cd tree picker, AI suggest/explain).

### Features

- native AI command explainer (Ctrl+Shift+X)
- native cd directory tree picker
- native ps in-place render — decorate ps output where it sits
- native ps inspector — incremental / filter
- native ps enhancement 1c — grouped inspector
- native ps enhancement 1b — signal bars + live sort
- native ps enhancement 1a — decorate the last ps output
- **preview:** OSC 133 command-start for exact grid reading
- **n3:** xdg-terminal-exec integration (freedesktop default terminal)
- **n3:** .rpm packaging (Fedora / RHEL / openSUSE)
- **n3:** AppImage packaging (single-file, no install)
- **n3:** wire the sampa-cli launcher flags into the native build
- **n3:** .deb packaging + fix stale help-overlay doc status
- wire ps-decorate core into the native build + mirror its spec
- **preview:** read the command off the grid, not keystrokes
- **ai:** render the suggester as a centered floating card
- **ai:** Phase 3 — consume context redaction from sampa-ai
- **ai:** Phase 2 overlay chrome — highlighted command + copy
- **ai:** wire opt-in Claude command suggester (Phase 0-1)
- dock right-click actions (New Window, Edit Config)
- app icon + desktop entry + WM_CLASS
- **N2:** background transparency (opacity)
- **N5:** IME preedit caret sub-range
- **N2:** ligatures toggle (font.ligatures → shaping)
- **N2:** visual bell (BEL border flash)
- **N6:** DECRQM modifiable modes via shadow state (esctest 307→318)
- **N6:** selective erase → ED/EL translation (esctest 295→307)
- **N5:** kitty graphics protocol (APC)
- **N5:** IME / compose input (preedit + commit)
- **N5:** accessibility tree via AccessKit
- **N2:** config-driven keybindings (completes help-spec §6)
- **N5:** sixel graphics (DCS)
- **N2:** keyboard-shortcut help overlay + zoom (Ctrl+Shift+?)
- **N4:** safe command preview panel (Ctrl+Shift+E)
- **N4:** man panel (Ctrl+Shift+M)
- **N4:** palette matcher to spec (tiers + highlighting)
- **N4:** command palette (Ctrl+Shift+P)
- **N2:** search overlay (Ctrl+Shift+F)
- **N2:** visual tab bar (click-to-switch)
- **N2:** tabs (multi-session)
- **N2:** cursor shape (block/bar/underline) + blink from config
- **N2:** live config reload
- **N2:** wire sampa-config — theme, font, scrollback
- word/line selection (double/triple-click)
- **conformance:** DECDSR device-status reports + DECSET ?1048
- **conformance:** DECRQM permanently-reset mode replies
- **conformance:** XTWINOPS size/state reports + pixel/DECSLPP resize
- **conformance:** DECRQSS status-string replies
- plain-URL detection for Ctrl-click (no OSC-8 needed)
- inline images (iTerm2 OSC 1337) — decode + GPU composite
- OSC 8 hyperlinks — render + Ctrl-click to open
- honor CSI 8;h;w t (XTWINOPS resize)
- **conformance:** resolve OSC 4/10/11 color queries against the live table
- native Rust terminal — N0 through N5 slices

### Bug Fixes

- **cd:** clear the whole input line before composing cd <path>
- **deb:** pin libc6 to the built glibc; N3 clean-VM exit test done
- **render:** draw control chars (tab) as blank, not a tofu box
- **N5:** image scroll-out lifecycle (ride with content)

### Documentation

- mirror the cd tree picker spec from the reference build
- mirror the explain-direction section (§7) from the reference build
- spec the native ps enhancement 1a slice
- **ai:** add AI-overlay manual sign-off checklist
- mirror AI-integration feasibility/as-built doc from reference build
- add keyboard-shortcut help-overlay spec
- add command-palette search spec (target for the native N4 palette)
- add Rust-only feasibility assessment (moved from sampa_graphics_terminal)

### Build System

- **ai:** re-pin sampa-ai to origin/main after #26 merged
- **ai:** pin sampa-ai to a git rev for CI reproducibility

### Miscellaneous Tasks

- wire the cd tree picker core (sampa-fsnav) into the native build
- bump sampa-ai pin to include the command explainer
- bump ps-decorate pin to 1b/1c (bars + inspector) + refresh spec status
- line-tables-only debuginfo for the dev profile
- rename binary to sampa2 + own config path

