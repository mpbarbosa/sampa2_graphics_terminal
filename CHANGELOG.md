# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/), and this project adheres to
[Semantic Versioning](https://semver.org/). Entries are derived from
[Conventional Commits](https://www.conventionalcommits.org/).

## [0.16.0] - 2026-11-13

**The disk-usage views fill in while `du` is still walking.** Until now the overlay was
blank for the whole scan and then appeared at once, which is why the scan budget had been
raised twice — from 6 s to 90 s — on the belief that `du` prints nothing until it has
finished. It doesn't: it prints each directory as it completes it, and measured on a 142 GB
`$HOME` the rows arrive steadily — 10 924 of them over 91 s, 11 % of those inside the first
9 s. That output was arriving all along and being discarded until the last row landed. It
is now streamed into the view and redrawn four times a second, so **a usable treemap of a
142 GB home is on screen at about 3 seconds** with the strip counting `scanning… N dirs`
while it fills. Sizes shown mid-scan are lower bounds that only rise, never figures that
jump about.

The same measurement retired the budget as a concept: the home directory that 90 s was
sized against had already grown to 91.1 s, one day later. **The timeout is now a 600 s
backstop** against a `du` wedged on a dead mount rather than the gate on the view, and if
it ever fires it keeps whatever arrived and says the picture is partial, instead of
discarding a nearly complete scan. Closing the overlay or re-scanning now **kills the
child**, so a cancelled scan stops walking a large tree unseen.

### Features

- Stream the `du` scan into the view as it arrives, with a dirs-scanned counter in the
  breadcrumb, lower-bound sizes while it runs, and cancellation that actually kills `du`

### Documentation

- Correct `spec-du-treemap.md` and `spec-du-rings.md`, which both stated that `du` prints
  nothing until the walk ends; record the progressive scan that `spec-du-rings.md` §8 had
  listed as a follow-up

## [0.15.2] - 2026-11-13

**Two dead ends in the `du` views, both of which told you nothing where they should have
said something.** In the rings, a sector's label was gated on the chord across it, and that
chord shrinks back to **zero** as a sector approaches a full turn — so the sectors with the
most room were the ones refused a label. Any directory holding one dominant child drew that
child blank while labelling its own smaller children: zoomed into `~/Documents`, the ring
carrying the entire 34.2 G view was the only unlabelled thing on screen. The sweep is now
clamped to half a turn before the chord is taken, so the width rises and then holds at the
band's diameter and no sector is labelled less generously than a narrower one beside it.
The chord remains an estimate rather than a bound — through a sector facing east a
horizontal label runs radially, where the room is the band thickness — and the code now
says so instead of implying otherwise.

Separately, the **preview panel's `✗` now points somewhere**. `du` and its neighbours are
deliberately off `sampa-preview`'s read-only allowlist *because* an enhance view supersedes
them, but the refusal never said that; it now names the chord that opens the matching view,
read from the live binding, so a `[keybindings]` rebind shows the right key and an unbound
action names none.

### Bug Fixes

- Label the sector that sweeps the whole dial — a full-turn ring reported zero room and was
  skipped by the label gate
- Point a preview refusal at the command's enhance view, using the live binding rather than
  a hardcoded chord

### Documentation

- Record the clamp in the rings spec, which had stated the chord rule that was wrong
- Add the `git status` preview to the enhance-views table

## [0.15.1] - 2026-11-13

**The `du` views are usable on a real home directory again.** The disk-usage scan had a
**6-second budget**, but `du` prints nothing until it has walked the whole subtree — so the
timeout was never a safety margin around a fast operation, it was the ceiling on what the
view could show at all, and any sizeable `$HOME` hit it and displayed only `du timed out
after 6s`. Measured here, a 132 GB home takes **67 s**, so the budget is now **90 s**. The
overlay already says `scanning <path> …` while it waits and `Esc` closes it, which is what
makes the longer wait tolerable; a progressive scan that fills the view as it goes remains
the real fix, and is recorded as a follow-up in the rings spec. The duration is now a single
named constant with the message formatted from it, rather than the number being written out
in three places. `df`'s own 6-second timeout is unchanged — a stale mount is a different
failure, and waiting longer there helps nobody.

### Bug Fixes

- Raise the `du` scan budget from 6s to 90s — a real `$HOME` never finished inside the old one

## [0.15.0] - 2026-11-13

**`du` now draws rings.** The disk-usage view gained a second layout — a **sunburst** over
the same scan, where a sector's angle is its share of its parent and each ring is one level
deeper — so the whole hierarchy is on screen at once instead of one level per zoom. **`t`
toggles** it against the existing squarified treemap; the scan, breadcrumb, zoom stack and
insert-never-run `cd` are all shared, and nothing extra is scanned to draw it. Since most
sectors are far too narrow to label, **hovering one puts its full path and size in the
breadcrumb strip**, and **clicking an outer ring descends every level between it and the
current view in one step** — so a deep chain of single large directories
(`.cache/ollama/models/blobs`) is one click away rather than four zooms. The sectors are
drawn by a new annulus-sector shader: the quad pass can only place axis-aligned rectangles,
so each one is submitted as its bounding quad and carved out per fragment, with its edges
eroded by a fixed number of pixels rather than a fixed angle so the seams stay even on
every ring.

### Features

- `du` disk-usage rings — a sunburst showing every scanned depth at once, `t` toggles

## [0.14.0] - 2026-10-17

**CJK, emoji and symbols finally render properly.** The native build gained real **font
fallback** — a glyph the primary font lacks is now drawn from a system face instead of a
tofu box — and **wide characters no longer leave a stray spacer cell**, so a CJK or emoji
run sits on the grid at its true two-cell width. The second theme is **clipboard
reliability**: **Ctrl+Shift+V works under Wayland** at last (`arboard` was being built with
the X11 backend alone), and a paste that finds nothing to paste — no clipboard backend, a
backend error, or simply an empty clipboard — now says which it was in a **transient
toast** instead of doing nothing at all. Also here: the **command palette can run a command
immediately** rather than only inserting it at the prompt.

### Features

- Font fallback for CJK / emoji / symbols — N5
- Wide-char (CJK/emoji) rendering — drop the spacer cell — N6
- Transient toast for a failed or empty paste (§13 paste safety)
- Command-palette run-immediately affordance — N4

### Bug Fixes

- Ctrl+Shift+V paste under Wayland — build `arboard` with `wayland-data-control`

### Testing

- Fix the weechat app-matrix case; full-matrix smoke pass recorded — N6

## [0.13.0] - 2026-09-30

Two themes headline this release. **Nested split layouts** — the panes are now a recursive
split tree, so splitting the focused pane the *other* way subdivides just that pane (columns
inside a row, rows inside a column, to any depth), each nested divider draggable to
re-proportion its own split. And the **kitty keyboard protocol is now complete**: on top of the
disambiguate level from 0.12.0, this adds **event types** (key release + repeat events),
**report-all-keys-as-escapes**, and **associated text** — all four enhancement flags, verified
byte-for-byte against the reference terminal. Rounding out the interaction polish: **drag a
split divider** to resize panes, **drag tabs** to reorder them, and a **✕ close button** on the
help overlay.

### Features

- Nested split layouts — a recursive split tree, splits nest to any depth — N3
- Kitty keyboard event types — report key release and repeat events — N1
- Kitty keyboard report-all-keys-as-escapes — N1
- Kitty keyboard associated text — the typed text on report-all escapes — N1
- Drag a split divider to resize the panes it separates — N3
- Drag tabs along the tab bar to reorder them — N2
- ✕ close button in the help overlay — N3

## [0.12.0] - 2026-09-29

Two headline features land. **Horizontal splits** — `Ctrl+Shift+B` stacks panes as rows,
alongside `Ctrl+Shift+R` for columns — and the **kitty keyboard protocol** (disambiguate
level), so modern TUIs like neovim get unambiguous key reporting (`Ctrl+I` ≠ Tab, `Ctrl+M` ≠
Enter, `Esc` is explicit). Also new: **opt-in shell integration** (`eval "$(sampa2
--shell-integration zsh)"`) that gives the man/preview/palette **exact** OSC-133 command
boundaries; **X11/Wayland PRIMARY selection** (select-to-copy, middle-click paste);
**block/rectangular selection** (Alt-drag); **command-palette frecency ordering**; **styled
underlines** (SGR `4:2`–`4:5` double/curly/dotted/dashed + underline colour); **Kitty graphics
z-index (`z=`) and relative placement (`P=`)**; **in-panel search** in the man/cheat-sheet
panel; mouse-wheel **preview scrolling**; a tab-bar **"+" new-tab button**; **multi-line paste
confirmation** for non-bracketed apps; keybinding-config **diagnostics**; and a non-gating
**VT-ingest perf trend in CI**.

### Features

- Horizontal splits — stack panes as rows (`Ctrl+Shift+B`) — N3
- Kitty keyboard protocol — disambiguate level — N1
- Opt-in zsh/bash shell integration (OSC 7 + OSC 133) — N4
- X11/Wayland PRIMARY selection — select-to-copy, middle-click paste — N1
- Block (rectangular) selection via Alt-drag — N1
- Command-palette frecency ordering — N3
- Styled underlines (SGR 4:2/4:3/4:4/4:5) + underline colour (SGR 58) — N5
- Kitty graphics z-index (`z=`) — stacking order + under-text placement — N5
- Kitty graphics relative placement (`P=`, `H`/`V`) — pin an image to another — N5
- In-panel search for the man / cheat-sheet panel — N4
- Scroll the command-preview panel with the mouse wheel — N4
- "+" new-tab button in the tab bar — N2
- Multi-line paste confirm for non-bracketed apps (§13 paste safety) — N1
- Diagnostics for bad `[keybindings]` config entries — N4
- Dynamic cursor shape via DECSCUSR (`CSI Ps SP q`) — N1
- Focus reporting (XTFOCUS / DECSET 1004) — N1
- Application keypad mode (DECKPAM) — N1
- Accent-underline the Ctrl-hovered hyperlink — N5
- Native URL link-preview overlay (`Ctrl+Shift+U`); git-status preview mirrored to native
- `aws` / `helm` / `kubectl` / `docker` / `npm` / `cargo` command cheat-sheets in the native build
- Wire the VT-ingest bench as a non-gating trend in CI — N6

### Bug Fixes

- Default the URL-preview keybinding to `Ctrl+Shift+L` (not `U`)

### Testing

- CJK/emoji width + Unicode snapshot render checks — N6
- Expand the real-app matrix (nvim/tmux/less + mc/emacs/weechat) — N6

### Miscellaneous Tasks

- Pin the last core path deps to git — reproducible build + CI

## [0.11.0] - 2026-08-23

A native **`netstat`/`ss` connections table** joins the enhance-view set (`Ctrl+Shift+D`):
open sockets as a **state-coloured, listening-first** table with process names and a
sockets / listening / established summary. The **`gh` cheat-sheet drills in by sub-command**
now — `gh repo` shows `gh repo`'s commands (a leaf like `gh repo view` shows its raw help) —
and **Kitty graphics honour `c=`/`r=` cell-box scaling**, so a placement sizes to the
requested columns/rows.

### Features

- netstat / ss connections table (`Ctrl+Shift+D`)
- gh cheat-sheet sub-command drill-in (`gh repo` → `gh repo`'s commands)
- Kitty graphics cell-box scaling (`c=` / `r=`) — N5 placement geometry

## [0.10.0] - 2026-08-23

A native **`gh` command cheat-sheet** — type `gh` and press `Ctrl+Shift+M` and the man panel
shows `gh`'s subcommands grouped and aligned (it has no useful `man gh`) instead of a man
page. Plus the **Kitty graphics image lifecycle** rounds out (N5): deletion (`a=d` — clear
all or by id) and the transmit-then-place workflow (`a=t` stores an image, `a=p` places it by
id), on top of the existing transmit-and-display.

### Features

- Native gh command cheat-sheet in the man panel
- Kitty graphics deletion (a=d) — clear all / by id
- Kitty graphics transmit + place by id (a=t / a=p)

## [0.9.0] - 2026-08-20

The `uptime` load gauge lands natively, **completing the seven-view enhance set** (`ps` / `cd`
/ `du` / `free` / `df` / `ping` / `uptime`) — type `uptime` and press the enhance chord for
1/5/15-minute load bars scaled to your CPU cores and banded by how close each is to capacity.
Also adds a headless **VT-ingest throughput benchmark** (`sampa2 --bench [MiB]`) that measures
the parse+grid ceiling (the `cat 50MB` path minus GPU present), establishing the N6 perf
baseline.

### Features

- Native uptime load gauge — completing the enhance-view set
- VT ingest throughput benchmark (--bench) — N6 perf baseline
- Mirror load gauge spec + wire sampa-uptimedec into the native build

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

