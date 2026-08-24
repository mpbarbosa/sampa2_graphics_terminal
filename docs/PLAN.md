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

- ✅ Cargo workspace (`crates/sampa-native`, bin `sampa`); depends on the shared crates
  via **git-pinned deps** (ADR 0002 — the path→git migration is complete, so no sibling
  checkout is needed and CI can build) — clean build here. `sampa --smoke` is a headless
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
  ✅ **application keypad (DECKPAM)** — a numpad key (detected by `KeyLocation::Numpad`)
  sends its SS3 code (`ESC O p`–`y` for 0–9, `ESC O j/k/l/m/n/o` for the operators, `ESC O M`
  for keypad Enter) when the app has enabled `TermMode::APP_KEYPAD`, else the plain digit.
  `app_keypad_code` unit-tested; Xephyr-verified with `cat -v` (numpad `1 2 + Enter` →
  `^[Oq ^[Or ^[Ok ^[OM`). ⬜ still: **kitty keyboard protocol**, IME/compose sequences
  beyond what the IM handles.
- ✅ **Mouse (§8.2):** **SGR 1006** (+ X10 fallback) reporting for press/release/drag/
  wheel when the app enables a mouse mode (1000/1002/1003); button + Shift/Alt/Ctrl
  modifier bits; **Shift** forces local selection over app grab. 3 unit tests.
- ✅ **Selection + clipboard (§8.3, first pass):** click-drag **Simple** selection via
  `alacritty_terminal::Selection`, highlighted in the render; **Ctrl-Shift-C** /
  auto-copy-on-release and **Ctrl-Shift-V** / **middle-click** paste via `arboard`;
  **bracketed paste** (2004) wrapping with embedded-`ESC[201~` stripping (§13);
  **double-click = word** (Semantic) / **triple-click = line** (Lines) via click-count
  timing, auto-copied. PNG + membership + click-granularity + word-selection tests.
  ⬜ still: **block** selection (modifier-drag), X11 PRIMARY split, multi-line paste confirm.
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

### N2 — Comfort: config-driven renderer (§7.3, §11) 🔨
- 🔨 Consume `sampa-config` (loaded from the XDG path at startup, else defaults):
  ✅ **theme** (16 ANSI + fg/bg/selection loaded into the VT color table via OSC so
  `resolve` returns them), ✅ **font family** (CSS-list primary, `glyphon` generics) +
  **size** (drives cell metrics), ✅ **scrollback lines** → `TermConfig`. Verified by
  unit tests (hex/family parse, palette→table) **and a config-aware `--capture`** (navy
  `#001830` bg + size-20 font applied). ✅ **live reload** — an mtime-poll watcher wakes
  the loop on a config edit (`UserEvent::ConfigReload`) and re-applies theme + font
  (re-measures the cell advance, rebuilds the text buffer, resizes term/PTY) without a
  restart. ✅ **cursor shape** (block inverts the cell; **bar**/**underline** drawn as a
  thin quad in the config cursor color) + **blink** (~530ms tick, resets on keypress,
  honors the config toggle). ✅ **visual bell** — `BEL` from the active tab flashes a
  bright (cursor-color) border frame for ~120 ms, self-clearing via a short redraw burst
  (no audible bell). ✅ **ligatures toggle** — `font.ligatures` (default off, safest for
  grid alignment) selects `cosmic-text` `Shaping::Advanced` vs `Basic` for the grid text;
  applied live on config reload. ✅ **background transparency** — a native-only top-level
  `opacity` key (0–1; parsed + stripped before the strict `sampa-config` parse, since both
  it and `Window` `deny_unknown_fields`) makes the window translucent: the surface picks a
  premultiplied/postmultiplied alpha mode when the compositor supports one and the frame
  clears at that alpha (default-bg cells transparent, colored cells stay opaque). Needs a
  compositing WM; a change takes effect on restart. `parse_opacity`/`strip_native_keys`
  unit-tested; verified via `--capture` alpha channel. ⬜ still: fontconfig fallback;
  scrollback change still needs a restart (fresh `Term`).
- ✅ **Tabs** — multi-session: each tab owns its VT state/PTY/image-layer/pump; `App`
  keeps active-session pointers (re-pointed on switch). **Ctrl+Shift+T** new,
  **Ctrl+Shift+W** close (reaps the shell; quits on the last), **Ctrl+Tab** /
  **Ctrl+Shift+Tab** cycle; window title shows the active tab + `[i/n]`; resize applies
  to every tab. A **visual tab bar** shows when >1 tab open: equal-width segments,
  active one highlighted (bg-filled + accent underline) with dimmed inactive labels,
  **click-to-switch**; the grid offsets below it (and reflows on the 1↔2 boundary).
  Geometry/label/close-index math unit-tested; bar rendered + verified via `--capture`.
  ✅ **splits** — Ctrl+Shift+R splits a vertical pane, Ctrl+Shift+O cycles focus (per-pane
  render + resize + input routing; single-pane pixel-identical). ⬜ still: horizontal splits,
  nested layouts, drag-resize, tab reordering/drag, a “＋” new-tab affordance.
- ✅ **Search overlay** — **Ctrl+Shift+F** opens a bottom search bar; incremental
  regex match over the whole buffer (scrollback included) via `alacritty_terminal`'s
  `RegexSearch`/`RegexIter`. All matches highlighted, the current one brighter;
  **Enter**/↓ next, **Shift+Enter**/↑ previous (wrapping), the match scrolled into
  view; the bar shows `i/n` (or “no matches”). **Esc** closes. Grid text is clipped
  above the bar and inline images are scissored to the grid so nothing bleeds into the
  chrome. `find_matches`/nav/format are unit-tested (incl. a real-`Term` scrollback +
  regex test); bar rendered + verified via `--capture`.
- ✅ **Zoom** — **Ctrl +/−/0** change the font size at runtime (clamped 6–48 pt; 0
  resets to the configured size), rebuilding renderer metrics + reflowing the grid/PTY.
- ✅ **Help overlay + keybinding config** — **Ctrl+Shift+?** toggles a modal shortcut
  list to the [help-overlay spec](spec-help-overlay.md): rows built each open, chords
  **prettified** per §4. Closes on the (bound) chord, **Esc** (scoped so it never steals
  Esc from palette/search/panels), or a **backdrop click**. Now fully **config-driven**:
  an `Action` enum + `ACTIONS` table are the single source; `Keybindings::load()` applies
  `[keybindings]` overrides from the config file over the defaults; every app shortcut
  goes through one `dispatch(action)` (chord matched by `action_for`, layout-tolerant via
  `normalize_key` folding shifted symbols to a base token). So **rebinding a key changes
  both its trigger and its help row** (spec §6) — verified end-to-end (`[keybindings]`
  override → rendered help shows the new chord). `parse_chord`/`normalize_key`/
  `action_for`/`help_rows`/rebinding all unit-tested (66 tests). ⬜ still: a ✕ button;
  chord *validation* diagnostics.

**Exit:** a config edit hot-reloads theme/font/cursor; a Powerline + truecolor prompt
renders; tabs run independent shells; search highlights scrollback matches.

### N3 — Linux citizen (§12, §16) ✅
- ✅ Wire `sampa-cli`: `-e`/`--`, `--working-directory`, `--title`, `--hold`, `--login`,
  `--config`, `-h`/`-V`, and **`--class`/WM_CLASS set at runtime** (native win, unlike the
  origin's `tao` limitation) — verified via `xprop`.
- ✅ `.desktop` (`TerminalEmulator` category) + `update-alternatives` for
  `x-terminal-emulator` in the `.deb` `postinst` + **`xdg-terminal-exec`** integration
  (`X-TerminalArg{Exec,Title,AppId,Dir,Hold}` keys → the sampa-cli flags; verified with
  `xdg-terminal-exec --print-cmd`).
- ✅ Packaging: **`.deb`** (`lintian`-clean, `packaging/build-deb.sh`), **AppImage**
  (single no-install file, `packaging/build-appimage.sh`), and **`.rpm`**
  (`packaging/build-rpm.sh`) — a **single self-contained binary** with **no webkit runtime
  dependency** (a real gain over Path B's SONAME deps). No Flatpak (origin [ADR 0001]
  applies — sandbox fights a host-shell terminal).

**Exit:** ✅ verified in a clean `ubuntu:26.04` container — `apt install ./sampa2.deb`
resolves deps, `sampa2 --version`/`--smoke` run with no build toolchain, the
`x-terminal-emulator` alternative registers, `man sampa2` installs, purge deregisters
cleanly; the AppImage runs standalone (`--appimage-extract-and-run`); WM_CLASS verified via
`xprop`. The test also caught + fixed a real bug: the `.deb` `Depends: libc6` had no version
floor, so it "installed" on 24.04 then died with `GLIBC_2.43 not found` — now pinned
`libc6 (>= <detected>)`, so apt refuses too-old systems gracefully (see packaging/README).

### N4 — Signature features, natively drawn (§10) ✅  *(palette · man · preview all shipped)*
Services already exist; only the UI is new. Draw panels/overlays in the wgpu scene.

- ✅ **Palette** (`sampa-palette`): `Ctrl+Shift+P` opens a full-width dropdown below
  the tab bar; `list_executables($PATH)` enumerated once on open. The matcher implements
  the [command-palette search spec](spec-command-palette-search.md): whitespace-split
  **AND tokens**, tiered **exact > substring > subsequence** scoring (prefix/word-boundary
  bonuses, gap-penalized subsequence), best-first (stable ties), capped at `PALETTE_MAX`
  = 60 — so the grep family (`grep`, `grepdiff`, `git-grep`, `egrep`…) ranks above any
  scattered match, `git grep` → `git-grep`, `doc comp` → `docker-compose`. **Matched
  characters are highlighted** (bold + accent) from the reported hit indices. ↑/↓ move
  the selection (scrolled to stay visible), the bar shows the query + caret, `Esc`
  closes. **Enter inserts `"<cmd> "`** at the prompt (never auto-runs). Grid text is
  clipped below the panel and images/decorations/cursor are suppressed under it so nothing
  shows through. `score_token`/`score_command`/`filter_commands`/`palette_window`
  unit-tested (incl. spec acceptance cases); rendered + verified via `--capture`.
  ⬜ still: run-immediately affordance, recent/frecency ordering.
- ✅ **Man panel** (`sampa-man`): `Ctrl+Shift+M` opens a bottom panel with the man page
  for the **first token of the current command line** (tracked from typed keystrokes,
  `sudo`/`command`/`\` stripped; reset on Enter). `man -P cat <cmd>` runs on a
  **background thread** (never blocks the UI) and its sanitized output streams back via
  `UserEvent::ManReady`; ↑/↓ · PgUp/PgDn · Home scroll, `Esc` closes, "No man page"
  when absent. The panel clips the grid above it and scissors images out. `first_command_token`
  unit-tested; rendered + verified via `--capture` (`sampa-man`'s own sanitize/validate
  tests carry over). ⬜ still: OSC-133 prompt-boundary reset (opt-in shell hooks),
  auto-show on debounce, in-panel search.
- ✅ **Preview panel** (`sampa-preview`): `Ctrl+Shift+E` toggles a live bottom panel that
  **safely auto-runs** the current command as you type. Keystrokes debounce **550 ms**;
  only the settled line runs (a `preview_gen` token supersedes stale requests), off the
  UI thread. `sampa_preview::run_preview` is the **authoritative, untouched gate** —
  it refuses anything that can write/chain/redirect/substitute and runs the survivor in
  a throwaway zsh (cwd = the session's `/proc/<pid>/cwd`, stdin closed, 2 s timeout, 64 KB
  cap). Output shows in the panel; a rejection shows its reason in the header; an empty
  line (**after Enter**) clears it. **Re-asserted filesystem-verified**: a native
  integration test drives the exact `run_preview` call on `rm`/`mv`/redirect/`find -delete`/
  `&&`-chains and proves the victim file is byte-for-byte untouched while `cat` runs.
  ⬜ still: scroll, OSC-133 prompt-boundary reset for exact command capture.
- OSC 7/133 already handled by `sampa-shellint`; ship the opt-in zsh/bash hooks.

**Exit (§17):** palette inserts (not runs); man opens/closes correctly; preview
refuses writes (file provably untouched) and clears on Enter.

### N5 — Graphics, links, i18n, a11y 🔨  *(the native long tail — §19)*
- 🔨 **Images:** ✅ **iTerm2 inline images (OSC 1337)** end-to-end — an OSC scanner
  extracts the payload (chunk-split-safe), `image` decodes it, and a wgpu **textured
  pipeline composites** it into the scene at the cursor; **§13 OOM caps** (max dims,
  source bytes, in-flight OSC bytes, live-image count with oldest-evicted). 3 unit tests
  + PNG. ✅ **Sixel (DCS)** — a parallel `SixelScanner` captures the DCS payload (which
  alacritty ignores; `parse_sixel` rejects non-sixel DCS so it coexists with DECRQSS),
  and a pure two-pass rasterizer decodes it (color select/define `#`, RLE `!`, CR `$`,
  band-LF `-`, `?`..`~` sixels) into the **same `DecodedImage`/`ImageStore`/textured-quad
  path** as OSC 1337, honoring the dim/pixel caps. 4 unit tests (pixels/color/RLE/bands,
  scanner extraction, non-sixel rejection) + a pump smoke (consumed cleanly, no text leak)
  + a real-sixel `--capture` render. ✅ **Scroll-out lifecycle** — each image records the
  scrollback depth at insert (`base_history`); `image_row` maps its content's stable
  position into the current view, so an image **rides up with its text as new output
  scrolls in** and off the top (instead of sticking to a fixed screen row), while still
  following scrollback when you scroll up. `image_row` unit-tested (fresh / scrolled-in /
  scrolled-view / off-top / inserted-with-history). ✅ **Kitty graphics (APC)** — a
  parallel `KittyScanner` captures `ESC _ G … ST` APCs and **reassembles chunked
  transmissions** (`m=1` … `m=0`); `parse_kitty` decodes the result — **PNG** (`f=100`),
  raw **RGBA** (`f=32`), raw **RGB** (`f=24`, alpha filled) with `s`×`v` dims — for the
  immediate transmit+display action (`a=T`) into the shared image path, and an
  `ESC _ G i=<id>;OK ST` **ack** is sent so clients like `icat` don't block. ✅ **deletion
  (`a=d`)** — the `i=` image id is tracked per placement, and a delete request removes
  images: `d=a`/`d=A` (or `d` absent) clears **all** inline images (any source — an app
  clearing images wants a clear screen), `d=i`/`d=I` with `i=<id>` clears **by id**; other
  selectors are ignored. 5 unit tests (adds control parse, raw RGBA/RGB, action gate + ack,
  chunk reassembly, delete-by-id/all) + a pump smoke + a **chunked-PNG `--capture` render**;
  the transmit→delete round-trip is Xephyr-verified. ✅ **transmit + place by id
  (`a=t`/`a=p`)** — `a=t` decodes and **stores** the image under its `i=` id without
  displaying (capped store, oldest evicted); `a=p` **places** a stored image by id at the
  cursor. `a=T` still transmits-and-displays, and now also retains the image so it can be
  re-placed. `decode_kitty_payload` (the format decode) is split from the display gate and
  unit-tested for `a=t`; the transmit-only→place round-trip is Xephyr-verified (image hidden
  on `a=t`, shown on `a=p`). ✅ **cell-box scaling (`c=`/`r=`)** — a placement's display size
  follows the requested columns/rows against the live cell metrics (both → the exact box, one
  → that axis fixed and the other by source aspect, neither → natural), and `r=` rows are
  reserved exactly; the source texture stays native and the quad samples it.
  `image_display_size` unit-tested; Xephyr-verified (an 80×80 square placed `c=30,r=3` renders
  as a wide 30×3-cell strip with text flowing below). ⬜ still: z-index (`z=`) and relative
  placement.
- ✅ **OSC 8 hyperlinks:** OSC-8 tracked (via `alacritty_terminal`), rendered
  **underlined**, with a strict **http/https scheme gate** (§13 — never `file:`/`javascript:`);
  auto-opens nothing. ✅ **plain-URL detection** too (`url_at`: whitespace token under the
  cursor → extract http/https, trim punctuation) so links work without OSC-8. ✅ **in-window
  confirm/preview modal** — Ctrl+click raises a centered card showing the **real target**
  (the visible OSC-8 text can differ from the URI) resolved by **Enter/`o`** (open via
  `xdg-open`) or **Esc/`n`** (cancel), replacing the old open-and-retitle. ✅ **hover
  affordance** — Ctrl+hover over a safe link shows a **pointing-hand cursor** (recomputed on
  move and on Ctrl press/release; only touches the grid lock while Ctrl is down). 3 unit
  tests + PNG; confirm modal + no-auto-open Xephyr-verified.
- 🔨 **IME / dead keys / compose:** IME is enabled (`set_ime_allowed`) and winit's `Ime`
  events are wired — **Preedit** is stored and drawn **underlined at the cursor** (a
  cursor-tinted strip + accent underline; CJK renders via `cosmic-text` fallback),
  **Commit** sends the composed text to the shell (and feeds the man/preview input line),
  Enabled/Disabled clear it. The **candidate window is positioned at the cursor**
  (`set_ime_cursor_area`), tracked each frame via a new `Snapshot.cursor_rc` that reports
  the cursor cell for **any** cursor style. Preedit render verified via `--capture`
  (`にほんご` underlined); `cursor_rc` unit-tested; boots with IME enabled. ✅ **preedit
  caret** — winit's `Ime::Preedit` byte range is honored: a bright caret bar marks the IME
  cursor within the composition (`preedit_caret_cells` maps the byte offset to a cell,
  clamped/UTF-8-safe, unit-tested). ⏳ live IBus/fcitx composition can't be exercised
  headlessly. ⬜ still: dead-key/compose sequences beyond what the IM handles.
- ✅ **Accessibility (AccessKit):** the window now exposes an OS accessibility tree — a
  `Window` root labeled with the tab title + a `Role::Terminal` child whose value is the
  live screen text — so a screen reader (Orca/AT-SPI on Linux, and the other platforms
  `accesskit_winit` covers) can read the terminal. The adapter is created before the
  window is shown (start hidden → attach → reveal) and observes every window event; the
  tree is pushed via `update_if_active`, which **no-ops unless a client is attached**, so
  there's zero cost otherwise. `a11y_tree` is unit-tested; boots cleanly with the adapter
  under headless Xvfb (graceful with no session bus). ⬜ still: **caret/selection**
  exposure, per-line text nodes, action handling — screen-reader read-back can't be
  verified in this environment.
- 🔨 **Escape hardening in the VT layer (§13)** — a real `EventProxy` listener
  (replacing `VoidListener` for the live terminal) funnels VT events to the main loop:
  - ✅ **query replies routed** — DA/DSR/DECRQSS `PtyWrite` and OSC 4/10/11 color
    requests now answer the app (a correctness win — `VoidListener` silently dropped
    them); color replies use fixed palette values, never attacker input.
  - ✅ **OSC-52 write gate** — surfaced as `ClipboardStore` and gated by a **consent
    modal** (default): a clipboard write pops a centered card showing the byte count and a
    sanitized one-line preview, resolved by **Enter/`y`** (allow once), **`a`** (allow this
    session), or **Esc/`n`** (deny). `SAMPA_OSC52=allow` writes through without a prompt and
    `=deny` drops silently. **OSC-52 reads dropped** (no clipboard exfiltration to the PTY).
  - ✅ **title (OSC 0/2) sanitized** — control chars stripped, capped at 256.
  - ✅ **synchronous, in-order reply path** — replies (DA/DSR/DECRQSS/color, DECRQCRA)
    are written to the PTY by the parser thread in stream order via a shared
    `Arc<Mutex<PtyHandle>>`, not routed through the UI loop (a query must be answered
    before the next byte; verified: CPR after `CUP(3,6)` returns `ESC[3;6R`).
  - ✅ **DECRQCRA** (`CSI … * y`) — an incremental scanner splits the output stream so
    the rectangular-area checksum sees the exact grid state, replying
    `DCS Pid !~ HHHH ST` (raw 16-bit codepoint sum, empty = 0x20 — the
    `--xterm-checksum 334` convention). 6 unit tests (title/DA/OSC-52 ×2, scanner, checksum).
  - ✅ **interactive OSC-52 consent modal** — replaces the env-only toggle with a per-write
    prompt (allow once / allow session / deny), reusing the centered-card path; the payload
    preview strips control chars and clips to one line. `osc52_preview` unit-tested;
    Xephyr-verified end to end (allow writes the X clipboard, deny leaves it untouched).

**Exit:** sixel renders within caps, oversized rejected not OOM; links need a click
and show target; a CJK/emoji/compose input test passes; a screen reader sees the grid.

### N6 — Conformance, perf, v1 (§14, §17) 🔨
- 🔨 **esctest:** harness wired ([tools/conformance/](../tools/conformance/README.md)) —
  fetches pinned esctest2, runs headless under Xvfb against the native binary via
  `sampa -e python3 esctest.py`, scores with `--xterm-checksum 334`. **Baseline: 318
  passed / 41 known-bug / 209 failed** (gate: don't regress) — up from 45 by fixing
  suite-wide desyncs (**color queries** live, +202; **DECSTR** soft-reset, +4), adding
  **DECRQSS** status-string replies (+5), answering **XTWINOPS size/state reports** +
  pixel/DECSLPP resize (+11, `XtermWinops` 0/28 → 11/28), reporting **DECRQM
  permanently-reset modes** correctly (+13, `DECRQM` 8/33 → 21/33), answering all
  **DECDSR device-status reports** (+11, 0/11 → 11/11), translating **DECSET `?1048`**
  save/restore-cursor to DECSC/DECRC (+4), and translating **selective erase**
  (DECSED/DECSEL `CSI ? Ps J/K`) to plain ED/EL for the non-protection cases (+12).
  DECRQCRA correct (unit-tested + PTY-probed:
  `CUP(3,6)`→`ESC[3;6R`). **Now past the origin's ~305**; the remaining failures are
  genuine xterm feature coverage `alacritty_terminal` lacks (DECSCA protection, left-right
  margins, WM ops that need a live window manager) — ranked roadmap in
  [tools/conformance/](../tools/conformance/README.md).
- 🔨 **real-app matrix** (`app_matrix_smoke`, `--ignored`): ✅ echo / ls-color / seq-wrap /
  python / **vim** + **nvim** alt-screen / **htop** / **tmux** (status bar) / **less** (pager),
  each asserted against a rendered marker and **skipped when not installed** — added cases
  for the still-absent **mc** / **emacs -nw** / **weechat** so they run wherever present.
  ✅ **render checks** — end-to-end VT→grid/snapshot tests for **truecolor** (`38;2` → exact
  RGB), **256-indexed**, **inverse**, **sixel** (pixels/RLE/bands), and **CJK/emoji width**
  (双-width cells + spacer; a `café 日本 🎉` grid→snapshot round-trip keeps every code point).
  ⬜ still: **vttest** manual smoke.
- 🔨 **Perf:** the native ceiling is the payoff — ✅ **VT ingest throughput** benchmark
  (`sampa2 --bench [MiB]`) measures the parse+grid hot path (the `cat 50MB` ceiling minus GPU
  present) on a deterministic representative workload, reporting MiB/s, line rate, and the
  100k-line scrollback RSS. Baseline **~82–87 MiB/s · ~1.2 M lines/s · ~193 MiB scrollback**
  (`bench_workload` unit-tested), documented in [perf.md](perf.md). ⬜ still: typometer
  added-input-latency **< one frame** (needs the live window), and wiring the bench as a
  non-gating **trend in CI**.

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
