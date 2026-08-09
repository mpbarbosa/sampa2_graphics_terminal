//! Sampa native (Path C) — entry point.
//!
//! **N0** proved the pipeline (PTY → `alacritty_terminal` → `glyphon`) in a native
//! window. **N1** is filling in the "real terminal" contract; this file currently
//! covers the renderer's **color + cursor** pass:
//!
//! - a solid-quad wgpu pass draws per-cell **background colors** and a **block
//!   cursor** (aligned to the measured monospace advance);
//! - `glyphon` rich-text draws per-cell **foreground colors** with bold/italic;
//! - ANSI named/indexed/truecolor are resolved against a built-in 256-color palette,
//!   honoring any OSC 4/10/11 overrides the VT engine has recorded.
//!
//! Still N1-pending: full keyboard encoding (DECCKM/CSI-u), mouse, selection,
//! scrollback scrolling, underline/strikethrough, damage-tracked redraw.
//!
//! Modes: `sampa` (window), `sampa --smoke` (headless wiring check),
//! `sampa --capture <png>` (offscreen render of a color demo — CI screenshot / proof).

mod smoke;

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;

use alacritty_terminal::event::{Event as TermEvent, EventListener, VoidListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionRange, SelectionType};
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};
use sampa_palette::list_executables;
use accesskit::{Node as AccessNode, NodeId as AccessNodeId, Role as AccessRole, Tree as AccessTree, TreeUpdate};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::vte::ansi::Processor;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor, Rgb};
use anyhow::Result;
use bytemuck::{Pod, Zeroable};
use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, Style,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight,
};
use pty_core::pty::{spawn, PtyEvent, PtyHandle, SpawnConfig};
use sampa_config::CursorStyle;
use wgpu::util::DeviceExt;
use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

// --- Fixed metrics (N1: single monospace font) -------------------------------
const FONT_SIZE: f32 = 15.0;
const LINE_HEIGHT: f32 = 18.0;
const PAD: f32 = 6.0;
/// Height of the visual tab bar, shown only when more than one tab is open.
const TAB_BAR_H: f32 = 26.0;
/// Height of the search bar (overlaid at the bottom while search is open).
const SEARCH_H: f32 = 22.0;
/// Highlight backgrounds for search matches (all) and the current match.
const SEARCH_MATCH_BG: [u8; 3] = [0x66, 0x5c, 0x1e];
const SEARCH_CURRENT_BG: [u8; 3] = [0xe0, 0xa0, 0x22];
/// Cap on matches tracked per search, to bound work on huge scrollback.
const SEARCH_MAX_MATCHES: usize = 2000;
/// Command palette: rows shown at once, and the cap on ranked results kept (spec §5).
const PALETTE_VISIBLE: usize = 10;
const PALETTE_MAX: usize = 60;
/// Man panel: the most body lines shown at once (fewer if the window is short).
const MAN_VISIBLE: usize = 18;
/// How long the visual bell border flashes.
const BELL_FLASH: std::time::Duration = std::time::Duration::from_millis(120);
/// Preview panel: max body lines shown, and the debounce before a settled line runs.
const PREVIEW_VISIBLE: usize = 12;
const PREVIEW_DEBOUNCE_MS: u64 = 550;
/// Font-size bounds for zoom (Ctrl +/−/0).
const FONT_SIZE_MIN: f32 = 6.0;
const FONT_SIZE_MAX: f32 = 48.0;

/// An app-level keyboard action, bound to a chord via [`Keybindings`] and dispatched
/// centrally. The single source of truth for what the non-PTY keys do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    Copy,
    Paste,
    Search,
    Palette,
    ToggleMan,
    TogglePreview,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Help,
}

/// Action metadata: `(action, config key, help label, default chord)`. The config key
/// is what a `[keybindings]` TOML entry overrides; the default chord is in the spec's
/// token form (rendered by [`prettify_chord`]). Order is the help overlay's §3a order,
/// with `Paste` last (shown as a §3b fixed row, not in §3a).
const ACTIONS: &[(Action, &str, &str, &str)] = &[
    (Action::NewTab, "new_tab", "New tab", "Ctrl+Shift+T"),
    (Action::CloseTab, "close_tab", "Close tab", "Ctrl+Shift+W"),
    (Action::NextTab, "next_tab", "Next tab", "Ctrl+Tab"),
    (Action::PrevTab, "prev_tab", "Previous tab", "Ctrl+Shift+Tab"),
    (Action::Copy, "copy", "Copy selection", "Ctrl+Shift+C"),
    (Action::Search, "search", "Find in terminal", "Ctrl+Shift+F"),
    (Action::Palette, "palette", "Command palette", "Ctrl+Shift+P"),
    (Action::ToggleMan, "toggle_man", "Toggle man-page panel", "Ctrl+Shift+M"),
    (Action::TogglePreview, "toggle_preview", "Toggle command preview", "Ctrl+Shift+E"),
    (Action::ZoomIn, "zoom_in", "Zoom in", "Ctrl+Equal"),
    (Action::ZoomOut, "zoom_out", "Zoom out", "Ctrl+Minus"),
    (Action::ZoomReset, "zoom_reset", "Reset zoom", "Ctrl+0"),
    (Action::Help, "help", "This help", "Ctrl+Shift+Slash"),
    (Action::Paste, "paste", "Paste", "Ctrl+Shift+V"),
];

// --- XTWINOPS pixel/display metrics (§17 conformance) -------------------------
// alacritty answers only the char-size winop (CSI 18 t) and drops the pixel/report
// ones. esctest cross-checks that the pixel, char, and cell reports agree, so we
// report a fixed cell size and a nominal display size; the parser thread stays
// window-free. Exact match to the GPU-measured advance isn't tested — only that
// text-area px == chars × cell px, which these constants keep consistent.
const CELL_W_PX: u16 = 9; // ~ FONT_SIZE * 0.6
const CELL_H_PX: u16 = LINE_HEIGHT as u16; // 18
const DISPLAY_W_PX: u16 = 1920;
const DISPLAY_H_PX: u16 = 1080;
const DISPLAY_COLS: u16 = DISPLAY_W_PX / CELL_W_PX; // 213
const DISPLAY_ROWS: u16 = DISPLAY_H_PX / CELL_H_PX; // 60

// Default theme colors (dark). OSC 10/11 can override at runtime.
const DEFAULT_FG: [u8; 3] = [0xcd, 0xd6, 0xf4];
const DEFAULT_BG: [u8; 3] = [0x11, 0x11, 0x1b];
const SELECTION_BG: [u8; 3] = [0x3a, 0x40, 0x5a];

/// Standard ANSI 16-color palette (indices 0..15).
const ANSI16: [[u8; 3]; 16] = [
    [0x00, 0x00, 0x00], [0xcc, 0x00, 0x00], [0x4e, 0x9a, 0x06], [0xc4, 0xa0, 0x00],
    [0x34, 0x65, 0xa4], [0x75, 0x50, 0x7b], [0x06, 0x98, 0x9a], [0xd3, 0xd7, 0xcf],
    [0x55, 0x57, 0x53], [0xef, 0x29, 0x29], [0x8a, 0xe2, 0x34], [0xfc, 0xe9, 0x4f],
    [0x72, 0x9f, 0xcf], [0xad, 0x7f, 0xa8], [0x34, 0xe2, 0xe2], [0xee, 0xee, 0xec],
];

/// Render-time theme colors from config (the palette itself is loaded into the VT
/// color table so `resolve` reads it; these are the render defaults).
#[derive(Clone, Copy)]
struct Theme {
    fg: [u8; 3],
    bg: [u8; 3],
    selection: [u8; 3],
    cursor: [u8; 3],
}

impl Default for Theme {
    fn default() -> Self {
        Theme { fg: DEFAULT_FG, bg: DEFAULT_BG, selection: SELECTION_BG, cursor: DEFAULT_FG }
    }
}

/// Build the render Theme from config colors.
fn theme_from(c: &sampa_config::Colors) -> Theme {
    Theme {
        fg: parse_hex(&c.foreground).unwrap_or(DEFAULT_FG),
        bg: parse_hex(&c.background).unwrap_or(DEFAULT_BG),
        selection: parse_hex(&c.selection).unwrap_or(SELECTION_BG),
        cursor: parse_hex(&c.cursor).unwrap_or(DEFAULT_FG),
    }
}

/// Parse `#rrggbb` into RGB.
fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().strip_prefix('#').filter(|h| h.len() == 6)?;
    let n = u32::from_str_radix(h, 16).ok()?;
    Some([(n >> 16) as u8, (n >> 8) as u8, n as u8])
}

/// Build the OSC sequences that load config colors into the VT color table: OSC 4 for
/// the 16 ANSI slots, OSC 10/11/12 for fg/bg/cursor. Fed to the parser at startup so
/// `resolve` returns the themed colors (it consults the table first).
fn color_setup(c: &sampa_config::Colors) -> Vec<u8> {
    let mut out = Vec::new();
    let x2 = |v: u8| format!("{v:02x}{v:02x}");
    let mut osc = |prefix: String, hex: &str| {
        if let Some([r, g, b]) = parse_hex(hex) {
            out.extend_from_slice(
                format!("\x1b]{prefix}rgb:{}/{}/{}\x1b\\", x2(r), x2(g), x2(b)).as_bytes(),
            );
        }
    };
    let ansi = [
        &c.black, &c.red, &c.green, &c.yellow, &c.blue, &c.magenta, &c.cyan, &c.white,
        &c.bright_black, &c.bright_red, &c.bright_green, &c.bright_yellow, &c.bright_blue,
        &c.bright_magenta, &c.bright_cyan, &c.bright_white,
    ];
    for (i, hex) in ansi.iter().enumerate() {
        osc(format!("4;{i};"), hex);
    }
    osc("10;".into(), &c.foreground);
    osc("11;".into(), &c.background);
    osc("12;".into(), &c.cursor);
    out
}

/// The primary family from a CSS-style fallback list (first entry, quotes stripped).
fn primary_family(list: &str) -> String {
    list.split(',')
        .next()
        .unwrap_or("monospace")
        .trim()
        .trim_matches(['"', '\''])
        .to_string()
}

/// Resolve a family name to a `glyphon::Family`, honoring the CSS generics.
fn family_of(name: &str) -> Family<'_> {
    match name.to_ascii_lowercase().as_str() {
        "monospace" | "ui-monospace" | "" => Family::Monospace,
        "sans-serif" => Family::SansSerif,
        "serif" => Family::Serif,
        _ => Family::Name(name),
    }
}

/// The native build's config file: `$XDG_CONFIG_HOME/sampa2/config.toml` (falling back
/// to `$HOME/.config/sampa2/config.toml`). Deliberately separate from the shared
/// `sampa-config` path (`…/sampa/…`) so this build's config is independent of the origin.
fn config_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))?;
    Some(base.join("sampa2").join("config.toml"))
}

/// The raw config file text, if present.
fn config_text() -> Option<String> {
    std::fs::read_to_string(config_path()?).ok()
}

/// Parse the native-only top-level `opacity = <0..1>` key (background transparency),
/// ignoring comments. `None` if absent/malformed. This key isn't in the shared
/// `sampa-config` schema, so it's [`strip_native_keys`]-ped before that strict parse.
fn parse_opacity(text: &str) -> Option<f32> {
    text.lines().find_map(|l| {
        let l = l.split('#').next().unwrap_or("").trim();
        let rest = l.strip_prefix("opacity")?.trim_start().strip_prefix('=')?;
        rest.trim().parse::<f32>().ok()
    })
}

/// Remove native-only keys (currently `opacity`) so the file passes the strict
/// `sampa-config` parse (both `Config` and its sections `deny_unknown_fields`).
fn strip_native_keys(text: &str) -> String {
    text.lines()
        .filter(|l| !l.split('#').next().unwrap_or("").trim_start().starts_with("opacity"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Background opacity from the config (clamped 0..1), defaulting to 1.0 (opaque).
fn load_opacity() -> f32 {
    config_text().and_then(|t| parse_opacity(&t)).unwrap_or(1.0).clamp(0.0, 1.0)
}

/// Load config from the native XDG path if present, else built-in defaults (§11).
fn load_config() -> sampa_config::Config {
    if let Some(text) = config_text() {
        // Strip the native-only keys the strict sampa-config parse would reject.
        match sampa_config::Config::from_toml(&strip_native_keys(&text)) {
            Ok(c) => return c,
            Err(e) => eprintln!("config: {e}; using defaults"),
        }
    }
    sampa_config::Config::from_toml("").expect("built-in default config parses")
}

/// A single displayable cell after VT-state + attribute resolution.
#[derive(Clone)]
struct CellVis {
    c: char,
    fg: [u8; 3],
    bg: [u8; 3],
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    hyperlink: bool,
}

/// One frame's worth of grid content, extracted under the term lock.
struct Snapshot {
    cols: usize,
    rows: usize,
    offset: i32, // display_offset: image at absolute line L shows at row L + offset
    cells: Vec<CellVis>, // row-major, len == cols * rows
    cursor: Option<(usize, usize)>, // (row, col) for bar/underline cursors (block inverts the cell)
    cursor_rc: Option<(usize, usize)>, // (row, col) of the cursor for ANY style (IME preedit anchor)
    history: usize, // scrollback depth now — pairs with each image's base_history
}

impl Snapshot {
    fn cell(&self, r: usize, c: usize) -> &CellVis {
        &self.cells[r * self.cols + c]
    }
    fn to_text(&self) -> String {
        let mut s = String::with_capacity(self.rows * (self.cols + 1));
        for r in 0..self.rows {
            for c in 0..self.cols {
                s.push(self.cell(r, c).c);
            }
            s.push('\n');
        }
        s
    }
}

/// What the renderer needs to draw the command-palette dropdown: the query, the
/// currently-visible rows, and which of those rows is selected.
struct PaletteView<'a> {
    query: &'a str,
    rows: &'a [PaletteMatch],
    selected: usize,
}

/// A bottom overlay panel (man page or command preview): a header line and the visible
/// body (already sliced to what fits, lines joined by `\n`).
struct PanelView<'a> {
    title: &'a str,
    body: &'a str,
}

/// Side effects the VT engine raises during parsing, forwarded from the parser
/// thread to the main loop (which owns the PTY, window, and clipboard).
enum AppEvent {
    Title(String),
    ClipboardStore(String),
    Bell,
}

/// Alacritty `EventListener` that funnels VT events off the parser thread (§13).
/// **Query replies** (DA/DSR/DECRQSS, OSC 4/10/11 color) go to `reply_tx` and are
/// written to the PTY **synchronously, in stream order**, by the parser thread — apps
/// that block on a reply must see it before anything else. Title/clipboard/bell go to
/// `app_tx` for the UI thread. OSC-52 **reads** and pixel size probes are dropped so
/// nothing echoes attacker-controlled data back to input.
struct EventProxy {
    reply_tx: std::sync::mpsc::Sender<Reply>,
    app_tx: std::sync::mpsc::Sender<AppEvent>,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: TermEvent) {
        match event {
            TermEvent::PtyWrite(s) => {
                let _ = self.reply_tx.send(Reply::Bytes(s.into_bytes()));
            }
            TermEvent::ColorRequest(idx, fmt) => {
                // Resolved against the live color table at drain time (§ color queries).
                let _ = self.reply_tx.send(Reply::Color(idx, fmt));
            }
            TermEvent::Title(s) => {
                let _ = self.app_tx.send(AppEvent::Title(s));
            }
            TermEvent::ResetTitle => {
                let _ = self.app_tx.send(AppEvent::Title(String::new()));
            }
            TermEvent::ClipboardStore(_, s) => {
                let _ = self.app_tx.send(AppEvent::ClipboardStore(s));
            }
            TermEvent::Bell => {
                let _ = self.app_tx.send(AppEvent::Bell);
            }
            // Dropped: ClipboardLoad (deny reads), TextAreaSizeRequest (pixels), and
            // cosmetic/lifecycle events (Wakeup, MouseCursorDirty, …).
            _ => {}
        }
    }
}

/// The default RGB for a color index when the app hasn't set one — never attacker input.
fn palette_rgb(idx: usize) -> Rgb {
    let [r, g, b] = match idx {
        257 => DEFAULT_BG,                 // Background
        i if i < 256 => xterm256(i as u8), // palette entry
        _ => DEFAULT_FG,                   // Foreground(256)/Cursor(258)/other
    };
    Rgb { r, g, b }
}

/// A reply the parser thread must send back to the PTY. Color queries are resolved
/// against the *live* color table at drain time (so an OSC 4/10/11 *set* is reflected),
/// which is why they carry the formatter rather than pre-rendered bytes.
enum Reply {
    Bytes(Vec<u8>),
    Color(usize, Arc<dyn Fn(Rgb) -> String + Send + Sync>),
}

/// Turn a queued `Reply` into bytes, reading the current color for a color query.
fn resolve_reply<L: EventListener>(reply: Reply, term: &Term<L>) -> Vec<u8> {
    match reply {
        Reply::Bytes(b) => b,
        Reply::Color(idx, fmt) => {
            let rgb = term.colors()[idx].unwrap_or_else(|| palette_rgb(idx));
            fmt(rgb).into_bytes()
        }
    }
}

/// Strip control characters and cap length — OSC 0/2 titles are attacker-controlled.
fn sanitize_title(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(256).collect()
}

// --- DECRQCRA — request checksum of rectangular area (conformance, §13/§17) ---
//
// `CSI Pid ; Pp ; Pt ; Pl ; Pb ; Pr * y` asks for a checksum of a screen rectangle so
// tools like esctest can read the grid back. Alacritty ignores it, so we watch the
// output stream for the sequence and reply with the raw 16-bit sum of the rectangle's
// codepoints (empty = 0x20) as `DCS Pid ! ~ HHHH ST` — the convention esctest selects
// with `--xterm-checksum 334`.

#[derive(Debug, PartialEq)]
struct Decrqcra {
    pid: u16,
    top: Option<u16>,
    left: Option<u16>,
    bottom: Option<u16>,
    right: Option<u16>,
}

#[derive(Default, PartialEq)]
enum DecrqcraState {
    #[default]
    Ground,
    Esc,
    Csi,
}

/// Incremental watcher for DECRQCRA requests in the PTY output stream (survives chunk
/// splits). Returns, per completed request, the byte offset just past it so the caller
/// can advance the VT parser up to that point before computing the checksum.
#[derive(Default)]
struct DecrqcraScanner {
    state: DecrqcraState,
    params: Vec<u16>,
    seen: Vec<bool>, // per-param: had ≥1 digit (empty `;;` vs explicit `0`, for XTWINOPS)
    cur: u32,
    cur_seen: bool, // a digit was consumed for the parameter being accumulated
    star: bool,     // saw '*' intermediate (DECRQCRA)
    bang: bool,     // saw '!' intermediate (DECSTR)
    private: bool,  // saw a leading '?' private marker (DEC private modes)
    bad: bool,      // saw a disqualifying intermediate / private marker
}

/// Something the output-stream scanner extracts that alacritty leaves unhandled and
/// we must act on: a rectangular-area checksum request, or a soft reset.
enum ScanEvent {
    Decrqcra(Decrqcra),
    Decstr,
    /// XTWINOPS resize in **cells** (`CSI 8 ; rows ; cols t`, or DECSLPP `CSI Ps t`).
    /// `None` keeps that dimension; an explicit `0` resolves to the display maximum.
    Resize { rows: Option<u16>, cols: Option<u16> },
    /// XTWINOPS resize in **pixels** (`CSI 4 ; h ; w t`); converted to cells against the
    /// fixed cell metrics. Same `None` = keep, `0` = maximize convention.
    ResizePixels { h: Option<u16>, w: Option<u16> },
    /// A window/size **report** query (`CSI 11/13/14/15/16/19 t`). The reply is built
    /// from the live grid plus the fixed metrics at drain time.
    WinopReport(u16),
    /// DEC private mode `?1048` (save/restore cursor) — the engine leaves it unhandled,
    /// so we translate it to the DECSC/DECRC (`ESC 7` / `ESC 8`) it supports. `save` is
    /// the SET (`h`) direction; RESET (`l`) restores.
    SaveRestoreCursor { save: bool },
    /// DEC device-status report query (`CSI ? Ps [; Pid] n`, DECDSR) — vte only handles
    /// the non-private DSR, so these go unanswered. Replied to at drain time (DECXCPR
    /// needs the live cursor; DECCKSR echoes the Pid).
    Decdsr { ps: u16, pid: Option<u16> },
    /// Selective erase (`CSI ? Ps J` DECSED / `CSI ? Ps K` DECSEL) — the engine ignores
    /// the private erase. With no DECSCA protection tracked (all cells unprotected), it
    /// is equivalent to plain ED/EL, which we inject. `line` = DECSEL (`K`).
    SelectiveErase { line: bool, ps: u16 },
    /// A set/reset of a shadowed modifiable mode (SM/RM `CSI Ps h/l`, DECSET/DECRESET
    /// `CSI ? Ps h/l`) whose DECRQM state the engine can't report — recorded so we can.
    SetMode { dec: bool, mode: u16, set: bool },
}

impl DecrqcraScanner {
    fn new() -> Self {
        Self::default()
    }

    fn enter_csi(&mut self) {
        self.state = DecrqcraState::Csi;
        self.params.clear();
        self.seen.clear();
        self.cur = 0;
        self.cur_seen = false;
        self.star = false;
        self.bang = false;
        self.private = false;
        self.bad = false;
    }

    /// Finalize the parameter currently being accumulated into `params`/`seen`.
    fn push_param(&mut self) {
        self.params.push(self.cur.min(0xffff) as u16);
        self.seen.push(self.cur_seen);
        self.cur = 0;
        self.cur_seen = false;
    }

    /// Build the XTWINOPS event for a completed `CSI … t`, or `None` if it's an op we
    /// leave to the engine (18/22/23) or don't act on. `params[0]` selects the op.
    fn winop(&self) -> Option<ScanEvent> {
        let op = *self.params.first()?;
        let present = |i: usize| self.seen.get(i).copied().unwrap_or(false);
        let val = |i: usize| self.params.get(i).copied().unwrap_or(0);
        // Resolve a resize dimension: omitted → keep (None); explicit 0 → maximize to
        // the display; otherwise the given value.
        let dim = |i: usize, max: u16| -> Option<u16> {
            if !present(i) {
                None
            } else if val(i) == 0 {
                Some(max)
            } else {
                Some(val(i))
            }
        };
        match op {
            4 => Some(ScanEvent::ResizePixels {
                h: dim(1, DISPLAY_H_PX),
                w: dim(2, DISPLAY_W_PX),
            }),
            8 => Some(ScanEvent::Resize {
                rows: dim(1, DISPLAY_ROWS),
                cols: dim(2, DISPLAY_COLS),
            }),
            11 | 13 | 14 | 15 | 16 | 19 => Some(ScanEvent::WinopReport(op)),
            // DECSLPP: `CSI Ps t` with Ps ≥ 24 sets the line count, keeping columns.
            op if op >= 24 => Some(ScanEvent::Resize { rows: Some(op), cols: None }),
            _ => None,
        }
    }

    fn request(&self) -> Decrqcra {
        let p = |i: usize| self.params.get(i).copied().filter(|&v| v > 0);
        Decrqcra {
            pid: self.params.first().copied().unwrap_or(0),
            top: p(2),
            left: p(3),
            bottom: p(4),
            right: p(5),
        }
    }

    fn feed(&mut self, bytes: &[u8]) -> Vec<(usize, ScanEvent)> {
        let mut out = Vec::new();
        for (i, &b) in bytes.iter().enumerate() {
            match self.state {
                DecrqcraState::Ground => {
                    if b == 0x1b {
                        self.state = DecrqcraState::Esc;
                    }
                }
                DecrqcraState::Esc => match b {
                    b'[' => self.enter_csi(),
                    0x1b => {}
                    _ => self.state = DecrqcraState::Ground,
                },
                DecrqcraState::Csi => match b {
                    b'0'..=b'9' => {
                        self.cur = self.cur.saturating_mul(10).saturating_add((b - b'0') as u32);
                        self.cur_seen = true;
                    }
                    b';' => self.push_param(),
                    0x2a => self.star = true, // '*'
                    0x21 => self.bang = true, // '!'
                    // A leading '?' is the DEC-private marker; anywhere else it disqualifies.
                    b'?' if self.params.is_empty() && !self.cur_seen && !self.private => {
                        self.private = true
                    }
                    0x40..=0x7e => {
                        self.push_param(); // finalize the trailing parameter
                        if !self.bad {
                            if b == b'y' && self.star && !self.bang {
                                // DECRQCRA: CSI … * y
                                out.push((i + 1, ScanEvent::Decrqcra(self.request())));
                            } else if b == b'p' && self.bang && !self.star {
                                // DECSTR: CSI ! p
                                out.push((i + 1, ScanEvent::Decstr));
                            } else if b == b't' && !self.star && !self.bang && !self.private {
                                // XTWINOPS (`CSI … t`): resize / DECSLPP / size reports.
                                if let Some(ev) = self.winop() {
                                    out.push((i + 1, ev));
                                }
                            } else if (b == b'h' || b == b'l')
                                && self.private
                                && !self.star
                                && !self.bang
                                && self.params.first() == Some(&1048)
                            {
                                // DEC private ?1048 (save/restore cursor) → DECSC/DECRC.
                                out.push((
                                    i + 1,
                                    ScanEvent::SaveRestoreCursor { save: b == b'h' },
                                ));
                            } else if (b == b'h' || b == b'l') && !self.star && !self.bang {
                                // SM/RM (`CSI Ps h/l`) or DECSET/DECRESET (`CSI ? Ps h/l`):
                                // shadow the set/reset of modes we track for DECRQM.
                                for &m in &self.params {
                                    if decrqm_modifiable_mode(self.private, m) {
                                        out.push((
                                            i + 1,
                                            ScanEvent::SetMode { dec: self.private, mode: m, set: b == b'h' },
                                        ));
                                    }
                                }
                            } else if b == b'n' && self.private && !self.star && !self.bang {
                                // DECDSR: CSI ? Ps [; Pid] n — device-status report.
                                out.push((
                                    i + 1,
                                    ScanEvent::Decdsr {
                                        ps: self.params.first().copied().unwrap_or(0),
                                        pid: self.params.get(1).copied(),
                                    },
                                ));
                            } else if (b == b'J' || b == b'K') && self.private && !self.star && !self.bang {
                                // DECSED (`? J`) / DECSEL (`? K`): no protection tracked,
                                // so translate to plain ED/EL.
                                out.push((
                                    i + 1,
                                    ScanEvent::SelectiveErase {
                                        line: b == b'K',
                                        ps: self.params.first().copied().unwrap_or(0),
                                    },
                                ));
                            }
                        }
                        self.state = DecrqcraState::Ground;
                    }
                    0x20..=0x2f | 0x3a..=0x3f => self.bad = true, // other intermediate / private
                    _ => self.state = DecrqcraState::Ground,       // C0 etc. → abort
                },
            }
        }
        out
    }
}

/// Soft-reset (DECSTR) is unhandled by the VT engine (vte only dispatches `$p`/`?$p`
/// for `CSI p`), so we synthesize the state reset it must perform: origin mode off,
/// full scroll region, replace/insert off, normal cursor keys, cursor shown, default
/// SGR, and cursor home. Injected into the parser at the point the `CSI ! p` appears.
const DECSTR_RESET: &[u8] = b"\x1b[?6l\x1b[r\x1b[4l\x1b[?1l\x1b[?25h\x1b[m\x1b[H";

// --- DECRQSS — request selection/setting status string (§17) -----------------
// alacritty doesn't answer `DCS $ q <Pt> ST`, so we watch the stream for it and
// reply `DCS 1 $ r <value> <Pt> ST` (or `DCS 0 $ r ST` for unsupported queries).

#[derive(Default, PartialEq)]
enum DcsState {
    #[default]
    Ground,
    Esc,
    Dcs,
    EscInDcs,
}

/// Accumulates `DCS … ST/BEL` bodies and yields the ones that begin `$q` (DECRQSS),
/// returning the query name `Pt` (the bytes after `$q`). Bodies are tiny; capped.
#[derive(Default)]
struct DcsScanner {
    state: DcsState,
    buf: Vec<u8>,
    over: bool,
}

impl DcsScanner {
    fn new() -> Self {
        Self::default()
    }

    fn finish(&mut self) -> Option<Vec<u8>> {
        let out = (!self.over && self.buf.starts_with(b"$q")).then(|| self.buf[2..].to_vec());
        self.buf.clear();
        self.over = false;
        self.state = DcsState::Ground;
        out
    }

    fn feed(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for &b in bytes {
            match self.state {
                DcsState::Ground => {
                    if b == 0x1b {
                        self.state = DcsState::Esc;
                    }
                }
                DcsState::Esc => match b {
                    b'P' => {
                        self.state = DcsState::Dcs;
                        self.buf.clear();
                        self.over = false;
                    }
                    0x1b => {}
                    _ => self.state = DcsState::Ground,
                },
                DcsState::Dcs => match b {
                    0x07 => out.extend(self.finish()),
                    0x1b => self.state = DcsState::EscInDcs,
                    _ => {
                        if self.buf.len() < 64 {
                            self.buf.push(b);
                        } else {
                            self.over = true;
                        }
                    }
                },
                DcsState::EscInDcs => match b {
                    b'\\' => out.extend(self.finish()),
                    _ => {
                        self.buf.clear();
                        self.state = if b == 0x1b { DcsState::Esc } else { DcsState::Ground };
                    }
                },
            }
        }
        out
    }
}

/// Reconstruct the current SGR parameters from the pen (cursor template) for DECRQSS.
fn sgr_from_template<L: EventListener>(term: &Term<L>) -> String {
    let f = term.grid().cursor.template.flags;
    let mut s = String::from("0");
    for (flag, code) in [
        (Flags::BOLD, "1"),
        (Flags::DIM, "2"),
        (Flags::ITALIC, "3"),
        (Flags::UNDERLINE, "4"),
        (Flags::INVERSE, "7"),
        (Flags::HIDDEN, "8"),
        (Flags::STRIKEOUT, "9"),
    ] {
        if f.contains(flag) {
            s.push(';');
            s.push_str(code);
        }
    }
    s
}

/// Build the DECRQSS reply for query `pt`. Handles the fixed/tractable settings;
/// scroll-region/margins/cursor-style need private engine state and report invalid.
fn decrqss_reply<L: EventListener>(pt: &[u8], term: &Term<L>) -> Vec<u8> {
    let body = match pt {
        b"\"p" => "1$r64;1\"p".to_string(),                 // DECSCL: VT level 4, 7-bit
        b"\"q" => "1$r1\"q".to_string(),                    // DECSCA (default)
        b"m" => format!("1$r{}m", sgr_from_template(term)), // SGR
        b"+q" | b"*}" | b"$}" | b"*x" => {
            format!("1$r0{}", String::from_utf8_lossy(pt)) // report 0 for these
        }
        _ => "0$r".to_string(), // unsupported → invalid
    };
    format!("\x1bP{body}\x1b\\").into_bytes()
}

// --- Inline images (iTerm2 OSC 1337, §6.4) -----------------------------------
// alacritty has no image support, so we watch the output stream for
// `OSC 1337 ; File = <args> : <base64> ST`, decode it (with §13 OOM caps), and
// composite it into the GPU scene at the cursor.

const MAX_IMAGE_DIM: u32 = 4096; // reject absurd dimensions
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024; // decoded-source cap
const MAX_OSC_BYTES: usize = 12 * 1024 * 1024; // in-flight OSC accumulation cap
const MAX_IMAGES: usize = 32; // live image cap (oldest evicted)

struct DecodedImage {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

/// Decode an iTerm2 `1337;File=<args>:<base64>` OSC body into RGBA, enforcing caps.
fn parse_iterm_image(payload: &[u8]) -> Option<DecodedImage> {
    let rest = payload.strip_prefix(b"1337;File=")?;
    let colon = rest.iter().position(|&b| b == b':')?;
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&rest[colon + 1..])
        .ok()?;
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
        return None;
    }
    let rgba = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let (width, height) = rgba.dimensions();
    if width == 0 || height == 0 || width > MAX_IMAGE_DIM || height > MAX_IMAGE_DIM {
        return None;
    }
    Some(DecodedImage { width, height, rgba: rgba.into_raw() })
}

#[derive(Default, PartialEq)]
enum OscState {
    #[default]
    Ground,
    Esc,
    Osc,
    EscInOsc,
}

/// Watches the output stream for `OSC 1337 ; … ST/BEL` and returns the payloads of
/// completed image OSCs (only buffering ones that begin `1337;`, capped at
/// `MAX_OSC_BYTES`). Runs in parallel with the VT parser (which ignores OSC 1337).
#[derive(Default)]
struct ImageScanner {
    state: OscState,
    buf: Vec<u8>,
    is_image: bool,
    checked: bool,
    overflow: bool,
}

impl ImageScanner {
    fn new() -> Self {
        Self::default()
    }

    fn begin(&mut self) {
        self.state = OscState::Osc;
        self.buf.clear();
        self.is_image = true;
        self.checked = false;
        self.overflow = false;
    }

    fn push(&mut self, b: u8) {
        if self.overflow || !self.is_image {
            return;
        }
        self.buf.push(b);
        if self.buf.len() > MAX_OSC_BYTES {
            self.overflow = true;
            self.buf = Vec::new();
            self.is_image = false;
        } else if !self.checked && self.buf.len() >= 5 {
            self.checked = true;
            if !self.buf.starts_with(b"1337;") {
                self.is_image = false;
                self.buf = Vec::new();
            }
        }
    }

    fn finish(&mut self) -> Option<Vec<u8>> {
        let out = (self.is_image && !self.overflow && self.buf.starts_with(b"1337;"))
            .then(|| std::mem::take(&mut self.buf));
        self.buf = Vec::new();
        self.state = OscState::Ground;
        out
    }

    fn feed(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for &b in bytes {
            match self.state {
                OscState::Ground => {
                    if b == 0x1b {
                        self.state = OscState::Esc;
                    }
                }
                OscState::Esc => match b {
                    b']' => self.begin(),
                    0x1b => {}
                    _ => self.state = OscState::Ground,
                },
                OscState::Osc => match b {
                    0x07 => out.extend(self.finish()), // BEL terminator
                    0x1b => self.state = OscState::EscInOsc,
                    _ => self.push(b),
                },
                OscState::EscInOsc => match b {
                    b'\\' => out.extend(self.finish()), // ST terminator
                    _ => {
                        self.buf = Vec::new();
                        self.state = if b == 0x1b { OscState::Esc } else { OscState::Ground };
                    }
                },
            }
        }
        out
    }
}

// --- Sixel graphics (DCS <params> q … ST, §6.4) ------------------------------
// alacritty ignores DCS sixel, so a parallel scanner captures the payload and
// `parse_sixel` rasterizes it into the same `DecodedImage`/`ImageStore` path the
// iTerm2 images use. The parser is pure and unit-tested.

/// Payload cap for one sixel image (raw command bytes), and a pixel cap on the result.
const MAX_SIXEL_BYTES: usize = 4 * 1024 * 1024;
const MAX_SIXEL_PIXELS: usize = 4_000_000;

/// Read a run of ASCII digits as a number, saturating; returns the value and next index.
fn sixel_num(data: &[u8], mut i: usize) -> (u32, usize) {
    let mut n = 0u32;
    while i < data.len() && data[i].is_ascii_digit() {
        n = n.saturating_mul(10).saturating_add((data[i] - b'0') as u32);
        i += 1;
    }
    (n, i)
}

/// The default sixel color registers: the 16 VT340-ish colors, rest black. Most sixels
/// define their own colors with `#n;2;r;g;b`, so this only backstops undefined registers.
fn default_sixel_palette() -> [[u8; 3]; 256] {
    let mut p = [[0u8; 3]; 256];
    let base = [
        [0, 0, 0], [205, 0, 0], [0, 205, 0], [205, 205, 0], [0, 0, 238], [205, 0, 205],
        [0, 205, 205], [229, 229, 229], [127, 127, 127], [255, 0, 0], [0, 255, 0],
        [255, 255, 0], [92, 92, 255], [255, 0, 255], [0, 255, 255], [255, 255, 255],
    ];
    for (i, c) in base.iter().enumerate() {
        p[i] = *c;
    }
    p
}

/// Walk sixel command `data` (the bytes after `q`), calling `emit(x, y, rgb)` for every
/// lit pixel. Handles color select/define (`#`), RLE (`!`), CR (`$`), LF (`-`), and the
/// sixel data bytes `?`..`~` (each = 6 vertical pixels). Colors resolve at emit time.
fn sixel_walk(data: &[u8], mut emit: impl FnMut(u32, u32, [u8; 3])) {
    let mut palette = default_sixel_palette();
    let mut color = palette[0];
    let (mut x, mut band) = (0u32, 0u32);
    let mut i = 0;
    let scale = |v: u32| ((v.min(100) * 255 + 50) / 100) as u8;
    while i < data.len() {
        match data[i] {
            b'#' => {
                let (n, j) = sixel_num(data, i + 1);
                i = j;
                let reg = (n & 0xff) as usize;
                if i < data.len() && data[i] == b';' {
                    // Definition: ;Pu;Px;Py;Pz — Pu=2 is RGB (0..100 each).
                    let (pu, j) = sixel_num(data, i + 1);
                    i = j;
                    let mut vals = [0u32; 3];
                    let mut k = 0;
                    while k < 3 && i < data.len() && data[i] == b';' {
                        let (v, j) = sixel_num(data, i + 1);
                        i = j;
                        vals[k] = v;
                        k += 1;
                    }
                    if pu == 2 {
                        palette[reg] = [scale(vals[0]), scale(vals[1]), scale(vals[2])];
                    }
                }
                color = palette[reg];
            }
            b'!' => {
                let (rep, j) = sixel_num(data, i + 1);
                i = j;
                let rep = rep.max(1);
                if i < data.len() && (0x3f..=0x7e).contains(&data[i]) {
                    let bits = data[i] - 0x3f;
                    for k in 0..rep {
                        for bit in 0..6u32 {
                            if bits & (1 << bit) != 0 {
                                emit(x + k, band * 6 + bit, color);
                            }
                        }
                    }
                    x += rep;
                    i += 1;
                }
            }
            b'$' => {
                x = 0;
                i += 1;
            }
            b'-' => {
                x = 0;
                band += 1;
                i += 1;
            }
            c @ 0x3f..=0x7e => {
                let bits = c - 0x3f;
                for bit in 0..6u32 {
                    if bits & (1 << bit) != 0 {
                        emit(x, band * 6 + bit, color);
                    }
                }
                x += 1;
                i += 1;
            }
            _ => i += 1, // whitespace / newlines / unknown
        }
    }
}

/// Rasterize a sixel DCS payload (`<params> q <data>`) into RGBA. `None` if it isn't a
/// sixel (the pre-`q` params must be numeric) or has no pixels / exceeds the caps.
fn parse_sixel(payload: &[u8]) -> Option<DecodedImage> {
    let qpos = payload.iter().position(|&b| b == b'q')?;
    if !payload[..qpos].iter().all(|&b| b.is_ascii_digit() || b == b';' || b == b' ') {
        return None; // e.g. DECRQSS `$q…` — not a sixel
    }
    let data = &payload[qpos + 1..];
    // Pass 1: extent.
    let (mut mx, mut my, mut any) = (0u32, 0u32, false);
    sixel_walk(data, |x, y, _| {
        any = true;
        mx = mx.max(x);
        my = my.max(y);
    });
    if !any {
        return None;
    }
    let (w, h) = (mx + 1, my + 1);
    if w > MAX_IMAGE_DIM || h > MAX_IMAGE_DIM || (w as usize) * (h as usize) > MAX_SIXEL_PIXELS {
        return None;
    }
    // Pass 2: rasterize (opaque where lit, transparent elsewhere).
    let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
    sixel_walk(data, |x, y, c| {
        let idx = ((y * w + x) as usize) * 4;
        rgba[idx..idx + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
    });
    Some(DecodedImage { width: w, height: h, rgba })
}

/// Watches the stream for a DCS sixel (`ESC P <params> q … ST/BEL`) and returns the
/// completed payload. Runs beside the VT parser (which ignores DCS) and the DECRQSS
/// `DcsScanner`; `parse_sixel` rejects non-sixel DCS, so the two don't conflict.
#[derive(Default)]
struct SixelScanner {
    state: DcsState,
    buf: Vec<u8>,
    over: bool,
}

impl SixelScanner {
    fn new() -> Self {
        Self::default()
    }

    fn finish(&mut self) -> Option<Vec<u8>> {
        let out = (!self.over && !self.buf.is_empty()).then(|| std::mem::take(&mut self.buf));
        self.buf.clear();
        self.over = false;
        self.state = DcsState::Ground;
        out
    }

    fn feed(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for &b in bytes {
            match self.state {
                DcsState::Ground => {
                    if b == 0x1b {
                        self.state = DcsState::Esc;
                    }
                }
                DcsState::Esc => match b {
                    b'P' => {
                        self.state = DcsState::Dcs;
                        self.buf.clear();
                        self.over = false;
                    }
                    0x1b => {}
                    _ => self.state = DcsState::Ground,
                },
                DcsState::Dcs => match b {
                    0x07 => out.extend(self.finish()),
                    0x1b => self.state = DcsState::EscInDcs,
                    _ => {
                        if self.buf.len() < MAX_SIXEL_BYTES {
                            self.buf.push(b);
                        } else {
                            self.over = true;
                        }
                    }
                },
                DcsState::EscInDcs => match b {
                    b'\\' => out.extend(self.finish()),
                    _ => {
                        self.buf.clear();
                        self.state = if b == 0x1b { DcsState::Esc } else { DcsState::Ground };
                    }
                },
            }
        }
        out
    }
}

// --- Kitty graphics protocol (APC G, §6.4) -----------------------------------
// `ESC _ G <k=v,…> ; <base64 payload> ST`. alacritty ignores APC, so a parallel
// scanner accumulates chunked transmissions (m=1) and `parse_kitty` decodes the
// result into the shared image path. v1 handles immediate transmit+display (a=T).

const MAX_KITTY_BYTES: usize = 12 * 1024 * 1024; // in-flight APC accumulation cap

/// Value of control key `key` in a kitty `k=v,k=v` control string, if present.
fn kitty_key<'a>(control: &'a str, key: &str) -> Option<&'a str> {
    control.split(',').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k.trim() == key).then_some(v.trim())
    })
}

fn kitty_num(control: &str, key: &str) -> Option<u32> {
    kitty_key(control, key)?.parse().ok()
}

/// Decode a completed kitty transmission into RGBA — but only when it asks to display
/// (`a=T`). Formats: `f=100` PNG (default here), `f=32` raw RGBA, `f=24` raw RGB; raw
/// needs the pixel dimensions `s`×`v`. Enforces the shared dimension/byte caps.
fn parse_kitty(control: &str, payload: &[u8]) -> Option<DecodedImage> {
    if kitty_key(control, "a") != Some("T") {
        return None; // transmit-only / placement / delete — not handled in v1
    }
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(payload).ok()?;
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
        return None;
    }
    match kitty_key(control, "f").unwrap_or("100") {
        "100" => {
            let rgba = image::load_from_memory(&bytes).ok()?.to_rgba8();
            let (width, height) = rgba.dimensions();
            (width > 0 && height > 0 && width <= MAX_IMAGE_DIM && height <= MAX_IMAGE_DIM)
                .then(|| DecodedImage { width, height, rgba: rgba.into_raw() })
        }
        "32" | "24" => {
            let (w, h) = (kitty_num(control, "s")?, kitty_num(control, "v")?);
            if w == 0 || h == 0 || w > MAX_IMAGE_DIM || h > MAX_IMAGE_DIM {
                return None;
            }
            let px = (w as usize) * (h as usize);
            let rgba = if kitty_key(control, "f") == Some("24") {
                if bytes.len() < px * 3 {
                    return None;
                }
                bytes.chunks_exact(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect()
            } else {
                if bytes.len() < px * 4 {
                    return None;
                }
                bytes[..px * 4].to_vec()
            };
            Some(DecodedImage { width: w, height: h, rgba })
        }
        _ => None,
    }
}

/// A kitty APC response (`ESC _ G i=<id>;OK ST`) so clients like `icat` don't block.
/// Sent only when the transmission carried an id and didn't ask to be quiet (`q=1|2`).
fn kitty_response(control: &str) -> Option<Vec<u8>> {
    let id = kitty_key(control, "i")?;
    if matches!(kitty_key(control, "q"), Some("1") | Some("2")) {
        return None;
    }
    Some(format!("\x1b_Gi={id};OK\x1b\\").into_bytes())
}

/// Watches the stream for kitty graphics APCs (`ESC _ G … ST/BEL`), accumulating chunked
/// transmissions (`m=1` … `m=0`) and yielding `(control, payload)` for completed images.
#[derive(Default)]
struct KittyScanner {
    state: DcsState, // reuses the same Ground/Esc/Body/EscInBody shape
    buf: Vec<u8>,
    over: bool,
    pending_control: Option<String>,
    pending_payload: Vec<u8>,
}

impl KittyScanner {
    fn new() -> Self {
        Self::default()
    }

    /// Process one completed APC body (the bytes between `ESC _` and `ST`).
    fn complete(&mut self, out: &mut Vec<(String, Vec<u8>)>) {
        let buf = std::mem::take(&mut self.buf);
        self.state = DcsState::Ground;
        if self.over {
            self.over = false;
            self.pending_control = None;
            self.pending_payload.clear();
            return;
        }
        let Some(body) = buf.strip_prefix(b"G") else {
            return; // not a graphics APC
        };
        let (control, payload) = match body.iter().position(|&b| b == b';') {
            Some(i) => (String::from_utf8_lossy(&body[..i]).into_owned(), body[i + 1..].to_vec()),
            None => (String::from_utf8_lossy(body).into_owned(), Vec::new()),
        };
        let more = kitty_key(&control, "m") == Some("1");
        if self.pending_control.is_some() {
            self.pending_payload.extend_from_slice(&payload);
            if !more {
                let ctrl = self.pending_control.take().unwrap();
                out.push((ctrl, std::mem::take(&mut self.pending_payload)));
            }
        } else if more {
            self.pending_control = Some(control);
            self.pending_payload = payload;
        } else {
            out.push((control, payload));
        }
    }

    fn feed(&mut self, bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        for &b in bytes {
            match self.state {
                DcsState::Ground => {
                    if b == 0x1b {
                        self.state = DcsState::Esc;
                    }
                }
                DcsState::Esc => match b {
                    b'_' => {
                        self.state = DcsState::Dcs;
                        self.buf.clear();
                        self.over = false;
                    }
                    0x1b => {}
                    _ => self.state = DcsState::Ground,
                },
                DcsState::Dcs => match b {
                    0x07 => self.complete(&mut out),
                    0x1b => self.state = DcsState::EscInDcs,
                    _ => {
                        if self.buf.len() < MAX_KITTY_BYTES {
                            self.buf.push(b);
                        } else {
                            self.over = true;
                        }
                    }
                },
                DcsState::EscInDcs => match b {
                    b'\\' => self.complete(&mut out),
                    _ => {
                        self.buf.clear();
                        self.state = if b == 0x1b { DcsState::Esc } else { DcsState::Ground };
                    }
                },
            }
        }
        out
    }
}

/// One decoded image placed on the grid. `anchor` is the screen line at insert time and
/// `base_history` the scrollback depth then; together they pin the image to its content
/// so it scrolls up with the text (see [`image_row`]). `id` keys the GPU texture.
struct PlacedImage {
    id: u64,
    anchor: i32,
    base_history: usize,
    col: usize,
    width: u32,
    height: u32,
    rgba: Option<Vec<u8>>, // taken by the renderer on first upload
}

/// The visible grid row for an image: its content's stable position (`anchor` plus the
/// scrollback that existed at insert) mapped into the current view. As new output scrolls
/// in, `history` grows, so the image rides up with its text and eventually off the top.
fn image_row(anchor: i32, base_history: usize, history: usize, offset: i32) -> i32 {
    anchor - (history as i32 - base_history as i32) + offset
}

/// Live inline images, shared between the parser thread (adds) and the renderer
/// (uploads + composites). Capped at `MAX_IMAGES`, oldest evicted.
#[derive(Default)]
struct ImageStore {
    images: Vec<PlacedImage>,
    next_id: u64,
}

impl ImageStore {
    fn add(&mut self, anchor: i32, base_history: usize, col: usize, img: DecodedImage) {
        let id = self.next_id;
        self.next_id += 1;
        self.images.push(PlacedImage {
            id,
            anchor,
            base_history,
            col,
            width: img.width,
            height: img.height,
            rgba: Some(img.rgba),
        });
        if self.images.len() > MAX_IMAGES {
            self.images.remove(0);
        }
    }
}

/// Compute the DECRQCRA reply for `req` against the current grid.
fn compute_decrqcra<L: EventListener>(term: &Term<L>, req: &Decrqcra) -> Vec<u8> {
    let grid = term.grid();
    let (rows, cols) = (grid.screen_lines(), grid.columns());
    let top = req.top.map(|v| v as usize).unwrap_or(1).max(1);
    let left = req.left.map(|v| v as usize).unwrap_or(1).max(1);
    let bottom = req.bottom.map(|v| v as usize).unwrap_or(rows).min(rows);
    let right = req.right.map(|v| v as usize).unwrap_or(cols).min(cols);
    let mut sum: u32 = 0;
    for row in top..=bottom {
        for col in left..=right {
            let code = grid[Line((row - 1) as i32)][Column(col - 1)].c as u32;
            sum = sum.wrapping_add(if code == 0 { 0x20 } else { code });
        }
    }
    format!("\x1bP{}!~{:04X}\x1b\\", req.pid, sum & 0xffff).into_bytes()
}

// --- DECRQM permanently-reset modes (§17 conformance) ------------------------
// Modes esctest expects a DECRQM "permanently reset" (4) reply for: known to xterm but
// deliberately unavailable here. alacritty answers them as "not recognized" (0) because
// its mode enum doesn't list them; 4 is the correct reply for a terminal that knows the
// mode yet never sets it (what xterm does), so we rewrite 0 → 4 for exactly these. Keyed
// separately for ANSI (`CSI Ps $ y`) and DEC-private (`CSI ? Ps $ y`) — the numbers
// overlap across the two namespaces (e.g. ANSI 1 = GATM vs. DEC 1 = DECCKM).
const DECRQM_PERM_RESET_ANSI: &[u16] = &[
    1,  // GATM
    5,  // SRTM
    7,  // VEM
    10, // HEM
    11, // PUM
    13, // FEAM
    14, // FETM
    15, // MATM
    16, // TTM
    17, // SATM
    18, // TSM
    19, // EBM
];
const DECRQM_PERM_RESET_DEC: &[u16] = &[60]; // DECHCCM

// Modifiable modes esctest toggles + queries but `alacritty_terminal` doesn't track for
// DECRQM: it reports "not recognized" (0) regardless of SM/RM/DECSET/DECRESET. We shadow
// their set/reset state (from the toggle sequences) and rewrite the DECRQM reply to it.
const DECRQM_MOD_ANSI: &[u16] = &[2, 12]; // KAM, SRM
const DECRQM_MOD_DEC: &[u16] = &[
    3,  // DECCOLM
    4,  // DECSCLM
    5,  // DECSCNM
    18, // DECPFF
    19, // DECPEX
    42, // DECNRCM
    66, // DECNKM
    67, // DECBKM
    69, // DECLRMM
];

/// True if `mode` in the given namespace is one we shadow for DECRQM.
fn decrqm_modifiable_mode(dec: bool, mode: u16) -> bool {
    if dec { DECRQM_MOD_DEC } else { DECRQM_MOD_ANSI }.contains(&mode)
}

/// Rewrite an alacritty DECRQM reply (`CSI [?] Ps ; St $ y`) for a shadowed modifiable
/// mode to our tracked set/reset state (1 = set, 2 = reset; default reset when untoggled),
/// since the engine can't report these. `None` for replies we don't override.
fn decrqm_modifiable(
    bytes: &[u8],
    shadow: &std::collections::HashMap<(bool, u16), bool>,
) -> Option<Vec<u8>> {
    let inner = bytes.strip_prefix(b"\x1b[")?.strip_suffix(b"$y")?;
    let (dec, nums) = match inner.strip_prefix(b"?") {
        Some(rest) => (true, rest),
        None => (false, inner),
    };
    let (mode_s, _state) = std::str::from_utf8(nums).ok()?.split_once(';')?;
    let mode: u16 = mode_s.parse().ok()?;
    if !decrqm_modifiable_mode(dec, mode) {
        return None;
    }
    let state = if shadow.get(&(dec, mode)).copied().unwrap_or(false) { 1 } else { 2 };
    Some(format!("\x1b[{}{};{}$y", if dec { "?" } else { "" }, mode, state).into_bytes())
}

/// If `bytes` is an alacritty DECRQM reply (`CSI [?] Ps ; Ps2 $ y`) reporting a
/// permanently-reset mode as "not recognized" (state 0), return the corrected reply
/// with state 4. Returns `None` for non-DECRQM replies and modes we don't override, so
/// they pass through untouched.
fn decrqm_perm_reset(bytes: &[u8]) -> Option<Vec<u8>> {
    let inner = bytes.strip_prefix(b"\x1b[")?.strip_suffix(b"$y")?;
    let (dec, nums) = match inner.strip_prefix(b"?") {
        Some(rest) => (true, rest),
        None => (false, inner),
    };
    let (mode, state) = std::str::from_utf8(nums).ok()?.split_once(';')?;
    if state != "0" {
        return None;
    }
    let mode: u16 = mode.parse().ok()?;
    let list = if dec { DECRQM_PERM_RESET_DEC } else { DECRQM_PERM_RESET_ANSI };
    list.contains(&mode)
        .then(|| format!("\x1b[{}{};4$y", if dec { "?" } else { "" }, mode).into_bytes())
}

/// Build the reply to a DECDSR device-status query (`CSI ? Ps [; Pid] n`), or `None`
/// for a report we don't answer. vte only dispatches the non-private DSR, so these go
/// unanswered by the engine; the fixed values are the legal "feature absent" reports
/// (no printer, keyboard = North American, no locator, 0 macro space, …) and are what
/// esctest accepts. DECXCPR (6) reports the live cursor; the terminal presents as VT
/// level 2 (DA2 type 0), so it omits the page parameter. DECCKSR (63) echoes the Pid.
fn decdsr_reply(ps: u16, pid: Option<u16>, cur_row: u16, cur_col: u16) -> Option<Vec<u8>> {
    let s = match ps {
        6 => format!("\x1b[?{cur_row};{cur_col}R"), // DECXCPR (no page at VT level 2)
        15 => "\x1b[?13n".to_string(),              // printer port: no printer
        25 => "\x1b[?20n".to_string(),              // UDK: unlocked
        26 => "\x1b[?27;1n".to_string(),            // keyboard: North American (2 params)
        55 => "\x1b[?50n".to_string(),              // locator status: no locator
        56 => "\x1b[?57;0n".to_string(),            // locator type: unknown
        62 => "\x1b[0*{".to_string(),               // macro space: 0 (note: no ? prefix)
        63 => format!("\x1bP{}!~0000\x1b\\", pid.unwrap_or(0)), // macro checksum: 0
        75 => "\x1b[?70n".to_string(),              // data integrity: ready, no errors
        85 => "\x1b[?83n".to_string(),              // sessions: not multi-session
        _ => return None,
    };
    Some(s.into_bytes())
}

/// Build the reply to an XTWINOPS size/state **report** query (`CSI 11/13/14/15/16/19
/// t`) from the live grid (`cols`×`rows`) and the fixed pixel metrics. The engine drops
/// these, so we answer them here (§17). Formats match xterm / esctest's `escutil`:
///   11 → `CSI 1 t` (not iconified)   13 → `CSI 3 ; x ; y t` (window position)
///   14 → `CSI 4 ; h ; w t` (text-area px)   15 → `CSI 5 ; h ; w t` (screen px)
///   16 → `CSI 6 ; h ; w t` (cell px)   19 → `CSI 9 ; rows ; cols t` (screen chars)
fn winop_report(op: u16, cols: u16, rows: u16) -> Vec<u8> {
    let (cw, ch) = (CELL_W_PX as u32, CELL_H_PX as u32);
    let s = match op {
        11 => "\x1b[1t".to_string(),
        13 => "\x1b[3;0;0t".to_string(),
        14 => format!("\x1b[4;{};{}t", rows as u32 * ch, cols as u32 * cw),
        15 => format!("\x1b[5;{};{}t", DISPLAY_H_PX, DISPLAY_W_PX),
        16 => format!("\x1b[6;{};{}t", CELL_H_PX, CELL_W_PX),
        19 => format!("\x1b[9;{};{}t", DISPLAY_ROWS, DISPLAY_COLS),
        _ => return Vec::new(),
    };
    s.into_bytes()
}

struct TermState {
    parser: Processor,
    term: Term<EventProxy>,
    decrqcra: DecrqcraScanner,
    image_scanner: ImageScanner,
    sixel: SixelScanner,
    kitty: KittyScanner,
    dcs: DcsScanner,
    /// Shadow set/reset state for modifiable modes the engine can't report via DECRQM.
    decrqm_shadow: std::collections::HashMap<(bool, u16), bool>,
}

#[derive(Debug)]
enum UserEvent {
    Redraw,
    SessionExit { id: u64, detail: String },
    ConfigReload,
    CursorBlink,
    /// A background `man` render finished (`None` = no page / invalid command).
    ManReady { cmd: String, lines: Option<Vec<String>> },
    /// A debounced command preview finished; `gen` guards against stale results.
    PreviewReady { gen: u64, line: String, ran: bool, text: String },
    /// An AccessKit adapter event (tree request / action / deactivation).
    AccessKit(accesskit_winit::Event),
}

impl From<accesskit_winit::Event> for UserEvent {
    fn from(e: accesskit_winit::Event) -> Self {
        UserEvent::AccessKit(e)
    }
}

/// One terminal tab: its VT state, PTY, image layer, and per-tab UI-event channel.
/// `App` keeps Arc-clones of the active session's `state`/`pty`/`images` so existing
/// call sites stay unchanged; those are re-pointed on switch.
struct Session {
    id: u64,
    state: Arc<Mutex<TermState>>,
    pty: Arc<Mutex<PtyHandle>>,
    images: Arc<Mutex<ImageStore>>,
    app_rx: Receiver<AppEvent>,
    title: String,
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--smoke") {
        return smoke::run();
    }
    if let Some(i) = args.iter().position(|a| a == "--capture") {
        let path = args.get(i + 1).map(String::as_str).unwrap_or("sampa.png");
        return capture(path);
    }

    // CLI subset (§12.2), enough for launchers and the esctest runner:
    //   -e/-- CMD…   run CMD instead of $SHELL
    //   --working-directory DIR / -w DIR
    //   --title STR / -T STR
    let mut run_cmd: Option<Vec<String>> = None;
    let mut cwd: Option<String> = None;
    let mut win_title = "Sampa (native)".to_string();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-e" | "--" => {
                run_cmd = Some(args[i + 1..].to_vec());
                break;
            }
            "--working-directory" | "-w" => {
                cwd = args.get(i + 1).cloned();
                i += 1;
            }
            "--title" | "-T" => {
                if let Some(t) = args.get(i + 1) {
                    win_title = t.clone();
                }
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    let (shell, shell_args) = match run_cmd {
        Some(mut c) if !c.is_empty() => (c.remove(0), c),
        _ => (std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()), vec![]),
    };

    // Load config (default path if present, else built-in defaults) — drives fonts,
    // colors, and scrollback (§11).
    let cfg = load_config();
    let theme = theme_from(&cfg.colors);
    let font_size = cfg.font.size.clamp(6.0, 72.0);
    let font_family = primary_family(&cfg.font.family);
    let cursor_style = cfg.cursor.style;
    let blink = cfg.cursor.blink;

    let (cols, rows) = (80u16, 24u16);
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();

    // The first tab, from the CLI shell/args/cwd. Its pump thread starts inside.
    let session = spawn_session(0, &proxy, cols, rows, &cfg, shell, shell_args, cwd)?;
    let state = Arc::clone(&session.state);
    let pty = Arc::clone(&session.pty);
    let images = Arc::clone(&session.images);
    let sessions = vec![session];

    // Watch the config file (mtime poll) and wake the loop on change → live reload.
    if let Some(path) = config_path() {
        let watch_proxy = proxy.clone();
        thread::spawn(move || {
            let mtime = || std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            let mut last = mtime();
            loop {
                std::thread::sleep(std::time::Duration::from_millis(1000));
                let cur = mtime();
                if cur != last {
                    last = cur;
                    if watch_proxy.send_event(UserEvent::ConfigReload).is_err() {
                        break; // event loop gone
                    }
                }
            }
        });
    }

    // Cursor blink tick (~530ms). Always runs; App ignores it when blink is off.
    {
        let blink_proxy = proxy.clone();
        thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(530));
            if blink_proxy.send_event(UserEvent::CursorBlink).is_err() {
                break;
            }
        });
    }

    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App {
        sessions,
        active: 0,
        next_id: 1,
        proxy,
        state,
        pty,
        cols,
        rows,
        window: None,
        gfx: None,
        a11y: None,
        modifiers: ModifiersState::empty(),
        dumped: false,
        mouse_col: 0,
        mouse_row: 0,
        mouse_px: 0.0,
        mouse_py: 0.0,
        left_down: false,
        last_click: None,
        click_count: 0,
        clipboard: arboard::Clipboard::new().ok(),
        osc52_allow: std::env::var("SAMPA_OSC52").map(|v| v == "allow").unwrap_or(false),
        title: win_title,
        images,
        theme,
        font_size,
        font_size_base: font_size,
        font_family,
        cursor_style,
        ligatures: cfg.font.ligatures,
        opacity: load_opacity(),
        cursor_on: true,
        blink,
        help_on: false,
        keys: Keybindings::load(),
        preedit: String::new(),
        preedit_cursor: None,
        bell_until: None,
        search_on: false,
        search_query: String::new(),
        search_matches: Vec::new(),
        search_idx: 0,
        palette_on: false,
        palette_query: String::new(),
        palette_all: Vec::new(),
        palette_filtered: Vec::new(),
        palette_idx: 0,
        man_on: false,
        man_cmd: String::new(),
        man_lines: Vec::new(),
        man_scroll: 0,
        man_loading: false,
        input_line: String::new(),
        preview_on: false,
        preview_text: String::new(),
        preview_ran: false,
        preview_line: String::new(),
        preview_gen: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}

/// Create a tab: channels, VT state (with config palette + scrollback), PTY, image
/// layer, and a per-session reader→VT pump thread. Returns the `Session` handle.
#[allow(clippy::too_many_arguments)]
fn spawn_session(
    id: u64,
    proxy: &winit::event_loop::EventLoopProxy<UserEvent>,
    cols: u16,
    rows: u16,
    cfg: &sampa_config::Config,
    shell: String,
    args: Vec<String>,
    cwd: Option<String>,
) -> Result<Session> {
    let (app_tx, app_rx) = channel();
    let (reply_tx, reply_rx) = channel::<Reply>();
    let mut parser = Processor::new();
    let mut term = Term::new(
        alacritty_terminal::term::Config {
            scrolling_history: cfg.scrollback.lines as usize,
            ..TermConfig::default()
        },
        &TermSize::new(cols as usize, rows as usize),
        EventProxy { reply_tx, app_tx },
    );
    parser.advance(&mut term, &color_setup(&cfg.colors));
    let state = Arc::new(Mutex::new(TermState {
        parser,
        term,
        decrqcra: DecrqcraScanner::new(),
        image_scanner: ImageScanner::new(),
        sixel: SixelScanner::new(),
        kitty: KittyScanner::new(),
        dcs: DcsScanner::new(),
        decrqm_shadow: std::collections::HashMap::new(),
    }));
    let images = Arc::new(Mutex::new(ImageStore::default()));
    let (tx, rx) = channel();
    let pty = Arc::new(Mutex::new(spawn(
        SpawnConfig { shell, args, cwd, cols, rows, env: vec![] },
        tx,
    )?));
    thread::spawn({
        let (state, pty, images, proxy) =
            (Arc::clone(&state), Arc::clone(&pty), Arc::clone(&images), proxy.clone());
        move || pump(rx, state, proxy, reply_rx, pty, images, id)
    });
    Ok(Session { id, state, pty, images, app_rx, title: "shell".to_string() })
}

fn pump(
    rx: Receiver<PtyEvent>,
    state: Arc<Mutex<TermState>>,
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    reply_rx: Receiver<Reply>,
    pty: Arc<Mutex<PtyHandle>>,
    image_store: Arc<Mutex<ImageStore>>,
    id: u64,
) {
    for ev in rx {
        match ev {
            PtyEvent::Output(bytes) => {
                // Collect every reply this chunk produces, in stream order, then write
                // them back to the PTY synchronously (a query must be answered before
                // the next output byte is processed — that's what apps block on).
                let mut replies: Vec<Vec<u8>> = Vec::new();
                let mut pty_resize: Option<(u16, u16)> = None;
                let mut image_adds: Vec<(i32, usize, usize, DecodedImage)> = Vec::new();
                if let Ok(mut g) = state.lock() {
                    let g = &mut *g;
                    // Split the feed at each DECRQCRA so the checksum sees the exact
                    // grid state at the query point (§17 conformance).
                    let events = g.decrqcra.feed(&bytes);
                    let mut cursor = 0;
                    for (pos, ev) in events {
                        g.parser.advance(&mut g.term, &bytes[cursor..pos]);
                        cursor = pos;
                        match ev {
                            ScanEvent::Decrqcra(req) => {
                                // DA/DSR/color replies queued so far, then the checksum.
                                replies
                                    .extend(reply_rx.try_iter().map(|r| resolve_reply(r, &g.term)));
                                replies.push(compute_decrqcra(&g.term, &req));
                            }
                            // alacritty ignores DECSTR — apply the soft reset ourselves.
                            ScanEvent::Decstr => g.parser.advance(&mut g.term, DECSTR_RESET),
                            // Selective erase → plain ED/EL at the cursor (no protection).
                            ScanEvent::SelectiveErase { line, ps } => {
                                let fin = if line { 'K' } else { 'J' };
                                g.parser.advance(&mut g.term, format!("\x1b[{ps}{fin}").as_bytes());
                            }
                            // Record a modifiable mode's set/reset for DECRQM reporting.
                            ScanEvent::SetMode { dec, mode, set } => {
                                g.decrqm_shadow.insert((dec, mode), set);
                            }
                            // alacritty ignores XTWINOPS resize — resize the grid here,
                            // and remember to resize the PTY once we release the lock.
                            // `None` dimensions keep the current extent; values are
                            // clamped to a sane range (§13 OOM guard).
                            ScanEvent::Resize { rows, cols } => {
                                let (cur_cols, cur_rows) =
                                    (g.term.grid().columns() as u16, g.term.grid().screen_lines() as u16);
                                let cols = cols.unwrap_or(cur_cols).clamp(1, 1000);
                                let rows = rows.unwrap_or(cur_rows).clamp(1, 1000);
                                g.term.resize(TermSize::new(cols as usize, rows as usize));
                                pty_resize = Some((cols, rows));
                            }
                            // Pixel resize → cells against the fixed cell metrics.
                            ScanEvent::ResizePixels { h, w } => {
                                let (cur_cols, cur_rows) =
                                    (g.term.grid().columns() as u16, g.term.grid().screen_lines() as u16);
                                let cols = w.map(|w| w / CELL_W_PX).unwrap_or(cur_cols).clamp(1, 1000);
                                let rows = h.map(|h| h / CELL_H_PX).unwrap_or(cur_rows).clamp(1, 1000);
                                g.term.resize(TermSize::new(cols as usize, rows as usize));
                                pty_resize = Some((cols, rows));
                            }
                            // alacritty ignores ?1048 — apply the equivalent DECSC/DECRC.
                            ScanEvent::SaveRestoreCursor { save } => {
                                g.parser.advance(&mut g.term, if save { b"\x1b7" } else { b"\x1b8" });
                            }
                            // DECDSR device-status report — answered from fixed values,
                            // DECXCPR from the live cursor; after earlier queued replies.
                            ScanEvent::Decdsr { ps, pid } => {
                                replies
                                    .extend(reply_rx.try_iter().map(|r| resolve_reply(r, &g.term)));
                                let p = g.term.renderable_content().cursor.point;
                                let (row, col) =
                                    ((p.line.0 + 1).max(1) as u16, (p.column.0 + 1) as u16);
                                if let Some(reply) = decdsr_reply(ps, pid, row, col) {
                                    replies.push(reply);
                                }
                            }
                            // Size/state report — answered from the live grid + metrics,
                            // after any DA/DSR/color replies queued earlier in the chunk.
                            ScanEvent::WinopReport(op) => {
                                replies
                                    .extend(reply_rx.try_iter().map(|r| resolve_reply(r, &g.term)));
                                let (cols, rows) =
                                    (g.term.grid().columns() as u16, g.term.grid().screen_lines() as u16);
                                replies.push(winop_report(op, cols, rows));
                            }
                        }
                    }
                    g.parser.advance(&mut g.term, &bytes[cursor..]);
                    replies.extend(reply_rx.try_iter().map(|r| resolve_reply(r, &g.term)));

                    // DECRQSS status-string queries (unhandled by the engine).
                    for pt in g.dcs.feed(&bytes) {
                        replies.push(decrqss_reply(&pt, &g.term));
                    }

                    // Inline images (iTerm2 OSC 1337): decode each, anchor at the
                    // cursor, and reserve vertical space so following text flows below.
                    for payload in g.image_scanner.feed(&bytes) {
                        if let Some(img) = parse_iterm_image(&payload) {
                            let cur = g.term.renderable_content().cursor.point;
                            let (anchor, col) = (cur.line.0, cur.column.0);
                            let base = g.term.grid().history_size();
                            let rows = ((img.height as f32 / LINE_HEIGHT).ceil() as usize).max(1);
                            g.parser.advance(&mut g.term, "\r\n".repeat(rows).as_bytes());
                            image_adds.push((anchor, base, col, img));
                        }
                    }
                    // Sixel graphics (DCS): rasterize + place like an inline image.
                    for payload in g.sixel.feed(&bytes) {
                        if let Some(img) = parse_sixel(&payload) {
                            let cur = g.term.renderable_content().cursor.point;
                            let (anchor, col) = (cur.line.0, cur.column.0);
                            let base = g.term.grid().history_size();
                            let rows = ((img.height as f32 / LINE_HEIGHT).ceil() as usize).max(1);
                            g.parser.advance(&mut g.term, "\r\n".repeat(rows).as_bytes());
                            image_adds.push((anchor, base, col, img));
                        }
                    }
                    // Kitty graphics (APC): decode chunked transmissions, place, and ack.
                    for (control, payload) in g.kitty.feed(&bytes) {
                        replies.extend(kitty_response(&control));
                        if let Some(img) = parse_kitty(&control, &payload) {
                            let cur = g.term.renderable_content().cursor.point;
                            let (anchor, col) = (cur.line.0, cur.column.0);
                            let base = g.term.grid().history_size();
                            let rows = ((img.height as f32 / LINE_HEIGHT).ceil() as usize).max(1);
                            g.parser.advance(&mut g.term, "\r\n".repeat(rows).as_bytes());
                            image_adds.push((anchor, base, col, img));
                        }
                    }
                    // Correct alacritty's DECRQM replies (§17): permanently-reset modes
                    // 0→4, and shadowed modifiable modes to their tracked set/reset state.
                    // Done on the outgoing bytes, keyed by the reply's own mode number.
                    for r in replies.iter_mut() {
                        if let Some(fixed) =
                            decrqm_perm_reset(r).or_else(|| decrqm_modifiable(r, &g.decrqm_shadow))
                        {
                            *r = fixed;
                        }
                    }
                }
                if !image_adds.is_empty() {
                    if let Ok(mut store) = image_store.lock() {
                        for (anchor, base, col, img) in image_adds {
                            store.add(anchor, base, col, img);
                        }
                    }
                }
                if let Some((cols, rows)) = pty_resize {
                    if let Ok(p) = pty.lock() {
                        let _ = p.resize(cols, rows, 0, 0);
                    }
                }
                if !replies.is_empty() {
                    if let Ok(mut p) = pty.lock() {
                        for r in &replies {
                            let _ = p.write(r);
                        }
                    }
                }
                let _ = proxy.send_event(UserEvent::Redraw);
            }
            PtyEvent::Exit(info) => {
                let _ = proxy.send_event(UserEvent::SessionExit { id, detail: info.detail });
                break;
            }
        }
    }
}

struct App {
    sessions: Vec<Session>,
    active: usize,
    next_id: u64,
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    // Active-session pointers (Arc-clones re-pointed on switch) so existing call sites
    // (`self.state` / `self.pty` / `self.images`) keep working.
    state: Arc<Mutex<TermState>>,
    pty: Arc<Mutex<PtyHandle>>,
    cols: u16,
    rows: u16,
    window: Option<Arc<Window>>,
    gfx: Option<Gfx>,
    /// AccessKit adapter (created with the window); drives the OS accessibility tree.
    a11y: Option<accesskit_winit::Adapter>,
    modifiers: ModifiersState,
    dumped: bool,
    // mouse / selection
    mouse_col: usize,
    mouse_row: usize,
    mouse_px: f64, // raw pixel X/Y, for tab-bar hit-testing
    mouse_py: f64,
    left_down: bool,
    last_click: Option<(std::time::Instant, usize, usize)>,
    click_count: u8,
    clipboard: Option<arboard::Clipboard>,
    osc52_allow: bool,
    title: String,
    images: Arc<Mutex<ImageStore>>,
    theme: Theme,
    font_size: f32,
    /// The configured font size, restored by zoom-reset (Ctrl+0).
    font_size_base: f32,
    font_family: String,
    cursor_style: CursorStyle,
    ligatures: bool,
    /// Background opacity (1.0 = opaque). Set at launch; a change needs a restart.
    opacity: f32,
    cursor_on: bool,
    blink: bool,
    /// Keyboard-shortcut help overlay (Ctrl+Shift+?).
    help_on: bool,
    /// Live keybindings (defaults + `[keybindings]` config overrides).
    keys: Keybindings,
    /// The in-progress IME composition (preedit) text, drawn at the cursor.
    preedit: String,
    /// The IME cursor byte range within `preedit` (winit's `Ime::Preedit` range).
    preedit_cursor: Option<(usize, usize)>,
    /// When set and still in the future, the visual bell border is flashing.
    bell_until: Option<std::time::Instant>,
    // search overlay
    search_on: bool,
    search_query: String,
    search_matches: Vec<Match>,
    search_idx: usize,
    // command palette
    palette_on: bool,
    palette_query: String,
    palette_all: Vec<String>,
    palette_filtered: Vec<PaletteMatch>,
    palette_idx: usize,
    // man panel
    man_on: bool,
    man_cmd: String,
    man_lines: Vec<String>,
    man_scroll: usize,
    man_loading: bool,
    /// Best-effort current command line, accumulated from keystrokes (reset on Enter).
    input_line: String,
    // command preview (safe auto-run, gated by sampa-preview)
    preview_on: bool,
    preview_text: String,
    preview_ran: bool,
    preview_line: String, // the command the current preview_text is for
    /// Debounce/supersede token: only the newest scheduled preview runs + is accepted.
    preview_gen: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        // The AccessKit adapter must be created before the window is first shown, so
        // start hidden, attach the adapter, then reveal.
        let attrs = Window::default_attributes()
            .with_title(&self.title)
            .with_transparent(self.opacity < 1.0)
            .with_visible(false);
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        let a11y = accesskit_winit::Adapter::with_event_loop_proxy(&window, self.proxy.clone());
        window.set_ime_allowed(true); // enable IME (compose / CJK input)
        window.set_visible(true);
        let gfx = pollster::block_on(Gfx::new(
            Arc::clone(&window),
            Arc::clone(&self.images),
            self.theme,
            self.font_size,
            self.font_family.clone(),
            self.cursor_style,
            self.ligatures,
            self.opacity,
        ));
        self.window = Some(window);
        self.gfx = Some(gfx);
        self.a11y = Some(a11y);
        if let Ok(cmd) = std::env::var("SAMPA_AUTORUN") {
            self.pty_write(format!("{cmd}\r").as_bytes());
        }
        self.request_redraw();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            // Coalesce output bursts: mark the window dirty and let winit deliver a
            // single RedrawRequested per frame (DESIGN §4.3 — one draw per vsync,
            // drop intermediate frames, never intermediate state).
            UserEvent::Redraw => {
                self.drain_app_events();
                self.request_redraw();
            }
            UserEvent::SessionExit { id, detail } => {
                let _ = detail; // (v1: close the tab; --hold could keep it open)
                if let Some(idx) = self.sessions.iter().position(|s| s.id == id) {
                    if self.close_session(idx) {
                        event_loop.exit(); // last tab closed
                    }
                }
            }
            UserEvent::ConfigReload => self.reload_config(),
            UserEvent::ManReady { cmd, lines } => self.man_ready(cmd, lines),
            UserEvent::PreviewReady { gen, line, ran, text } => self.preview_ready(gen, line, ran, text),
            UserEvent::AccessKit(e) => {
                // A screen reader attached / requested the tree — push the current one.
                if matches!(e.window_event, accesskit_winit::WindowEvent::InitialTreeRequested) {
                    self.update_a11y();
                }
            }
            UserEvent::CursorBlink => {
                if self.blink {
                    self.cursor_on = !self.cursor_on;
                    self.request_redraw();
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        // Let the AccessKit adapter observe every window event (focus, geometry, …).
        if let (Some(a), Some(w)) = (self.a11y.as_mut(), self.window.as_ref()) {
            a.process_event(w, &event);
        }
        match event {
            WindowEvent::CloseRequested => {
                for s in &self.sessions {
                    if let Ok(mut p) = s.pty.lock() {
                        let _ = p.kill();
                    }
                }
                event_loop.exit();
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::Resized(size) => {
                self.resize(size.width.max(1), size.height.max(1));
                self.request_redraw();
            }
            WindowEvent::RedrawRequested => self.render_now(),
            WindowEvent::Ime(ime) => match ime {
                // The composed result: send it to the shell like typed input.
                Ime::Commit(text) => {
                    self.preedit.clear();
                    self.preedit_cursor = None;
                    if !text.is_empty() {
                        self.pty_write(text.as_bytes());
                        for c in text.chars() {
                            self.input_line.push(c);
                        }
                        self.schedule_preview();
                        self.scroll(Scroll::Bottom);
                        self.cursor_on = true;
                    }
                    self.request_redraw();
                }
                // Composition in progress: show it underlined at the cursor, with the
                // IME caret marked at the reported byte range.
                Ime::Preedit(text, range) => {
                    self.preedit = text;
                    self.preedit_cursor = range;
                    self.request_redraw();
                }
                Ime::Enabled | Ime::Disabled => {
                    self.preedit.clear();
                    self.preedit_cursor = None;
                    self.request_redraw();
                }
            },
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let m = self.modifiers;
                let action = self.keys.action_for(&event.logical_key, m);
                let is_esc = matches!(&event.logical_key, Key::Named(NamedKey::Escape));
                // Modal overlays capture keys while open, closing on their bound toggle
                // action (so a rebind still closes them) or Esc — scoped so Esc is never
                // swallowed for anyone else. Only one of these can be open at a time.
                if self.help_on {
                    if action == Some(Action::Help) || is_esc {
                        self.help_on = false;
                        self.request_redraw();
                    }
                    return;
                }
                if self.man_on {
                    if action == Some(Action::ToggleMan) {
                        self.man_close();
                    } else {
                        self.man_key(&event.logical_key);
                    }
                    return;
                }
                if self.palette_on {
                    if action == Some(Action::Palette) {
                        self.palette_close();
                    } else {
                        self.palette_key(&event.logical_key, event.text.as_deref());
                    }
                    return;
                }
                if self.search_on {
                    if action == Some(Action::Search) {
                        self.search_close();
                    } else {
                        self.search_key(&event.logical_key, event.text.as_deref(), m.shift_key());
                    }
                    return;
                }
                // Shift+PageUp/PageDown scroll scrollback locally (never reach the PTY).
                if m.shift_key() {
                    match &event.logical_key {
                        Key::Named(NamedKey::PageUp) => {
                            self.scroll(Scroll::PageUp);
                            return;
                        }
                        Key::Named(NamedKey::PageDown) => {
                            self.scroll(Scroll::PageDown);
                            return;
                        }
                        _ => {}
                    }
                }
                // Dispatch a bound app action (never reaches the PTY).
                if let Some(a) = action {
                    self.dispatch(a, event_loop);
                    return;
                }
                let app_cursor = self
                    .state
                    .lock()
                    .map(|g| g.term.mode().contains(TermMode::APP_CURSOR))
                    .unwrap_or(false);
                let bytes = encode_key(
                    &event.logical_key,
                    event.text.as_deref(),
                    m.shift_key(),
                    m.alt_key(),
                    m.control_key(),
                    app_cursor,
                );
                if !bytes.is_empty() {
                    self.track_input(&event.logical_key, event.text.as_deref());
                    self.schedule_preview(); // debounced safe auto-run (no-op if off)
                    self.pty_write(&bytes);
                    self.scroll(Scroll::Bottom); // typing snaps to the live prompt
                    self.cursor_on = true; // show the cursor on activity
                }
            }
            WindowEvent::CursorMoved { position, .. } => self.on_cursor_moved(position.x, position.y),
            WindowEvent::MouseInput { state, button, .. } => {
                self.on_mouse_button(button, state == ElementState::Pressed)
            }
            WindowEvent::MouseWheel { delta, .. } => self.on_mouse_wheel(delta),
            _ => {}
        }
    }
}

/// Which tab becomes active after removing tab `closed` (with `remaining` ≥ 1 left):
/// shift down if the closed tab was before it, else clamp to the new last.
fn active_after_close(active: usize, closed: usize, remaining: usize) -> usize {
    if closed < active {
        active - 1
    } else {
        active.min(remaining - 1)
    }
}

/// Top of the terminal grid in pixels: below the tab bar when it's visible
/// (more than one tab), otherwise the normal top padding.
fn top_offset(ntabs: usize) -> f32 {
    if ntabs > 1 {
        TAB_BAR_H
    } else {
        PAD
    }
}

/// Tab index under a tab-bar click at pixel `px` (window width `w`, `ntabs` ≥ 1),
/// with tabs laid out as equal-width segments across the full width.
fn tab_at_px(px: f64, w: f32, ntabs: usize) -> usize {
    let tabw = (w / ntabs as f32).max(1.0) as f64;
    ((px / tabw).floor() as usize).min(ntabs - 1)
}

/// Linear blend of two sRGB byte colors, `t` in [0,1] toward `b`.
fn blend(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let f = t.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (x as f32 * (1.0 - f) + y as f32 * f).round() as u8;
    [m(a[0], b[0]), m(a[1], b[1]), m(a[2], b[2])]
}

/// All matches of `query` across the whole buffer (scrollback included), left-to-right
/// top-to-bottom, capped at `max`. Empty on an invalid regex or empty query.
fn find_matches<L: EventListener>(term: &Term<L>, query: &str, max: usize) -> Vec<Match> {
    if query.is_empty() {
        return Vec::new();
    }
    let Ok(mut re) = RegexSearch::new(query) else {
        return Vec::new();
    };
    let start = Point::new(term.topmost_line(), Column(0));
    let end = Point::new(term.bottommost_line(), term.last_column());
    RegexIter::new(start, end, Direction::Right, term, &mut re)
        .take(max)
        .collect()
}

/// A ranked palette result: the command name and the char indices to emphasize
/// (`docs/spec-command-palette-search.md` §6).
#[derive(Clone, Debug, PartialEq)]
struct PaletteMatch {
    name: String,
    hits: Vec<usize>,
}

/// Word-boundary characters (spec §4): a token starting right after one of these scores
/// as a boundary hit (so `grep` ranks well in `git-grep`, `ast-grep`).
fn is_word_boundary(c: char) -> bool {
    matches!(c, '-' | '_' | '.' | '/' | '@' | '+')
}

/// First index at which `needle` occurs contiguously in `hay`.
fn find_subslice(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == *needle)
}

/// Score one whitespace-free token (already ASCII-lowercased) against a command's
/// lowercased chars (spec §4): first matching tier wins — exact > substring >
/// subsequence — so any substring match always beats any subsequence-only match.
/// Returns `(score, hit_indices)` or `None` if the token doesn't match at all.
fn score_token(cmd: &[char], t: &[char]) -> Option<(i32, Vec<usize>)> {
    if t.is_empty() {
        return Some((0, Vec::new()));
    }
    // Tier 1: exact.
    if cmd == t {
        return Some((1000, (0..t.len()).collect()));
    }
    // Tier 2: substring at first index `s`.
    if let Some(s) = find_subslice(cmd, t) {
        let mut score = 200 - (s.min(100) as i32);
        if s == 0 {
            score += 100; // prefix
        } else if is_word_boundary(cmd[s - 1]) {
            score += 60; // word boundary
        }
        return Some((score, (s..s + t.len()).collect()));
    }
    // Tier 3: subsequence (greedy, first-fit) with gap penalty + contiguity/prefix bonus.
    let mut score = 0i32;
    let mut hits = Vec::with_capacity(t.len());
    let mut cursor = 0usize;
    let mut prev: Option<usize> = None;
    for &tc in t {
        let idx = (cursor..cmd.len()).find(|&j| cmd[j] == tc)?;
        score -= (idx - cursor) as i32; // chars skipped
        if prev == Some(idx.wrapping_sub(1)) {
            score += 5; // adjacent to previous match
        }
        if idx == 0 {
            score += 10; // prefix
        }
        prev = Some(idx);
        cursor = idx + 1;
        hits.push(idx);
    }
    Some((score, hits))
}

/// Score a command against all query tokens (spec §3/§5): every token must match
/// (logical AND); the score is the sum of token scores minus a mild length penalty,
/// and the hit set is the union of per-token hits. `None` if any token fails.
fn score_command(cmd: &str, tokens: &[Vec<char>]) -> Option<(f64, Vec<usize>)> {
    let lower: Vec<char> = cmd.chars().map(|c| c.to_ascii_lowercase()).collect();
    let mut total = 0i32;
    let mut hits: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    for t in tokens {
        let (s, idxs) = score_token(&lower, t)?;
        total += s;
        hits.extend(idxs);
    }
    Some((total as f64 - lower.len() as f64 * 0.1, hits.into_iter().collect()))
}

/// Rank `all` against `query` per the palette-search spec: whitespace-split into tokens
/// (AND), tiered scoring, best-first (stable so ties keep input order), capped at `max`.
/// An empty query returns the head of the list with no hits (the full command list).
fn filter_commands(all: &[String], query: &str, max: usize) -> Vec<PaletteMatch> {
    let tokens: Vec<Vec<char>> = query
        .split_whitespace()
        .map(|t| t.chars().map(|c| c.to_ascii_lowercase()).collect())
        .collect();
    if tokens.is_empty() {
        return all
            .iter()
            .take(max)
            .map(|c| PaletteMatch { name: c.clone(), hits: Vec::new() })
            .collect();
    }
    let mut scored: Vec<(f64, &String, Vec<usize>)> = all
        .iter()
        .filter_map(|c| score_command(c, &tokens).map(|(s, h)| (s, c, h)))
        .collect();
    // Stable sort by score desc; equal scores retain input order (spec §5.2).
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored
        .into_iter()
        .take(max)
        .map(|(_, c, h)| PaletteMatch { name: c.clone(), hits: h })
        .collect()
}

/// Prettify a chord for display (spec §4): map only the *final* key token to a symbol,
/// leaving modifiers and letter/digit tokens as-is. Unknown tokens display verbatim.
fn prettify_chord(chord: &str) -> String {
    let mut tokens: Vec<String> = chord.split('+').map(str::to_string).collect();
    if let Some(last) = tokens.last_mut() {
        let sym = match last.as_str() {
            "Slash" => "?",
            "Equal" => "=",
            "Plus" => "+",
            "Minus" => "−",
            "Right" => "→",
            "Left" => "←",
            "Up" => "↑",
            "Down" => "↓",
            other => other,
        };
        *last = sym.to_string();
    }
    tokens.join("+")
}

/// A parsed key chord: required modifiers + a normalized key token (spec §4 form, e.g.
/// `T`, `Tab`, `Slash`, `Equal`). Events normalize to the same space for matching.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
struct Chord {
    ctrl: bool,
    shift: bool,
    alt: bool,
    key: String,
}

/// Parse a `Ctrl+Shift+T`-style chord string. `None` if empty or malformed (a missing
/// binding then simply never matches — spec §5).
fn parse_chord(s: &str) -> Option<Chord> {
    let mut c = Chord::default();
    for tok in s.split('+') {
        match tok {
            "Ctrl" | "Control" => c.ctrl = true,
            "Shift" => c.shift = true,
            "Alt" | "Meta" => c.alt = true,
            "" => return None,
            k if c.key.is_empty() => c.key = k.to_string(),
            _ => return None, // two keys → malformed
        }
    }
    (!c.key.is_empty()).then_some(c)
}

/// Normalize a winit logical key to a chord token so a live event compares equal to a
/// parsed chord. Letters uppercase; shifted symbols fold to their base name so the
/// Shift bit is the only signal (`/`+`?`→`Slash`, `=`+`+`→`Equal`, `-`+`_`→`Minus`).
fn normalize_key(key: &Key) -> Option<String> {
    match key {
        Key::Named(NamedKey::Tab) => Some("Tab".into()),
        Key::Named(NamedKey::Enter) => Some("Enter".into()),
        Key::Named(NamedKey::Space) => Some("Space".into()),
        Key::Character(s) => match s.as_str() {
            "/" | "?" => Some("Slash".into()),
            "=" | "+" => Some("Equal".into()),
            "-" | "_" => Some("Minus".into()),
            c if c.chars().count() == 1 => Some(c.to_uppercase()),
            _ => None,
        },
        _ => None,
    }
}

/// The live keybinding table: each action's chord string (for display) + its parsed
/// form (for matching). Built from [`ACTIONS`] defaults with `[keybindings]` overrides.
struct Keybindings {
    map: Vec<(Action, String, Option<Chord>)>,
}

impl Keybindings {
    /// Load defaults, then apply any `[keybindings]` entries from the config file.
    fn load() -> Self {
        let overrides = read_keybinding_overrides();
        let map = ACTIONS
            .iter()
            .map(|(a, key, _label, def)| {
                let s = overrides.get(*key).cloned().unwrap_or_else(|| def.to_string());
                let parsed = parse_chord(&s);
                (*a, s, parsed)
            })
            .collect();
        Self { map }
    }

    /// The action bound to a live key event, if any.
    fn action_for(&self, key: &Key, m: ModifiersState) -> Option<Action> {
        let ekey = normalize_key(key)?;
        let e = Chord { ctrl: m.control_key(), shift: m.shift_key(), alt: m.alt_key(), key: ekey };
        self.map.iter().find(|(_, _, ch)| ch.as_ref() == Some(&e)).map(|(a, _, _)| *a)
    }

    /// The (unprettified) chord string bound to `action`.
    fn chord_str(&self, action: Action) -> &str {
        self.map.iter().find(|(a, _, _)| *a == action).map(|(_, s, _)| s.as_str()).unwrap_or("")
    }

    /// The help overlay's rows — `(prettified chord, label)` — rebuilt each open (§5) so
    /// a rebind shows immediately: the §3a actions in order, then the §3b fixed rows
    /// (Paste from its live binding; scroll/Esc are literals). Empty chords are skipped.
    fn help_rows(&self) -> Vec<(String, String)> {
        let mut rows: Vec<(String, String)> = ACTIONS
            .iter()
            .filter(|(a, _, _, _)| *a != Action::Paste)
            .filter_map(|(a, _, label, _)| {
                let c = self.chord_str(*a);
                (!c.is_empty()).then(|| (prettify_chord(c), label.to_string()))
            })
            .collect();
        let paste = self.chord_str(Action::Paste);
        if !paste.is_empty() {
            rows.push((prettify_chord(paste), "Paste".to_string()));
        }
        rows.push(("Shift+PageUp".to_string(), "Scroll history up".to_string()));
        rows.push(("Shift+PageDown".to_string(), "Scroll history down".to_string()));
        rows.push(("Esc".to_string(), "Close this help, an overlay, or a panel".to_string()));
        rows
    }
}

/// Read `[keybindings]` `key = "chord"` entries from the config file (a tiny hand
/// parser — the shared `sampa-config` doesn't model keybindings). Missing file/section
/// yields no overrides, so the defaults stand.
fn read_keybinding_overrides() -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let Some(path) = config_path() else {
        return out;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return out;
    };
    let mut in_section = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            in_section = line == "[keybindings]";
            continue;
        }
        if in_section {
            if let Some((k, v)) = line.split_once('=') {
                let k = k.trim();
                let v = v.trim().trim_matches(['"', '\'']);
                if !k.is_empty() {
                    out.insert(k.to_string(), v.to_string());
                }
            }
        }
    }
    out
}

/// The command whose man page to show for an input line: the first whitespace token,
/// with a leading `\` stripped (`\ls`→`ls`) and a leading `sudo`/`command` skipped
/// (`sudo grep …`→`grep`).
/// The caret column within the preedit, in cells: the number of chars before the IME
/// cursor's byte offset (each char = one cell for the ASCII/compose case). With no range
/// winit reports, the caret sits at the end of the composition.
fn preedit_caret_cells(text: &str, range: Option<(usize, usize)>) -> usize {
    match range {
        Some((start, _)) => text.get(..start.min(text.len())).unwrap_or(text).chars().count(),
        None => text.chars().count(),
    }
}

fn first_command_token(line: &str) -> &str {
    let mut it = line.split_whitespace();
    let first = it.next().unwrap_or("");
    let first = first.strip_prefix('\\').unwrap_or(first);
    match first {
        "" | "sudo" | "command" => it.next().unwrap_or(""),
        t => t,
    }
}

/// Start index of the visible window so `idx` stays on screen (centered when possible).
fn palette_window(idx: usize, total: usize, visible: usize) -> usize {
    if total <= visible {
        0
    } else {
        idx.saturating_sub(visible / 2).min(total - visible)
    }
}

/// Next match index when stepping (wrapping) over `n` matches (`n` ≥ 1).
fn search_step_index(idx: usize, n: usize, forward: bool) -> usize {
    if forward {
        (idx + 1) % n
    } else {
        (idx + n - 1) % n
    }
}

/// The match to select after a query change: the first whose start line is at or
/// below the current viewport top, else the last (`starts` non-empty, sorted asc).
fn nearest_match_index(starts: &[i32], view_top: i32) -> usize {
    starts
        .iter()
        .position(|&l| l >= view_top)
        .unwrap_or(starts.len().saturating_sub(1))
}

/// The search-bar line: `/<query>` + a caret + a counter (or "no matches").
fn format_search_bar(query: &str, matches: usize, idx: usize) -> String {
    let tail = if query.is_empty() {
        String::new()
    } else if matches == 0 {
        "   no matches".to_string()
    } else {
        format!("   {}/{}", idx + 1, matches)
    };
    format!("  /{}\u{2582}{}", query, tail)
}

/// A compact tab-bar label: `<n>: <title>`, blank titles shown as "shell",
/// long titles truncated with an ellipsis.
fn tab_label(title: &str, i: usize) -> String {
    let t = title.trim();
    let t = if t.is_empty() { "shell" } else { t };
    let mut short: String = t.chars().take(18).collect();
    if t.chars().count() > 18 {
        short.push('…');
    }
    format!("{}: {}", i + 1, short)
}

impl App {
    fn cell_metrics(&self) -> (f32, f32) {
        self.gfx
            .as_ref()
            .map(|g| (g.r.cell_w, g.r.line_h))
            .unwrap_or((FONT_SIZE * 0.6, LINE_HEIGHT))
    }

    fn cell_at(&self, x: f64, y: f64) -> (usize, usize, Side) {
        let (cw, lh) = self.cell_metrics();
        let top = top_offset(self.sessions.len());
        let fx = (x as f32 - PAD) / cw;
        let col = (fx.max(0.0).floor() as usize).min(self.cols.saturating_sub(1) as usize);
        let row = (((y as f32 - top) / lh).max(0.0).floor() as usize)
            .min(self.rows.saturating_sub(1) as usize);
        let side = if fx - fx.floor() > 0.5 { Side::Right } else { Side::Left };
        (col, row, side)
    }

    fn term_mode(&self) -> TermMode {
        self.state.lock().map(|g| *g.term.mode()).unwrap_or(TermMode::NONE)
    }

    /// Report a mouse event to the app if it enabled mouse mode. Returns whether it did.
    fn report_mouse(&mut self, cb_base: u8, pressed: bool, motion: bool) -> bool {
        let mode = self.term_mode();
        let m = self.modifiers;
        match mouse_report(
            mode,
            cb_base,
            self.mouse_col,
            self.mouse_row,
            pressed,
            motion,
            m.shift_key(),
            m.alt_key(),
            m.control_key(),
        ) {
            Some(bytes) => {
                self.pty_write(&bytes);
                true
            }
            None => false,
        }
    }

    fn on_cursor_moved(&mut self, x: f64, y: f64) {
        self.mouse_px = x;
        self.mouse_py = y;
        let (col, row, side) = self.cell_at(x, y);
        let moved = col != self.mouse_col || row != self.mouse_row;
        self.mouse_col = col;
        self.mouse_row = row;

        // Shift forces local selection even when the app grabs the mouse.
        if !self.modifiers.shift_key() {
            let mode = self.term_mode();
            if mode.intersects(TermMode::MOUSE_MODE) {
                let motion_ok = mode.contains(TermMode::MOUSE_MOTION)
                    || (mode.contains(TermMode::MOUSE_DRAG) && self.left_down);
                if motion_ok && moved {
                    let cb = if self.left_down { 0 } else { 3 };
                    self.report_mouse(cb, true, true);
                }
                return;
            }
        }
        // Local selection drag.
        if self.left_down && moved {
            if let Ok(mut g) = self.state.lock() {
                let d = g.term.grid().display_offset() as i32;
                if let Some(sel) = g.term.selection.as_mut() {
                    sel.update(Point::new(Line(row as i32 - d), Column(col)), side);
                }
            }
            self.request_redraw();
        }
    }

    /// Open an OSC-8 hyperlink under the given cell, if any and if its scheme is safe.
    /// Explicit-action only (Ctrl+click); shows the target in the title, never auto-opens.
    fn open_hyperlink_at(&mut self, col: usize, row: usize) -> bool {
        let uri = match self.state.lock() {
            Ok(g) => {
                let d = g.term.grid().display_offset() as i32;
                let line = Line(row as i32 - d);
                let grid = g.term.grid();
                // OSC-8 hyperlink first; otherwise scan the row for a plain URL.
                grid[line][Column(col)]
                    .hyperlink()
                    .map(|h| h.uri().to_string())
                    .or_else(|| {
                        let text: String =
                            (0..grid.columns()).map(|c| grid[line][Column(c)].c).collect();
                        url_at(&text, col)
                    })
            }
            Err(_) => None,
        };
        match uri {
            Some(uri) if is_safe_url(&uri) => {
                if let Some(w) = &self.window {
                    w.set_title(&format!("↗ {}", sanitize_title(&uri)));
                }
                let _ = std::process::Command::new("xdg-open").arg(&uri).spawn();
                true
            }
            _ => false,
        }
    }

    fn on_mouse_button(&mut self, button: MouseButton, pressed: bool) {
        let cb_base = match button {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            _ => return,
        };
        // While the help overlay is open, any click dismisses it (backdrop-to-close).
        if self.help_on && button == MouseButton::Left && pressed {
            self.help_on = false;
            self.request_redraw();
            return;
        }
        // A click in the visual tab bar switches tabs (never reaches the grid).
        if button == MouseButton::Left
            && pressed
            && self.sessions.len() > 1
            && self.mouse_py < TAB_BAR_H as f64
        {
            let w = self
                .window
                .as_ref()
                .map(|win| win.inner_size().width as f32)
                .unwrap_or(1.0);
            self.switch_to(tab_at_px(self.mouse_px, w, self.sessions.len()));
            return;
        }
        // Ctrl+click opens a hyperlink under the cursor (explicit action, §13).
        if button == MouseButton::Left && pressed && self.modifiers.control_key() {
            let (col, row) = (self.mouse_col, self.mouse_row);
            if self.open_hyperlink_at(col, row) {
                return;
            }
        }
        if button == MouseButton::Left {
            self.left_down = pressed;
        }
        // Report to the app unless Shift forces local handling.
        if !self.modifiers.shift_key() && self.report_mouse(cb_base, pressed, false) {
            return;
        }
        match button {
            MouseButton::Left if pressed => {
                let (col, row) = (self.mouse_col, self.mouse_row);
                // Count rapid clicks on the same cell: 1 = char, 2 = word, 3 = line.
                let now = std::time::Instant::now();
                let (same, elapsed) = match self.last_click {
                    Some((t, c, r)) => (c == col && r == row, now.saturating_duration_since(t).as_millis()),
                    None => (false, u128::MAX),
                };
                self.click_count = next_click_count(self.click_count, same, elapsed);
                self.last_click = Some((now, col, row));
                let ty = selection_type_for(self.click_count);
                if let Ok(mut g) = self.state.lock() {
                    let d = g.term.grid().display_offset() as i32;
                    g.term.selection = Some(Selection::new(
                        ty,
                        Point::new(Line(row as i32 - d), Column(col)),
                        Side::Left,
                    ));
                }
                // Word/line selections are complete on press — copy them immediately.
                if self.click_count >= 2 {
                    self.copy_selection();
                }
                self.request_redraw();
            }
            MouseButton::Left => self.copy_selection(), // release: auto-copy
            MouseButton::Middle if pressed => self.paste_clipboard(),
            _ => {}
        }
    }

    fn on_mouse_wheel(&mut self, delta: MouseScrollDelta) {
        let up = match delta {
            MouseScrollDelta::LineDelta(_, y) => y > 0.0,
            MouseScrollDelta::PixelDelta(p) => p.y > 0.0,
        };
        if !self.modifiers.shift_key() && self.report_mouse(if up { 64 } else { 65 }, true, false) {
            return; // app is in mouse mode → wheel goes to it
        }
        self.scroll(if up { Scroll::Delta(3) } else { Scroll::Delta(-3) });
    }

    fn copy_selection(&mut self) {
        let text = self.state.lock().ok().and_then(|g| g.term.selection_to_string());
        if let (Some(text), Some(clip)) = (text, self.clipboard.as_mut()) {
            if !text.is_empty() {
                let _ = clip.set_text(text);
            }
        }
    }

    fn paste_clipboard(&mut self) {
        let Some(text) = self.clipboard.as_mut().and_then(|c| c.get_text().ok()) else {
            return;
        };
        // Strip any embedded paste-end marker (§13 paste-injection guard).
        let clean = text.replace("\x1b[201~", "");
        let bracketed = self.term_mode().contains(TermMode::BRACKETED_PASTE);
        let mut out = Vec::with_capacity(clean.len() + 12);
        if bracketed {
            out.extend_from_slice(b"\x1b[200~");
        }
        out.extend_from_slice(clean.as_bytes());
        if bracketed {
            out.extend_from_slice(b"\x1b[201~");
        }
        self.pty_write(&out);
    }

    fn resize(&mut self, w: u32, h: u32) {
        let (cell_w, line_h) = self.cell_metrics();
        let top = top_offset(self.sessions.len());
        let cols = (((w as f32 - 2.0 * PAD) / cell_w).floor() as u16).max(1);
        let rows = (((h as f32 - top - PAD) / line_h).floor() as u16).max(1);
        if let Some(gfx) = &mut self.gfx {
            gfx.resize(w, h);
        }
        if cols != self.cols || rows != self.rows {
            self.cols = cols;
            self.rows = rows;
            // Resize every tab so switching never needs a reflow.
            for s in &self.sessions {
                if let Ok(mut g) = s.state.lock() {
                    g.term.resize(TermSize::new(cols as usize, rows as usize));
                }
                if let Ok(p) = s.pty.lock() {
                    let _ = p.resize(cols, rows, w as u16, h as u16);
                }
            }
        }
    }

    fn request_redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn pty_write(&self, data: &[u8]) {
        if let Ok(mut p) = self.pty.lock() {
            let _ = p.write(data);
        }
    }

    /// Window title reflects the active tab (with `[i/n]` when there's more than one).
    fn update_title(&self) {
        if let (Some(w), Some(s)) = (&self.window, self.sessions.get(self.active)) {
            let t = if self.sessions.len() > 1 {
                format!("{} [{}/{}]", s.title, self.active + 1, self.sessions.len())
            } else {
                s.title.clone()
            };
            w.set_title(&t);
        }
    }

    /// Make tab `i` active: re-point the active-session Arcs, swap the renderer's image
    /// layer, and repaint.
    fn switch_to(&mut self, i: usize) {
        let Some(s) = self.sessions.get(i) else { return };
        self.active = i;
        self.state = Arc::clone(&s.state);
        self.pty = Arc::clone(&s.pty);
        self.images = Arc::clone(&s.images);
        if let Some(gfx) = &mut self.gfx {
            gfx.r.set_images(Arc::clone(&self.images));
        }
        self.cursor_on = true;
        self.update_title();
        self.request_redraw();
    }

    /// Cycle to the next/previous tab (wrapping); no-op with a single tab.
    fn cycle_tab(&mut self, forward: bool) {
        let n = self.sessions.len();
        if n > 1 {
            let next = if forward { (self.active + 1) % n } else { (self.active + n - 1) % n };
            self.switch_to(next);
        }
    }

    /// Run a bound keyboard action. The single place app shortcuts take effect, so the
    /// keybinding table (defaults + config) fully drives behavior + the help overlay.
    fn dispatch(&mut self, action: Action, event_loop: &ActiveEventLoop) {
        match action {
            Action::NewTab => self.new_tab(),
            Action::CloseTab => {
                if self.close_session(self.active) {
                    event_loop.exit();
                }
            }
            Action::NextTab => self.cycle_tab(true),
            Action::PrevTab => self.cycle_tab(false),
            Action::Copy => self.copy_selection(),
            Action::Paste => self.paste_clipboard(),
            Action::Search => self.search_open(),
            Action::Palette => self.palette_open(),
            Action::ToggleMan => self.man_open(),
            Action::TogglePreview => self.preview_toggle(),
            Action::ZoomIn => self.zoom_by(1.0),
            Action::ZoomOut => self.zoom_by(-1.0),
            Action::ZoomReset => self.zoom_reset(),
            Action::Help => {
                self.help_on = !self.help_on;
                self.request_redraw();
            }
        }
    }

    /// Recompute the grid for the current window size — used when the tab bar
    /// appears/disappears (1↔2 tabs) and the usable height changes.
    fn reflow(&mut self) {
        if let Some(w) = &self.window {
            let sz = w.inner_size();
            self.resize(sz.width, sz.height);
        }
    }

    /// Open a new tab running `$SHELL` at the current grid size, and switch to it.
    fn new_tab(&mut self) {
        let cfg = load_config();
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        match spawn_session(self.next_id, &self.proxy, self.cols, self.rows, &cfg, shell, vec![], None) {
            Ok(session) => {
                self.next_id += 1;
                self.sessions.push(session);
                let showed_bar = self.sessions.len() == 2;
                self.switch_to(self.sessions.len() - 1);
                if showed_bar {
                    self.reflow(); // bar just appeared → grid lost a row
                }
            }
            Err(e) => eprintln!("new tab: {e}"),
        }
    }

    /// Close tab `idx` (reaps its shell). Returns true when no tabs remain.
    fn close_session(&mut self, idx: usize) -> bool {
        if idx >= self.sessions.len() {
            return self.sessions.is_empty();
        }
        if let Ok(mut p) = self.sessions[idx].pty.lock() {
            let _ = p.kill();
        }
        self.sessions.remove(idx);
        if self.sessions.is_empty() {
            return true;
        }
        let hid_bar = self.sessions.len() == 1;
        self.switch_to(active_after_close(self.active, idx, self.sessions.len()));
        if hid_bar {
            self.reflow(); // bar just disappeared → grid regained a row
        }
        false
    }

    /// Open the incremental-search overlay (reusing any previous query).
    fn search_open(&mut self) {
        self.search_on = true;
        self.search_recompute();
    }

    /// Close the overlay and drop the highlights (the query is kept for reopen).
    fn search_close(&mut self) {
        self.search_on = false;
        self.request_redraw();
    }

    /// Handle a key while the overlay owns input: Esc closes, Enter/↓ next match,
    /// Shift+Enter/↑ previous, Backspace edits, printable text extends the query.
    fn search_key(&mut self, key: &Key, text: Option<&str>, shift: bool) {
        match key {
            Key::Named(NamedKey::Escape) => self.search_close(),
            Key::Named(NamedKey::Enter) => self.search_step(!shift), // Shift+Enter = previous
            Key::Named(NamedKey::ArrowDown) => self.search_step(true),
            Key::Named(NamedKey::ArrowUp) => self.search_step(false),
            Key::Named(NamedKey::Backspace) if !self.search_query.is_empty() => {
                self.search_query.pop();
                self.search_recompute();
            }
            _ => {
                if let Some(t) = text {
                    let add: String = t.chars().filter(|c| !c.is_control()).collect();
                    if !add.is_empty() {
                        self.search_query.push_str(&add);
                        self.search_recompute();
                    }
                }
            }
        }
    }

    /// Recompute all matches for the current query across the whole buffer (scrollback
    /// included), pick the one nearest the current view, and scroll it in.
    fn search_recompute(&mut self) {
        self.search_matches.clear();
        self.search_idx = 0;
        let q = self.search_query.clone();
        if !q.is_empty() {
            if let Ok(g) = self.state.lock() {
                self.search_matches = find_matches(&g.term, &q, SEARCH_MAX_MATCHES);
            }
        }
        // Pick the first match at or below the current viewport top, else the last.
        if !self.search_matches.is_empty() {
            let view_top = self
                .state
                .lock()
                .ok()
                .map(|g| -(g.term.grid().display_offset() as i32))
                .unwrap_or(0);
            let starts: Vec<i32> = self.search_matches.iter().map(|m| m.start().line.0).collect();
            self.search_idx = nearest_match_index(&starts, view_top);
            self.search_scroll_to_current();
        }
        self.request_redraw();
    }

    /// Advance to the next/previous match (wrapping) and scroll it into view.
    fn search_step(&mut self, forward: bool) {
        let n = self.search_matches.len();
        if n == 0 {
            return;
        }
        self.search_idx = search_step_index(self.search_idx, n, forward);
        self.search_scroll_to_current();
        self.request_redraw();
    }

    /// Scroll the display so the current match's start line is on screen.
    fn search_scroll_to_current(&mut self) {
        if let Some(m) = self.search_matches.get(self.search_idx).cloned() {
            if let Ok(mut g) = self.state.lock() {
                g.term.scroll_to_point(*m.start());
            }
        }
    }

    /// Paint search highlights onto a freshly built snapshot: every visible match gets
    /// a highlight bg, the current one a brighter bg + dark fg. No-op when closed.
    fn apply_search_highlight(&self, snap: &mut Snapshot) {
        if !self.search_on || self.search_matches.is_empty() {
            return;
        }
        let (rows, cols, offset) = (snap.rows, snap.cols, snap.offset);
        for (i, m) in self.search_matches.iter().enumerate() {
            let current = i == self.search_idx;
            let (s, e) = (m.start(), m.end());
            for abs in s.line.0..=e.line.0 {
                let r = abs + offset;
                if r < 0 || r as usize >= rows {
                    continue;
                }
                let c0 = if abs == s.line.0 { s.column.0 } else { 0 };
                let c1 = if abs == e.line.0 { e.column.0 } else { cols - 1 };
                for c in c0..=c1.min(cols - 1) {
                    let cell = &mut snap.cells[r as usize * cols + c];
                    if current {
                        cell.bg = SEARCH_CURRENT_BG;
                        cell.fg = [0x14, 0x14, 0x14];
                    } else {
                        cell.bg = SEARCH_MATCH_BG;
                    }
                }
            }
        }
    }

    /// The one-line search-bar text: the query plus a match counter (or "no matches").
    fn search_bar_text(&self) -> String {
        format_search_bar(&self.search_query, self.search_matches.len(), self.search_idx)
    }

    /// Open the command palette: enumerate `$PATH` executables once, reset the query.
    fn palette_open(&mut self) {
        let path = std::env::var("PATH").unwrap_or_default();
        self.palette_all = list_executables(&path);
        self.palette_query.clear();
        self.palette_on = true;
        self.palette_refilter();
    }

    fn palette_close(&mut self) {
        self.palette_on = false;
        self.request_redraw();
    }

    /// Keys while the palette owns input: Esc closes, Enter inserts the selected command
    /// at the prompt, ↑/↓ move the selection, Backspace/text edit the query.
    fn palette_key(&mut self, key: &Key, text: Option<&str>) {
        match key {
            Key::Named(NamedKey::Escape) => self.palette_close(),
            Key::Named(NamedKey::Enter) => self.palette_run(),
            Key::Named(NamedKey::ArrowDown) => self.palette_move(true),
            Key::Named(NamedKey::ArrowUp) => self.palette_move(false),
            Key::Named(NamedKey::Backspace) if !self.palette_query.is_empty() => {
                self.palette_query.pop();
                self.palette_refilter();
            }
            _ => {
                if let Some(t) = text {
                    let add: String = t.chars().filter(|c| !c.is_control()).collect();
                    if !add.is_empty() {
                        self.palette_query.push_str(&add);
                        self.palette_refilter();
                    }
                }
            }
        }
    }

    /// Re-run the fuzzy filter for the current query and reset the selection to the top.
    fn palette_refilter(&mut self) {
        self.palette_filtered = filter_commands(&self.palette_all, &self.palette_query, PALETTE_MAX);
        self.palette_idx = 0;
        self.request_redraw();
    }

    /// Move the selection down/up, clamped (no wrap).
    fn palette_move(&mut self, down: bool) {
        let n = self.palette_filtered.len();
        if n == 0 {
            return;
        }
        self.palette_idx = if down {
            (self.palette_idx + 1).min(n - 1)
        } else {
            self.palette_idx.saturating_sub(1)
        };
        self.request_redraw();
    }

    /// Insert the selected command (plus a trailing space) at the prompt, then close.
    /// Deliberately does not append a newline — the user reviews/adds args and runs it.
    fn palette_run(&mut self) {
        if let Some(m) = self.palette_filtered.get(self.palette_idx) {
            let bytes = format!("{} ", m.name).into_bytes();
            self.pty_write(&bytes);
            self.scroll(Scroll::Bottom);
        }
        self.palette_close();
    }

    /// Track the current command line from keystrokes sent to the shell (best-effort:
    /// printable text appends, Backspace pops, Enter/^C/^U reset). Feeds the man panel.
    fn track_input(&mut self, key: &Key, text: Option<&str>) {
        match key {
            Key::Named(NamedKey::Enter) => self.input_line.clear(),
            Key::Named(NamedKey::Backspace) => {
                self.input_line.pop();
            }
            _ => {
                if let Some(t) = text {
                    for c in t.chars() {
                        match c {
                            '\r' | '\n' | '\u{3}' | '\u{15}' => self.input_line.clear(), // Enter/^C/^U
                            c if !c.is_control() => self.input_line.push(c),
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    /// Open the man panel for the first token of the current command line, rendering
    /// `man <cmd>` on a background thread (it spawns a process and can block).
    fn man_open(&mut self) {
        let cmd = first_command_token(&self.input_line).to_string();
        self.man_on = true;
        self.man_scroll = 0;
        self.man_cmd = cmd.clone();
        if cmd.is_empty() {
            self.man_loading = false;
            self.man_lines = vec!["Type a command, then press Ctrl+Shift+M for its man page.".into()];
        } else {
            self.man_loading = true;
            self.man_lines.clear();
            let proxy = self.proxy.clone();
            std::thread::spawn(move || {
                let lines = sampa_man::render(&cmd)
                    .ok()
                    .flatten()
                    .map(|t| t.lines().map(str::to_string).collect());
                let _ = proxy.send_event(UserEvent::ManReady { cmd, lines });
            });
        }
        self.request_redraw();
    }

    fn man_close(&mut self) {
        self.man_on = false;
        self.request_redraw();
    }

    /// Fill the panel when a background `man` render completes (ignored if the panel was
    /// closed or a newer command was requested in the meantime).
    fn man_ready(&mut self, cmd: String, lines: Option<Vec<String>>) {
        if !self.man_on || cmd != self.man_cmd {
            return;
        }
        self.man_loading = false;
        self.man_scroll = 0;
        self.man_lines =
            lines.unwrap_or_else(|| vec![format!("No man page for '{cmd}'.")]);
        self.request_redraw();
    }

    /// Scroll keys while the man panel owns input (Esc handled by the caller).
    fn man_key(&mut self, key: &Key) {
        let page = MAN_VISIBLE.saturating_sub(1).max(1);
        match key {
            Key::Named(NamedKey::Escape) => self.man_close(),
            Key::Named(NamedKey::ArrowDown) => self.man_scroll_by(1, true),
            Key::Named(NamedKey::ArrowUp) => self.man_scroll_by(1, false),
            Key::Named(NamedKey::PageDown) => self.man_scroll_by(page, true),
            Key::Named(NamedKey::PageUp) => self.man_scroll_by(page, false),
            Key::Named(NamedKey::Home) => {
                self.man_scroll = 0;
                self.request_redraw();
            }
            _ => {}
        }
    }

    fn man_scroll_by(&mut self, delta: usize, down: bool) {
        // Keep at least one line visible; the render clamps the window to what fits.
        let max = self.man_lines.len().saturating_sub(1);
        self.man_scroll = if down {
            (self.man_scroll + delta).min(max)
        } else {
            self.man_scroll.saturating_sub(delta)
        };
        self.request_redraw();
    }

    /// Toggle the live command-preview panel; enabling it previews the current line.
    fn preview_toggle(&mut self) {
        self.preview_on = !self.preview_on;
        if self.preview_on {
            self.preview_text.clear();
            self.preview_line.clear();
            self.schedule_preview();
        }
        self.request_redraw();
    }

    /// The session shell's current working directory (via `/proc/<pid>/cwd`), so a
    /// preview's `ls`/`cat` reflect what the user sees.
    fn session_cwd(&self) -> Option<String> {
        let pid = self.pty.lock().ok()?.pid()?;
        std::fs::read_link(format!("/proc/{pid}/cwd"))
            .ok()?
            .to_str()
            .map(str::to_string)
    }

    /// Schedule a debounced preview of the current line. After the debounce, only the
    /// newest request (matching `preview_gen`) runs `run_preview` off-thread; the gate
    /// in `sampa-preview` refuses anything that could write. No-op when the panel is off.
    fn schedule_preview(&mut self) {
        if !self.preview_on {
            return;
        }
        let line = self.input_line.trim().to_string();
        // An empty line (e.g. right after Enter) clears the panel — don't run anything.
        if line.is_empty() {
            self.preview_text.clear();
            self.preview_line.clear();
            self.preview_ran = false;
            self.request_redraw();
            return;
        }
        let gen = self
            .preview_gen
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        let gen_arc = std::sync::Arc::clone(&self.preview_gen);
        let proxy = self.proxy.clone();
        let cwd = self.session_cwd();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(PREVIEW_DEBOUNCE_MS));
            // Superseded by newer input → don't even run the (possibly costly) command.
            if gen_arc.load(std::sync::atomic::Ordering::SeqCst) != gen {
                return;
            }
            let (ran, text) = match sampa_preview::run_preview(&line, cwd.as_deref()) {
                sampa_preview::Preview::Ran(out) => (true, out),
                sampa_preview::Preview::NotRun(reason) => (false, reason),
            };
            let _ = proxy.send_event(UserEvent::PreviewReady { gen, line, ran, text });
        });
    }

    /// Accept a completed preview if it's still the newest and the panel is open.
    fn preview_ready(&mut self, gen: u64, line: String, ran: bool, text: String) {
        if !self.preview_on
            || gen != self.preview_gen.load(std::sync::atomic::Ordering::SeqCst)
        {
            return; // stale or panel closed
        }
        self.preview_line = line;
        self.preview_ran = ran;
        self.preview_text = text;
        self.request_redraw();
    }

    /// The preview panel's header text (command + verdict).
    fn preview_status(&self) -> String {
        if self.preview_line.is_empty() {
            "preview — type a read-only command".to_string()
        } else if self.preview_ran {
            format!("preview ✓ {}", self.preview_line)
        } else {
            format!("preview ✗ {} — {}", self.preview_line, self.preview_text)
        }
    }

    /// Apply VT-raised side effects on the main thread (§13): route query replies to
    /// the PTY, sanitize + set the window title, and gate OSC-52 clipboard writes.
    fn drain_app_events(&mut self) {
        // Drain every tab's UI events (title updates per-tab; window title tracks the
        // active one). Collect first to avoid borrow conflicts with self.clipboard.
        let mut retitle = false;
        let mut bell = false;
        let mut stores: Vec<String> = Vec::new();
        for i in 0..self.sessions.len() {
            while let Ok(ev) = self.sessions[i].app_rx.try_recv() {
                match ev {
                    AppEvent::Title(s) => {
                        self.sessions[i].title = sanitize_title(&s);
                        retitle |= i == self.active;
                    }
                    // OSC-52 write gate: denied by default (SAMPA_OSC52=allow to permit).
                    AppEvent::ClipboardStore(s) if self.osc52_allow => stores.push(s),
                    AppEvent::ClipboardStore(_) => {}
                    AppEvent::Bell => bell |= i == self.active,
                }
            }
        }
        if bell {
            self.bell_until = Some(std::time::Instant::now() + BELL_FLASH);
            self.request_redraw();
        }
        if let Some(clip) = self.clipboard.as_mut() {
            for s in stores {
                let _ = clip.set_text(s);
            }
        }
        if retitle {
            self.update_title();
        }
    }

    /// Scroll the primary-screen scrollback (no-op on the alt screen), then repaint.
    fn scroll(&mut self, s: Scroll) {
        let mut changed = false;
        if let Ok(mut g) = self.state.lock() {
            if !g.term.mode().contains(TermMode::ALT_SCREEN) {
                g.term.scroll_display(s);
                changed = true;
            }
        }
        if changed {
            self.request_redraw();
        }
    }

    /// Live config reload: re-read the file and re-apply theme + font (colors, family,
    /// size). Scrollback size stays as at launch (needs a fresh Term).
    fn reload_config(&mut self) {
        let cfg = load_config();
        self.theme = theme_from(&cfg.colors);
        self.font_size = cfg.font.size.clamp(6.0, 72.0);
        self.font_size_base = self.font_size; // zoom resets to the configured size
        self.keys = Keybindings::load(); // pick up any [keybindings] changes
        self.ligatures = cfg.font.ligatures;
        self.font_family = primary_family(&cfg.font.family);
        self.cursor_style = cfg.cursor.style;
        self.blink = cfg.cursor.blink;
        if !self.blink {
            self.cursor_on = true;
        }
        if std::env::var_os("SAMPA_DEBUG").is_some() {
            eprintln!(
                "config reloaded: size={} family={:?} bg={:?}",
                self.font_size, self.font_family, self.theme.bg
            );
        }
        // Re-load the palette into the VT color table.
        if let Ok(mut g) = self.state.lock() {
            let g = &mut *g;
            g.parser.advance(&mut g.term, &color_setup(&cfg.colors));
        }
        if let Some(gfx) = &mut self.gfx {
            gfx.r.apply_config(
                self.theme,
                self.font_size,
                self.font_family.clone(),
                self.cursor_style,
                self.ligatures,
            );
        }
        // New cell metrics → recompute grid geometry and resize the term/PTY.
        if let Some(size) = self.window.as_ref().map(|w| w.inner_size()) {
            self.resize(size.width.max(1), size.height.max(1));
        }
        self.request_redraw();
    }

    /// Change the font size by `delta` points (Ctrl +/−), clamped, then re-lay the grid.
    fn zoom_by(&mut self, delta: f32) {
        self.apply_font_size((self.font_size + delta).clamp(FONT_SIZE_MIN, FONT_SIZE_MAX));
    }

    /// Restore the configured font size (Ctrl+0).
    fn zoom_reset(&mut self) {
        self.apply_font_size(self.font_size_base);
    }

    /// Apply a new font size: rebuild the renderer's metrics and reflow the grid/PTY.
    fn apply_font_size(&mut self, size: f32) {
        if (size - self.font_size).abs() < f32::EPSILON {
            return;
        }
        self.font_size = size;
        if let Some(gfx) = &mut self.gfx {
            gfx.r.apply_config(self.theme, self.font_size, self.font_family.clone(), self.cursor_style, self.ligatures);
        }
        if let Some(sz) = self.window.as_ref().map(|w| w.inner_size()) {
            self.resize(sz.width.max(1), sz.height.max(1));
        }
        self.request_redraw();
    }

    fn render_now(&mut self) {
        let mut snap = match self.state.lock() {
            Ok(g) => build_snapshot(&g.term, &self.theme, self.cursor_style, self.cursor_on),
            Err(_) => return,
        };
        self.apply_search_highlight(&mut snap);
        if !self.dumped && std::env::var_os("SAMPA_DUMP_GRID").is_some() {
            let text = snap.to_text();
            if text.contains("SEAM_OK") {
                self.dumped = true;
                eprintln!("---GRID DUMP (PTY->VT->render path)---");
                for line in text.lines().filter(|l| !l.trim().is_empty()) {
                    eprintln!("| {}", line.trim_end());
                }
                eprintln!("---END GRID DUMP---");
            }
        }
        let tabs: Vec<String> = self.sessions.iter().map(|s| s.title.clone()).collect();
        let active = self.active;
        let search = self.search_on.then(|| self.search_bar_text());
        // Window the filtered command list around the selection for the dropdown.
        let (pal_rows, pal_sel): (Vec<PaletteMatch>, usize) = if self.palette_on {
            let start = palette_window(self.palette_idx, self.palette_filtered.len(), PALETTE_VISIBLE);
            let rows = self.palette_filtered.iter().skip(start).take(PALETTE_VISIBLE).cloned().collect();
            (rows, self.palette_idx - start)
        } else {
            (Vec::new(), 0)
        };
        let palette = self.palette_on.then(|| PaletteView {
            query: &self.palette_query,
            rows: &pal_rows,
            selected: pal_sel,
        });
        // Bottom panel: the man page (modal) or, if not, the live command preview.
        // Both slice their body to the lines that fit the window.
        let panel_title;
        let panel_body;
        let (_, lh) = self.cell_metrics();
        let win_h = self.window.as_ref().map(|w| w.inner_size().height as f32).unwrap_or(0.0);
        let ptop = top_offset(self.sessions.len());
        let fit = |max: usize| {
            let avail = (win_h - ptop - (lh + 6.0) - 6.0).max(lh);
            ((avail / lh).floor() as usize).clamp(1, max)
        };
        let panel = if self.man_on {
            let visible = fit(MAN_VISIBLE);
            let total = self.man_lines.len();
            let start = self.man_scroll.min(total.saturating_sub(1));
            let end = (start + visible).min(total);
            panel_body = self.man_lines.get(start..end).map(|s| s.join("\n")).unwrap_or_default();
            panel_title = if self.man_loading {
                format!("man {} — loading…", self.man_cmd)
            } else {
                format!(
                    "man {}   {}   ·  ↑/↓ PgUp/PgDn · Esc",
                    self.man_cmd,
                    if total > 0 { format!("{}–{}/{}", start + 1, end, total) } else { "0/0".into() }
                )
            };
            Some(PanelView { title: &panel_title, body: &panel_body })
        } else if self.preview_on {
            let visible = fit(PREVIEW_VISIBLE);
            // The body is the command output (only when it actually ran); a rejection's
            // reason lives in the header instead.
            let src = if self.preview_ran { self.preview_text.as_str() } else { "" };
            let lines: Vec<&str> = src.lines().collect();
            let shown = lines.len().min(visible);
            panel_body = lines[..shown].join("\n");
            let more = if lines.len() > shown { format!("  (+{} lines)", lines.len() - shown) } else { String::new() };
            panel_title = format!("{}{}   ·  Ctrl+Shift+E hides", self.preview_status(), more);
            Some(PanelView { title: &panel_title, body: &panel_body })
        } else {
            None
        };
        // Help overlay: rebuilt each frame it's open (spec §5), so a config reload shows.
        let help = self.help_on.then(|| self.keys.help_rows());
        // IME preedit: the in-progress composition, drawn at the cursor cell.
        let preedit = (!self.preedit.is_empty())
            .then_some(())
            .and(snap.cursor_rc)
            .map(|(r, c)| {
                let caret = preedit_caret_cells(&self.preedit, self.preedit_cursor);
                (self.preedit.as_str(), r, c, caret)
            });
        // Keep the IME candidate window near the cursor while composing.
        if let (Some(w), Some((r, c))) = (&self.window, preedit.map(|(_, r, c, _)| (r, c))) {
            let (cw, lh) = self.cell_metrics();
            let top = top_offset(self.sessions.len());
            let (x, y) = (PAD + c as f32 * cw, top + r as f32 * lh);
            w.set_ime_cursor_area(PhysicalPosition::new(x, y), PhysicalSize::new(cw * 8.0, lh));
        }
        // Visual bell: flash a border while `bell_until` is in the future, re-drawing
        // until it lapses (then one final frame clears it).
        let bell = self.bell_until.is_some_and(|t| std::time::Instant::now() < t);
        if let Some(gfx) = &mut self.gfx {
            gfx.render(&snap, &tabs, active, search.as_deref(), palette.as_ref(), panel.as_ref(), help.as_deref(), preedit, bell);
        }
        if bell {
            self.request_redraw();
        }
        self.update_a11y();
    }

    /// Push the current terminal text to the accessibility tree — but only if a client
    /// (screen reader) is active, so it costs nothing otherwise. The closure captures
    /// owned data (not `self`) to keep clear of the `a11y` borrow.
    fn update_a11y(&mut self) {
        let Some(adapter) = self.a11y.as_mut() else {
            return;
        };
        let title = self
            .sessions
            .get(self.active)
            .map(|s| s.title.clone())
            .unwrap_or_else(|| self.title.clone());
        let state = Arc::clone(&self.state);
        let cursor_style = self.cursor_style;
        adapter.update_if_active(|| {
            let text = state
                .lock()
                .ok()
                .map(|g| build_snapshot(&g.term, &Theme::default(), cursor_style, false).to_text())
                .unwrap_or_default();
            a11y_tree(&title, &text)
        });
    }

}

/// Encode a key press into the bytes a terminal application expects (§8.1).
///
/// Honors application-cursor mode (DECCKM) for the arrow/Home/End keys and encodes
/// modifiers with the xterm `CSI 1 ; <mod> <final>` / `CSI <code> ; <mod> ~` scheme
/// (mod = 1 + shift + 2·alt + 4·ctrl). Alt acts as Meta (ESC prefix). Kitty keyboard
/// protocol and IME/compose remain future work.
fn encode_key(
    key: &Key,
    text: Option<&str>,
    shift: bool,
    alt: bool,
    ctrl: bool,
    app_cursor: bool,
) -> Vec<u8> {
    let modn: u8 = 1 + shift as u8 + 2 * alt as u8 + 4 * ctrl as u8;

    // Cursor keys: CSI form with a modifier, else SS3 under DECCKM, else CSI.
    let cursor = |fin: char| -> Vec<u8> {
        if modn > 1 {
            format!("\x1b[1;{modn}{fin}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{fin}").into_bytes()
        } else {
            format!("\x1b[{fin}").into_bytes()
        }
    };
    // Editing/function keys using the numeric "~" scheme.
    let tilde = |code: u8| -> Vec<u8> {
        if modn > 1 {
            format!("\x1b[{code};{modn}~").into_bytes()
        } else {
            format!("\x1b[{code}~").into_bytes()
        }
    };
    // F1–F4: SS3 form, or CSI with a modifier.
    let ss3 = |fin: char| -> Vec<u8> {
        if modn > 1 {
            format!("\x1b[1;{modn}{fin}").into_bytes()
        } else {
            format!("\x1bO{fin}").into_bytes()
        }
    };
    // Alt = Meta: prefix with ESC.
    let meta = |bytes: &[u8]| -> Vec<u8> {
        if alt {
            let mut v = Vec::with_capacity(bytes.len() + 1);
            v.push(0x1b);
            v.extend_from_slice(bytes);
            v
        } else {
            bytes.to_vec()
        }
    };

    match key {
        Key::Named(n) => match n {
            NamedKey::Enter => meta(b"\r"),
            NamedKey::Tab if shift => b"\x1b[Z".to_vec(), // CBT (back-tab)
            NamedKey::Tab => meta(b"\t"),
            NamedKey::Backspace => meta(&[0x7f]),
            NamedKey::Escape => meta(&[0x1b]),
            NamedKey::Space if ctrl => vec![0], // Ctrl-Space → NUL
            NamedKey::Space => meta(b" "),
            NamedKey::ArrowUp => cursor('A'),
            NamedKey::ArrowDown => cursor('B'),
            NamedKey::ArrowRight => cursor('C'),
            NamedKey::ArrowLeft => cursor('D'),
            NamedKey::Home => cursor('H'),
            NamedKey::End => cursor('F'),
            NamedKey::Insert => tilde(2),
            NamedKey::Delete => tilde(3),
            NamedKey::PageUp => tilde(5),
            NamedKey::PageDown => tilde(6),
            NamedKey::F1 => ss3('P'),
            NamedKey::F2 => ss3('Q'),
            NamedKey::F3 => ss3('R'),
            NamedKey::F4 => ss3('S'),
            NamedKey::F5 => tilde(15),
            NamedKey::F6 => tilde(17),
            NamedKey::F7 => tilde(18),
            NamedKey::F8 => tilde(19),
            NamedKey::F9 => tilde(20),
            NamedKey::F10 => tilde(21),
            NamedKey::F11 => tilde(23),
            NamedKey::F12 => tilde(24),
            _ => text.map(|t| meta(t.as_bytes())).unwrap_or_default(),
        },
        Key::Character(s) if ctrl => {
            let cb = ctrl_byte(s);
            if alt && !cb.is_empty() {
                let mut v = vec![0x1b];
                v.extend_from_slice(&cb);
                v
            } else {
                cb
            }
        }
        Key::Character(s) => meta(s.as_bytes()),
        _ => text.map(|t| meta(t.as_bytes())).unwrap_or_default(),
    }
}

/// Map a character under Ctrl to its C0 control byte; empty if it has no mapping.
fn ctrl_byte(s: &str) -> Vec<u8> {
    let Some(c) = s.chars().next() else {
        return vec![];
    };
    let byte = match c {
        ' ' | '@' | '2' => 0,
        'a'..='z' => c as u8 - b'a' + 1,
        'A'..='Z' => c as u8 - b'A' + 1,
        '[' | '3' => 27,
        '\\' | '4' => 28,
        ']' | '5' => 29,
        '^' | '6' => 30,
        '_' | '7' => 31,
        '?' | '8' => 127,
        _ => return s.as_bytes().to_vec(), // no control code; pass through
    };
    vec![byte]
}

/// Encode a mouse event for an app that enabled mouse reporting (§8.2). Prefers SGR
/// 1006 (`CSI < b ; x ; y M/m`, no column limit) and falls back to legacy X10.
/// `col`/`row` are 0-based; returns `None` when no mouse mode is active.
fn mouse_report(
    mode: TermMode,
    cb_base: u8,
    col: usize,
    row: usize,
    pressed: bool,
    motion: bool,
    shift: bool,
    alt: bool,
    ctrl: bool,
) -> Option<Vec<u8>> {
    if !mode.intersects(TermMode::MOUSE_MODE) {
        return None;
    }
    let mut cb = cb_base;
    if motion {
        cb += 32;
    }
    if shift {
        cb += 4;
    }
    if alt {
        cb += 8;
    }
    if ctrl {
        cb += 16;
    }
    let (x, y) = (col + 1, row + 1);
    if mode.contains(TermMode::SGR_MOUSE) {
        Some(format!("\x1b[<{cb};{x};{y}{}", if pressed { 'M' } else { 'm' }).into_bytes())
    } else {
        // X10: release is button 3; coordinates are byte-offset by 32 (capped).
        let cbx = if pressed { cb } else { (cb & !3) | 3 };
        let e = |v: usize| (32 + v.min(223)) as u8;
        Some(vec![0x1b, b'[', b'M', 32 + cbx, e(x), e(y)])
    }
}

/// Extract the visible grid + attributes + cursor into a `Snapshot`. A **block** cursor
/// inverts its cell here; **bar/underline** cursors are recorded in `cursor` for the
/// renderer to draw a thin quad (and `cursor_on` lets blink hide it).
fn build_snapshot<L: EventListener>(
    term: &Term<L>,
    theme: &Theme,
    cursor_style: CursorStyle,
    cursor_on: bool,
) -> Snapshot {
    let content = term.renderable_content();
    let colors = content.colors;
    let cursor = content.cursor;
    let selection = content.selection;
    let grid = term.grid();
    let rows = grid.screen_lines();
    let cols = grid.columns();

    // Absolute-line coordinates: 0.. is the active screen, negatives are scrollback.
    // Display row r shows absolute line `r - display_offset`.
    let offset = grid.display_offset() as i32;
    let visible = cursor_on && !matches!(cursor.shape, CursorShape::Hidden);
    let cursor_abs = visible.then_some((cursor.point.line.0, cursor.point.column.0));
    let block = matches!(cursor_style, CursorStyle::Block);

    let mut cells = Vec::with_capacity(rows * cols);
    let mut cursor_cell = None;
    let mut cursor_rc = None;
    for r in 0..rows {
        let abs = r as i32 - offset;
        for c in 0..cols {
            let is_cursor = cursor_abs == Some((abs, c));
            if is_cursor {
                cursor_rc = Some((r, c)); // any style — used to anchor the IME preedit
            }
            if is_cursor && !block {
                cursor_cell = Some((r, c)); // bar/underline: drawn by the renderer
            }
            let selected = selection
                .as_ref()
                .is_some_and(|range| in_selection(range, abs, c));
            cells.push(cell_vis(
                &grid[Line(abs)][Column(c)],
                colors,
                is_cursor && block, // only the block cursor inverts the cell
                selected,
                theme.selection,
            ));
        }
    }
    Snapshot { cols, rows, offset, cells, cursor: cursor_cell, cursor_rc, history: grid.history_size() }
}

/// AccessKit node ids: a `Window` root containing one `Terminal` node whose value is
/// the visible grid text (so a screen reader can read it).
const A11Y_ROOT: AccessNodeId = AccessNodeId(0);
const A11Y_TERMINAL: AccessNodeId = AccessNodeId(1);

/// Build the accessibility tree: a window root labeled with the title, and a terminal
/// child whose value is `text` (the visible grid). Rebuilt on each update so the value
/// tracks the screen. Pure — unit-tested independently of the platform adapter.
fn a11y_tree(title: &str, text: &str) -> TreeUpdate {
    let mut root = AccessNode::new(AccessRole::Window);
    root.set_label(title);
    root.set_children(vec![A11Y_TERMINAL]);
    let mut term = AccessNode::new(AccessRole::Terminal);
    term.set_value(text);
    TreeUpdate {
        nodes: vec![(A11Y_ROOT, root), (A11Y_TERMINAL, term)],
        tree: Some(AccessTree::new(A11Y_ROOT)),
        focus: A11Y_TERMINAL,
    }
}

/// Whether grid cell (line, col) falls inside a selection range.
fn in_selection(range: &SelectionRange, line: i32, col: usize) -> bool {
    let (sl, sc) = (range.start.line.0, range.start.column.0);
    let (el, ec) = (range.end.line.0, range.end.column.0);
    if range.is_block {
        line >= sl && line <= el && col >= sc.min(ec) && col <= sc.max(ec)
    } else {
        (line > sl || (line == sl && col >= sc)) && (line < el || (line == el && col <= ec))
    }
}

/// Multi-click window for word/line selection.
const MULTI_CLICK_MS: u128 = 400;

/// Advance the click counter: a rapid click on the same cell counts up (capped at 3,
/// then wraps to 1); anything else restarts at 1.
fn next_click_count(prev: u8, same_cell: bool, elapsed_ms: u128) -> u8 {
    if same_cell && elapsed_ms < MULTI_CLICK_MS && (1..3).contains(&prev) {
        prev + 1
    } else {
        1
    }
}

/// Selection granularity for a click count: 1 = char, 2 = word, 3 = line.
fn selection_type_for(count: u8) -> SelectionType {
    match count {
        2 => SelectionType::Semantic,
        3 => SelectionType::Lines,
        _ => SelectionType::Simple,
    }
}

/// Resolve one grid cell (color + attributes + cursor inversion) for display.
fn cell_vis(
    cell: &alacritty_terminal::term::cell::Cell,
    colors: &Colors,
    is_cursor: bool,
    selected: bool,
    selection_bg: [u8; 3],
) -> CellVis {
    let flags = cell.flags;
    let bold = flags.contains(Flags::BOLD);
    let mut fg = resolve(cell.fg, colors, DEFAULT_FG, bold);
    let mut bg = resolve(cell.bg, colors, DEFAULT_BG, false);
    if flags.contains(Flags::DIM) {
        fg = dim(fg);
    }
    let mut inverse = flags.contains(Flags::INVERSE);
    if is_cursor {
        inverse = !inverse; // block cursor = invert the cell
    }
    if inverse {
        std::mem::swap(&mut fg, &mut bg);
    }
    if flags.contains(Flags::HIDDEN) {
        fg = bg;
    }
    if selected {
        bg = selection_bg;
    }
    let underline = flags.intersects(
        Flags::UNDERLINE
            | Flags::DOUBLE_UNDERLINE
            | Flags::UNDERCURL
            | Flags::DOTTED_UNDERLINE
            | Flags::DASHED_UNDERLINE,
    );
    CellVis {
        c: if cell.c == '\0' { ' ' } else { cell.c },
        fg,
        bg,
        bold,
        italic: flags.contains(Flags::ITALIC),
        underline,
        strike: flags.contains(Flags::STRIKEOUT),
        hyperlink: cell.hyperlink().is_some(),
    }
}

/// Whether a URL is safe to hand to the OS opener — http/https only (§13: never open
/// `file:`, `javascript:`, or other schemes from terminal output).
fn is_safe_url(uri: &str) -> bool {
    let u = uri.trim();
    u.starts_with("http://") || u.starts_with("https://")
}

/// Find a plain http/https URL in `row` covering column `col` (for terminals that
/// don't emit OSC-8). Takes the whitespace-delimited token under the cursor, extracts
/// the URL within it, and trims trailing punctuation.
fn url_at(row: &str, col: usize) -> Option<String> {
    let chars: Vec<char> = row.chars().collect();
    if col >= chars.len() || chars[col].is_whitespace() {
        return None;
    }
    let (mut start, mut end) = (col, col);
    while start > 0 && !chars[start - 1].is_whitespace() {
        start -= 1;
    }
    while end + 1 < chars.len() && !chars[end + 1].is_whitespace() {
        end += 1;
    }
    let token: String = chars[start..=end].iter().collect();
    let pos = token.find("https://").or_else(|| token.find("http://"))?;
    let url = token[pos..].trim_end_matches(|c: char| {
        matches!(c, '.' | ',' | ';' | ':' | ')' | ']' | '}' | '"' | '\'' | '!' | '?')
    });
    is_safe_url(url).then(|| url.to_string())
}

fn rgb_arr(c: Rgb) -> [u8; 3] {
    [c.r, c.g, c.b]
}

fn dim(c: [u8; 3]) -> [u8; 3] {
    [
        (c[0] as f32 * 0.66) as u8,
        (c[1] as f32 * 0.66) as u8,
        (c[2] as f32 * 0.66) as u8,
    ]
}

/// Resolve an ANSI color to RGB, honoring OSC-set overrides then the built-in palette.
fn resolve(color: AnsiColor, colors: &Colors, _default: [u8; 3], bold: bool) -> [u8; 3] {
    match color {
        AnsiColor::Spec(rgb) => rgb_arr(rgb),
        AnsiColor::Indexed(i) => colors[i as usize].map(rgb_arr).unwrap_or_else(|| xterm256(i)),
        AnsiColor::Named(n) => {
            if let Some(rgb) = colors[n as usize] {
                return rgb_arr(rgb);
            }
            let idx = n as usize;
            if idx < 8 {
                ANSI16[idx + if bold { 8 } else { 0 }]
            } else if idx < 16 {
                ANSI16[idx]
            } else {
                match n {
                    NamedColor::Background => DEFAULT_BG,
                    NamedColor::DimForeground => dim(DEFAULT_FG),
                    _ => DEFAULT_FG, // Foreground / Cursor / Bright* / Dim*
                }
            }
        }
    }
}

/// The xterm 256-color palette for indices with no explicit override.
fn xterm256(i: u8) -> [u8; 3] {
    match i {
        0..=15 => ANSI16[i as usize],
        16..=231 => {
            let x = i - 16;
            let step = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
            [step(x / 36 % 6), step(x / 6 % 6), step(x % 6)]
        }
        _ => {
            let v = 8 + 10 * (i - 232);
            [v, v, v]
        }
    }
}

// --- wgpu: solid-quad pass + glyphon text ------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct QuadInstance {
    rect: [f32; 4],  // x, y, w, h in pixels
    color: [f32; 4], // rgba, target-space
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ScreenUniform {
    size: [f32; 2],
    _pad: [f32; 2],
}

const QUAD_SHADER: &str = r#"
struct Screen { size: vec2<f32>, _pad: vec2<f32> };
@group(0) @binding(0) var<uniform> screen: Screen;

struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32> };

@vertex
fn vs(@builtin(vertex_index) vi: u32,
      @location(0) rect: vec4<f32>,
      @location(1) color: vec4<f32>) -> VsOut {
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    let px = rect.xy + corner * rect.zw;
    let ndc = vec2<f32>(px.x / screen.size.x * 2.0 - 1.0, 1.0 - px.y / screen.size.y * 2.0);
    var out: VsOut;
    out.pos = vec4<f32>(ndc, 0.0, 1.0);
    out.color = color;
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> { return in.color; }
"#;

const IMAGE_SHADER: &str = r#"
struct Screen { size: vec2<f32>, _pad: vec2<f32> };
@group(0) @binding(0) var<uniform> screen: Screen;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex
fn vs(@builtin(vertex_index) vi: u32, @location(0) rect: vec4<f32>) -> VsOut {
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    let px = rect.xy + corner * rect.zw;
    let ndc = vec2<f32>(px.x / screen.size.x * 2.0 - 1.0, 1.0 - px.y / screen.size.y * 2.0);
    var out: VsOut;
    out.pos = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = corner;
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> { return textureSample(tex, samp, in.uv); }
"#;

/// Target-agnostic render core: owns the device/queue, glyphon pipeline, and the
/// solid-quad pipeline. `paint` draws a `Snapshot` into any texture view (a window
/// surface frame, or an offscreen texture for `--capture`).
/// Measure a monospace cell's advance for the given font (shape 20 'M's, average).
fn measure_cell_w(fs: &mut FontSystem, size: f32, line_h: f32, family: &str) -> f32 {
    let mut probe = Buffer::new(fs, Metrics::new(size, line_h));
    probe.set_size(Some(4096.0), Some(line_h));
    probe.set_text(
        "MMMMMMMMMMMMMMMMMMMM",
        &Attrs::new().family(family_of(family)),
        Shaping::Advanced,
        None,
    );
    probe.shape_until_scroll(fs, false);
    probe
        .layout_runs()
        .next()
        .map(|r| r.line_w / 20.0)
        .filter(|w| *w > 0.1)
        .unwrap_or(size * 0.6)
}

struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    srgb: bool,
    cell_w: f32,
    line_h: f32,
    font_system: FontSystem,
    swash_cache: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    buffer: Buffer,
    tab_buffers: Vec<Buffer>,     // one per tab-bar label, grown lazily
    search_buffer: Buffer,        // the search-bar text
    palette_buffers: Vec<Buffer>, // [0] = query line, [1..] = result rows
    panel_title_buffer: Buffer, // bottom-panel header (man/preview)
    panel_body_buffer: Buffer,  // bottom-panel body (multi-line)
    help_buffer: Buffer,        // keyboard-shortcut help overlay (title + rows)
    preedit_buffer: Buffer,     // IME preedit (composition) text
    quad_pipeline: wgpu::RenderPipeline,
    quad_uniform: wgpu::Buffer,
    quad_bind_group: wgpu::BindGroup,
    // inline images
    image_pipeline: wgpu::RenderPipeline,
    image_bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    image_textures: HashMap<u64, (wgpu::Texture, wgpu::BindGroup)>,
    images: Arc<Mutex<ImageStore>>,
    theme: Theme,
    font_family: String,
    cursor_style: CursorStyle,
    ligatures: bool, // grid text uses Advanced shaping when on, Basic (no ligatures) off
    opacity: f32,    // background clear alpha (1.0 = opaque)
    premultiplied: bool, // surface alpha mode is premultiplied → premultiply the clear
}

impl Renderer {
    #[allow(clippy::too_many_arguments)]
    fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        format: wgpu::TextureFormat,
        images: Arc<Mutex<ImageStore>>,
        theme: Theme,
        font_size: f32,
        font_family: String,
        cursor_style: CursorStyle,
        ligatures: bool,
        opacity: f32,
        premultiplied: bool,
    ) -> Self {
        let srgb = format.is_srgb();
        let line_h = (font_size * 1.2).ceil();
        let mut font_system = FontSystem::new();
        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);

        // Measure the monospace advance so the quad grid lines up with the glyphs.
        let cell_w = measure_cell_w(&mut font_system, font_size, line_h, &font_family);
        let buffer = Buffer::new(&mut font_system, Metrics::new(font_size, line_h));
        let search_buffer = Buffer::new(&mut font_system, Metrics::new(font_size, line_h));
        let panel_title_buffer = Buffer::new(&mut font_system, Metrics::new(font_size, line_h));
        let panel_body_buffer = Buffer::new(&mut font_system, Metrics::new(font_size, line_h));
        let help_buffer = Buffer::new(&mut font_system, Metrics::new(font_size, line_h));
        let preedit_buffer = Buffer::new(&mut font_system, Metrics::new(font_size, line_h));

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("quad"),
            source: wgpu::ShaderSource::Wgsl(QUAD_SHADER.into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("quad-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let quad_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quad-uniform"),
            size: std::mem::size_of::<ScreenUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let quad_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("quad-bg"),
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: quad_uniform.as_entire_binding(),
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("quad-layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        const ATTRS: [wgpu::VertexAttribute; 2] =
            wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4];
        let quad_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("quad-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<QuadInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &ATTRS,
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        // Image pipeline: group 0 = screen uniform (reuses `bgl`), group 1 = texture+sampler.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("image-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let image_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("image-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let image_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("image"),
            source: wgpu::ShaderSource::Wgsl(IMAGE_SHADER.into()),
        });
        let image_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("image-layout"),
            bind_group_layouts: &[Some(&bgl), Some(&image_bgl)],
            immediate_size: 0,
        });
        const IMG_ATTRS: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![0 => Float32x4];
        let image_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("image-pipeline"),
            layout: Some(&image_layout),
            vertex: wgpu::VertexState {
                module: &image_shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 16,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &IMG_ATTRS,
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &image_shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        Self {
            device,
            queue,
            srgb,
            cell_w,
            line_h,
            font_system,
            swash_cache,
            viewport,
            atlas,
            text_renderer,
            buffer,
            tab_buffers: Vec::new(),
            search_buffer,
            palette_buffers: Vec::new(),
            panel_title_buffer,
            panel_body_buffer,
            help_buffer,
            preedit_buffer,
            quad_pipeline,
            quad_uniform,
            quad_bind_group,
            image_pipeline,
            image_bgl,
            sampler,
            image_textures: HashMap::new(),
            images,
            theme,
            font_family,
            cursor_style,
            ligatures,
            opacity,
            premultiplied,
        }
    }

    /// Upload any newly-added images to GPU textures and drop textures for evicted
    /// images. Returns the per-image draw rects (in pixels) for the current frame.
    fn sync_images(&mut self, offset: i32, history: usize, top: f32, w: u32, h: u32) -> Vec<(u64, [f32; 4])> {
        let mut rects = Vec::new();
        let Ok(mut store) = self.images.lock() else {
            return rects;
        };
        let live: std::collections::HashSet<u64> = store.images.iter().map(|i| i.id).collect();
        self.image_textures.retain(|id, _| live.contains(id));

        for img in &mut store.images {
            // Lazily upload the pixels the first time we see this image.
            if let Some(rgba) = img.rgba.take() {
                let size = wgpu::Extent3d {
                    width: img.width,
                    height: img.height,
                    depth_or_array_layers: 1,
                };
                let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("image"),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                self.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &tex,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &rgba,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(img.width * 4),
                        rows_per_image: Some(img.height),
                    },
                    size,
                );
                let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
                let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("image-bg"),
                    layout: &self.image_bgl,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&self.sampler),
                        },
                    ],
                });
                self.image_textures.insert(img.id, (tex, bind));
            }
            // Place at (col, absolute anchor + display_offset), natural pixel size.
            let x = PAD + img.col as f32 * self.cell_w;
            let y = top + image_row(img.anchor, img.base_history, history, offset) as f32 * self.line_h;
            if x < w as f32 && y < h as f32 && y + img.height as f32 > 0.0 {
                rects.push((img.id, [x, y, img.width as f32, img.height as f32]));
            }
        }
        rects
    }

    /// Re-apply theme + font at runtime (live config reload). Re-measures the cell
    /// advance and rebuilds the text buffer for the new metrics.
    fn apply_config(
        &mut self,
        theme: Theme,
        font_size: f32,
        font_family: String,
        cursor_style: CursorStyle,
        ligatures: bool,
    ) {
        self.theme = theme;
        self.font_family = font_family;
        self.cursor_style = cursor_style;
        self.ligatures = ligatures;
        self.line_h = (font_size * 1.2).ceil();
        self.cell_w = measure_cell_w(&mut self.font_system, font_size, self.line_h, &self.font_family);
        self.buffer = Buffer::new(&mut self.font_system, Metrics::new(font_size, self.line_h));
        self.search_buffer = Buffer::new(&mut self.font_system, Metrics::new(font_size, self.line_h));
        self.panel_title_buffer = Buffer::new(&mut self.font_system, Metrics::new(font_size, self.line_h));
        self.panel_body_buffer = Buffer::new(&mut self.font_system, Metrics::new(font_size, self.line_h));
        self.help_buffer = Buffer::new(&mut self.font_system, Metrics::new(font_size, self.line_h));
        self.preedit_buffer = Buffer::new(&mut self.font_system, Metrics::new(font_size, self.line_h));
        // Overlay row buffers carry the old line height — drop so they're rebuilt.
        self.tab_buffers.clear();
        self.palette_buffers.clear();
    }

    /// Point the renderer at a different tab's image layer (on tab switch); drop the
    /// previous tab's GPU textures (re-uploaded lazily on the next paint).
    fn set_images(&mut self, images: Arc<Mutex<ImageStore>>) {
        self.image_textures.clear();
        self.images = images;
    }

    fn chan(&self, v: u8) -> f32 {
        let n = v as f32 / 255.0;
        if self.srgb {
            if n <= 0.04045 {
                n / 12.92
            } else {
                ((n + 0.055) / 1.055).powf(2.4)
            }
        } else {
            n
        }
    }

    fn color4(&self, rgb: [u8; 3]) -> [f32; 4] {
        [self.chan(rgb[0]), self.chan(rgb[1]), self.chan(rgb[2]), 1.0]
    }

    /// Draw `snap` into `view` (size `w`×`h`). Submits its own command buffer; does
    /// not present or read back — the caller owns the target's lifecycle.
    /// Tab-bar background/segment quads (no-op with ≤1 tab): a dim strip, the active
    /// segment filled with the terminal bg + a cursor-colored accent, thin separators.
    fn tab_bar_quads(&self, tabs: &[String], active: usize, w: u32, out: &mut Vec<QuadInstance>) {
        if tabs.len() <= 1 {
            return;
        }
        let bar_bg = blend(self.theme.bg, self.theme.fg, 0.10);
        let sep = blend(self.theme.bg, self.theme.fg, 0.28);
        out.push(QuadInstance { rect: [0.0, 0.0, w as f32, TAB_BAR_H], color: self.color4(bar_bg) });
        let tabw = w as f32 / tabs.len() as f32;
        for i in 0..tabs.len() {
            let x = i as f32 * tabw;
            if i == active {
                out.push(QuadInstance {
                    rect: [x, 0.0, tabw, TAB_BAR_H],
                    color: self.color4(self.theme.bg),
                });
                out.push(QuadInstance {
                    rect: [x, TAB_BAR_H - 2.0, tabw, 2.0],
                    color: self.color4(self.theme.cursor),
                });
            }
            if i > 0 {
                out.push(QuadInstance {
                    rect: [x, 4.0, 1.0, TAB_BAR_H - 8.0],
                    color: self.color4(sep),
                });
            }
        }
    }

    /// Shape each tab's label into its own reusable buffer (grown lazily). No-op when
    /// the bar is hidden. Active label uses the theme fg; inactive labels fade toward bg.
    fn shape_tab_labels(&mut self, tabs: &[String], active: usize, w: u32) {
        if tabs.len() <= 1 {
            return;
        }
        while self.tab_buffers.len() < tabs.len() {
            let b = Buffer::new(&mut self.font_system, Metrics::new(13.0, self.line_h));
            self.tab_buffers.push(b);
        }
        let tabw = w as f32 / tabs.len() as f32;
        let inactive = blend(self.theme.fg, self.theme.bg, 0.45);
        for (i, title) in tabs.iter().enumerate() {
            let label = tab_label(title, i);
            let c = if i == active { self.theme.fg } else { inactive };
            let attrs = Attrs::new()
                .family(family_of(&self.font_family))
                .color(Color::rgb(c[0], c[1], c[2]));
            let buf = &mut self.tab_buffers[i];
            buf.set_size(Some((tabw - 16.0).max(1.0)), Some(TAB_BAR_H));
            buf.set_rich_text([(label.as_str(), attrs)], &Attrs::new(), Shaping::Advanced, None);
            buf.shape_until_scroll(&mut self.font_system, false);
        }
    }

    /// Shape the palette's query line (buffer 0) + visible result rows (1..) into their
    /// reusable buffers. Row buffers are grown lazily and reused across frames.
    fn shape_palette(&mut self, p: &PaletteView, w: u32, rowh: f32) {
        let need = p.rows.len() + 1;
        let fs = self.line_h / 1.2;
        while self.palette_buffers.len() < need {
            let b = Buffer::new(&mut self.font_system, Metrics::new(fs, self.line_h));
            self.palette_buffers.push(b);
        }
        let fam = family_of(&self.font_family);
        let fg = self.theme.fg;
        let hit = self.theme.cursor; // matched chars emphasized in the accent color
        let width = (w as f32 - 2.0 * PAD - 16.0).max(1.0);
        // Row 0: the query line with a caret.
        {
            let attrs = Attrs::new().family(fam).color(Color::rgb(fg[0], fg[1], fg[2]));
            let query = format!("> {}\u{2582}", p.query);
            let buf = &mut self.palette_buffers[0];
            buf.set_size(Some(width), Some(rowh));
            buf.set_rich_text([(query.as_str(), attrs)], &Attrs::new(), Shaping::Advanced, None);
            buf.shape_until_scroll(&mut self.font_system, false);
        }
        // Rows 1..: command names with matched characters emphasized (spec §6).
        for (ri, m) in p.rows.iter().enumerate() {
            // Coalesce consecutive chars of the same hit/non-hit state into spans.
            let mut spans: Vec<(String, bool)> = Vec::new();
            // `m.hits` is already sorted and deduplicated; walk it with an index.
            let mut hit_idx = 0;
            let hits = &m.hits;
            for (ci, ch) in m.name.chars().enumerate() {
                let is_hit = hit_idx < hits.len() && hits[hit_idx] == ci;
                if is_hit {
                    hit_idx += 1;
                }
                match spans.last_mut() {
                    Some((s, sh)) if *sh == is_hit => s.push(ch),
                    _ => spans.push((ch.to_string(), is_hit)),
                }
            }
            let buf = &mut self.palette_buffers[ri + 1];
            buf.set_size(Some(width), Some(rowh));
            buf.set_rich_text(
                spans.iter().map(|(s, h)| {
                    let c = if *h { hit } else { fg };
                    let mut a = Attrs::new().family(fam).color(Color::rgb(c[0], c[1], c[2]));
                    if *h {
                        a = a.weight(Weight::BOLD);
                    }
                    (s.as_str(), a)
                }),
                &Attrs::new(),
                Shaping::Advanced,
                None,
            );
            buf.shape_until_scroll(&mut self.font_system, false);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint(&mut self, snap: &Snapshot, view: &wgpu::TextureView, w: u32, h: u32, tabs: &[String], active: usize, search: Option<&str>, palette: Option<&PaletteView>, panel: Option<&PanelView>, help: Option<&[(String, String)]>, preedit: Option<(&str, usize, usize, usize)>, bell: bool) {
        // The grid starts below the tab bar when it's shown (more than one tab); the
        // search bar and the bottom panel (man page / command preview) each overlay a
        // strip/panel at the bottom.
        let top = top_offset(tabs.len());
        let panel_header_h = self.line_h + 6.0;
        let panel_top = panel
            .map(|pv| {
                let n = pv.body.lines().count().max(1) as f32;
                (h as f32 - (panel_header_h + n * self.line_h + 6.0)).max(top)
            })
            .unwrap_or(h as f32);
        let grid_bottom = {
            let sb = if search.is_some() { (h as f32 - SEARCH_H).max(0.0) } else { h as f32 };
            sb.min(panel_top)
        };
        // The command palette drops down as a full-width panel below the tab bar.
        let pal_rowh = self.line_h + 6.0;
        let pal_top = top + 2.0;
        let pal_bottom = palette
            .map(|p| pal_top + pal_rowh * (p.rows.len() as f32 + 1.0) + 8.0)
            .unwrap_or(pal_top);
        // The help overlay is a top panel too (title line + a blank + one row each).
        let help_top = top + 2.0;
        let help_bottom = help
            .map(|rows| help_top + (rows.len() as f32 + 3.0) * self.line_h + 12.0)
            .unwrap_or(help_top);
        // Grid text is clipped below whichever top panel is open so it doesn't show
        // through (clamped so a panel taller than the window leaves a valid grid area).
        let grid_top = if help.is_some() {
            help_bottom.min(grid_bottom)
        } else if palette.is_some() {
            pal_bottom.min(grid_bottom)
        } else {
            top
        };
        // Background/cursor quads (drawn under the text) and decoration quads
        // (underline/strikethrough, drawn over it).
        let mut bg_quads: Vec<QuadInstance> = Vec::new();
        let mut deco_quads: Vec<QuadInstance> = Vec::new();
        // Tab-bar segment quads render first, under everything.
        self.tab_bar_quads(tabs, active, w, &mut bg_quads);
        for r in 0..snap.rows {
            let y = top + r as f32 * self.line_h;
            // Rows hidden behind an overlay (palette dropdown / search bar) skip their
            // decorations + cursor, which are drawn after the panel and would leak over it.
            let row_visible = y >= grid_top - 0.5 && y + self.line_h <= grid_bottom + 0.5;
            for c in 0..snap.cols {
                let cell = snap.cell(r, c);
                let x = PAD + c as f32 * self.cell_w;
                if cell.bg != self.theme.bg {
                    bg_quads.push(QuadInstance {
                        rect: [x, y, self.cell_w + 0.5, self.line_h],
                        color: self.color4(cell.bg),
                    });
                }
                if row_visible && (cell.underline || cell.hyperlink) {
                    deco_quads.push(QuadInstance {
                        rect: [x, y + self.line_h - 2.0, self.cell_w, 1.5],
                        color: self.color4(cell.fg),
                    });
                }
                if row_visible && cell.strike {
                    deco_quads.push(QuadInstance {
                        rect: [x, y + self.line_h * 0.45, self.cell_w, 1.5],
                        color: self.color4(cell.fg),
                    });
                }
            }
        }
        // Bar/underline cursor (block inverts its cell in build_snapshot).
        if let Some((r, c)) = snap.cursor.filter(|(r, _)| {
            let y = top + *r as f32 * self.line_h;
            y >= grid_top - 0.5 && y + self.line_h <= grid_bottom + 0.5
        }) {
            let (x, y) = (PAD + c as f32 * self.cell_w, top + r as f32 * self.line_h);
            let rect = match self.cursor_style {
                CursorStyle::Bar => Some([x, y, 2.0, self.line_h]),
                CursorStyle::Underline => Some([x, y + self.line_h - 2.0, self.cell_w, 2.0]),
                CursorStyle::Block => None,
            };
            if let Some(rect) = rect {
                deco_quads.push(QuadInstance { rect, color: self.color4(self.theme.cursor) });
            }
        }
        // Search bar: an opaque strip at the bottom (drawn over the grid) + a top rule.
        if search.is_some() {
            let bar_bg = blend(self.theme.bg, self.theme.fg, 0.14);
            let rule = blend(self.theme.bg, self.theme.fg, 0.30);
            bg_quads.push(QuadInstance {
                rect: [0.0, grid_bottom, w as f32, SEARCH_H],
                color: self.color4(bar_bg),
            });
            bg_quads.push(QuadInstance {
                rect: [0.0, grid_bottom, w as f32, 1.0],
                color: self.color4(rule),
            });
        }
        // Command-palette panel: opaque background (over the grid), input field, the
        // selected-row highlight, and rules. Pushed last so it covers grid cells.
        if let Some(p) = palette {
            let panel = self.color4(blend(self.theme.bg, self.theme.fg, 0.09));
            let inputbg = self.color4(blend(self.theme.bg, self.theme.fg, 0.16));
            let rule = self.color4(blend(self.theme.bg, self.theme.fg, 0.34));
            let wf = w as f32;
            bg_quads.push(QuadInstance { rect: [0.0, pal_top, wf, pal_bottom - pal_top], color: panel });
            bg_quads.push(QuadInstance { rect: [0.0, pal_top, wf, pal_rowh], color: inputbg });
            bg_quads.push(QuadInstance { rect: [0.0, pal_top + pal_rowh, wf, 1.0], color: rule });
            if !p.rows.is_empty() {
                let sel_y = pal_top + pal_rowh * (p.selected as f32 + 1.0);
                bg_quads.push(QuadInstance {
                    rect: [0.0, sel_y, wf, pal_rowh],
                    color: self.color4(self.theme.selection),
                });
            }
            bg_quads.push(QuadInstance { rect: [0.0, pal_bottom - 1.0, wf, 1.0], color: rule });
        }
        // Bottom panel (man/preview): an opaque panel with a header strip + top rule.
        if panel.is_some() {
            let body_bg = self.color4(blend(self.theme.bg, self.theme.fg, 0.07));
            let header = self.color4(blend(self.theme.bg, self.theme.fg, 0.16));
            let rule = self.color4(blend(self.theme.bg, self.theme.fg, 0.34));
            let wf = w as f32;
            bg_quads.push(QuadInstance { rect: [0.0, panel_top, wf, h as f32 - panel_top], color: body_bg });
            bg_quads.push(QuadInstance { rect: [0.0, panel_top, wf, panel_header_h], color: header });
            bg_quads.push(QuadInstance { rect: [0.0, panel_top, wf, 1.0], color: rule });
            bg_quads.push(QuadInstance { rect: [0.0, panel_top + panel_header_h, wf, 1.0], color: rule });
        }
        // Help overlay: an opaque top panel with a bottom rule (modal shortcut list).
        if help.is_some() {
            let body_bg = self.color4(blend(self.theme.bg, self.theme.fg, 0.09));
            let rule = self.color4(blend(self.theme.bg, self.theme.fg, 0.34));
            let wf = w as f32;
            bg_quads.push(QuadInstance { rect: [0.0, help_top, wf, help_bottom - help_top], color: body_bg });
            bg_quads.push(QuadInstance { rect: [0.0, help_bottom - 1.0, wf, 1.0], color: rule });
        }
        // IME preedit: an opaque cell-strip + underline at the cursor (text added below),
        // plus a bright caret bar at the IME cursor position within the composition.
        if let Some((text, r, c, caret)) = preedit {
            let n = text.chars().count().max(1) as f32;
            let x = PAD + c as f32 * self.cell_w;
            let y = top + r as f32 * self.line_h;
            let ww = n * self.cell_w;
            bg_quads.push(QuadInstance {
                rect: [x, y, ww, self.line_h],
                color: self.color4(blend(self.theme.bg, self.theme.cursor, 0.25)),
            });
            deco_quads.push(QuadInstance {
                rect: [x, y + self.line_h - 2.0, ww, 1.5],
                color: self.color4(self.theme.cursor),
            });
            deco_quads.push(QuadInstance {
                rect: [x + caret as f32 * self.cell_w, y, 2.0, self.line_h],
                color: self.color4(self.theme.fg),
            });
        }
        // Visual bell: a bright border frame over everything (drawn in the deco pass).
        if bell {
            let (wf, hf, t) = (w as f32, h as f32, 3.0);
            let c = self.color4(self.theme.cursor);
            for rect in [
                [0.0, 0.0, wf, t],           // top
                [0.0, hf - t, wf, t],        // bottom
                [0.0, 0.0, t, hf],           // left
                [wf - t, 0.0, t, hf],        // right
            ] {
                deco_quads.push(QuadInstance { rect, color: c });
            }
        }

        // Foreground text as per-cell colored rich-text spans.
        let base = Attrs::new().family(family_of(&self.font_family));
        let mut spans: Vec<(String, [u8; 3], bool, bool)> = Vec::new();
        for r in 0..snap.rows {
            for c in 0..snap.cols {
                let cell = snap.cell(r, c);
                let key = (cell.fg, cell.bold, cell.italic);
                match spans.last_mut() {
                    Some((s, fg, b, i)) if (*fg, *b, *i) == key => s.push(cell.c),
                    _ => spans.push((cell.c.to_string(), cell.fg, cell.bold, cell.italic)),
                }
            }
            spans.push(("\n".to_string(), DEFAULT_FG, false, false));
        }

        self.buffer
            .set_size(Some(w as f32 - 2.0 * PAD), Some(h as f32 - top - PAD));
        self.buffer.set_rich_text(
            spans.iter().map(|(s, fg, bold, italic)| {
                let mut a = Attrs::new()
                    .family(family_of(&self.font_family))
                    .color(Color::rgb(fg[0], fg[1], fg[2]));
                if *bold {
                    a = a.weight(Weight::BOLD);
                }
                if *italic {
                    a = a.style(Style::Italic);
                }
                (s.as_str(), a)
            }),
            &base,
            if self.ligatures { Shaping::Advanced } else { Shaping::Basic },
            None,
        );
        self.buffer.shape_until_scroll(&mut self.font_system, false);
        // Shape the tab-bar labels + search text + palette rows (no-ops when hidden)
        // before borrowing the buffers to build text areas.
        self.shape_tab_labels(tabs, active, w);
        if let Some(p) = palette {
            self.shape_palette(p, w, pal_rowh);
        }
        if let Some(text) = search {
            let attrs = Attrs::new()
                .family(family_of(&self.font_family))
                .color(Color::rgb(self.theme.fg[0], self.theme.fg[1], self.theme.fg[2]));
            self.search_buffer.set_size(Some((w as f32 - 2.0 * PAD).max(1.0)), Some(SEARCH_H));
            self.search_buffer.set_rich_text([(text, attrs)], &Attrs::new(), Shaping::Advanced, None);
            self.search_buffer.shape_until_scroll(&mut self.font_system, false);
        }
        if let Some(pv) = panel {
            let fam = family_of(&self.font_family);
            let fg = self.theme.fg;
            let hi = self.theme.cursor;
            let width = (w as f32 - 2.0 * PAD).max(1.0);
            let body_h = (h as f32 - panel_top - panel_header_h).max(self.line_h);
            self.panel_title_buffer.set_size(Some(width), Some(panel_header_h));
            self.panel_title_buffer.set_rich_text(
                [(pv.title, Attrs::new().family(fam).color(Color::rgb(hi[0], hi[1], hi[2])))],
                &Attrs::new(),
                Shaping::Advanced,
                None,
            );
            self.panel_title_buffer.shape_until_scroll(&mut self.font_system, false);
            self.panel_body_buffer.set_size(Some(width), Some(body_h));
            self.panel_body_buffer.set_rich_text(
                [(pv.body, Attrs::new().family(fam).color(Color::rgb(fg[0], fg[1], fg[2])))],
                &Attrs::new(),
                Shaping::Advanced,
                None,
            );
            self.panel_body_buffer.shape_until_scroll(&mut self.font_system, false);
        }
        if let Some(rows) = help {
            let fam = family_of(&self.font_family);
            let fg = self.theme.fg;
            let hi = self.theme.cursor;
            let width = (w as f32 - 2.0 * PAD).max(1.0);
            let height = (help_bottom - help_top).max(self.line_h);
            // Title (accent, bold) then a blank line, then "chord<pad>label" per row —
            // the chord in the accent color, the label in fg, aligned in the monospace.
            let wmax = rows.iter().map(|(c, _)| c.chars().count()).max().unwrap_or(0);
            let mut spans: Vec<(String, [u8; 3], bool)> = Vec::new();
            spans.push(("Keyboard shortcuts\n\n".to_string(), hi, true));
            for (chord, label) in rows {
                let pad = " ".repeat(wmax.saturating_sub(chord.chars().count()) + 3);
                spans.push((chord.clone(), hi, false));
                spans.push((format!("{pad}{label}\n"), fg, false));
            }
            let buf = &mut self.help_buffer;
            buf.set_size(Some(width), Some(height));
            buf.set_rich_text(
                spans.iter().map(|(s, c, bold)| {
                    let mut a = Attrs::new().family(fam).color(Color::rgb(c[0], c[1], c[2]));
                    if *bold {
                        a = a.weight(Weight::BOLD);
                    }
                    (s.as_str(), a)
                }),
                &Attrs::new(),
                Shaping::Advanced,
                None,
            );
            buf.shape_until_scroll(&mut self.font_system, false);
        }
        if let Some((text, _, _, _)) = preedit {
            let fg = self.theme.fg;
            let attrs = Attrs::new().family(family_of(&self.font_family)).color(Color::rgb(fg[0], fg[1], fg[2]));
            self.preedit_buffer.set_size(Some((w as f32).max(1.0)), Some(self.line_h));
            self.preedit_buffer.set_rich_text([(text, attrs)], &Attrs::new(), Shaping::Advanced, None);
            self.preedit_buffer.shape_until_scroll(&mut self.font_system, false);
        }

        self.viewport
            .update(&self.queue, Resolution { width: w, height: h });
        let mut text_areas: Vec<TextArea> = Vec::with_capacity(2 + tabs.len());
        text_areas.push(TextArea {
            buffer: &self.buffer,
            left: PAD,
            top,
            scale: 1.0,
            bounds: TextBounds { left: 0, top: grid_top as i32, right: w as i32, bottom: grid_bottom as i32 },
            default_color: Color::rgb(self.theme.fg[0], self.theme.fg[1], self.theme.fg[2]),
            custom_glyphs: &[],
        });
        if search.is_some() {
            let ltop = grid_bottom + ((SEARCH_H - self.line_h) / 2.0).max(0.0);
            text_areas.push(TextArea {
                buffer: &self.search_buffer,
                left: PAD,
                top: ltop,
                scale: 1.0,
                bounds: TextBounds { left: 0, top: grid_bottom as i32, right: w as i32, bottom: h as i32 },
                default_color: Color::rgb(self.theme.fg[0], self.theme.fg[1], self.theme.fg[2]),
                custom_glyphs: &[],
            });
        }
        if panel.is_some() {
            let fg = Color::rgb(self.theme.fg[0], self.theme.fg[1], self.theme.fg[2]);
            let title_top = panel_top + ((panel_header_h - self.line_h) / 2.0).max(0.0);
            text_areas.push(TextArea {
                buffer: &self.panel_title_buffer,
                left: PAD,
                top: title_top,
                scale: 1.0,
                bounds: TextBounds { left: 0, top: panel_top as i32, right: w as i32, bottom: (panel_top + panel_header_h) as i32 },
                default_color: fg,
                custom_glyphs: &[],
            });
            let body_top = panel_top + panel_header_h + 2.0;
            text_areas.push(TextArea {
                buffer: &self.panel_body_buffer,
                left: PAD,
                top: body_top,
                scale: 1.0,
                bounds: TextBounds { left: 0, top: body_top as i32, right: w as i32, bottom: h as i32 },
                default_color: fg,
                custom_glyphs: &[],
            });
        }
        if help.is_some() {
            let fg = Color::rgb(self.theme.fg[0], self.theme.fg[1], self.theme.fg[2]);
            text_areas.push(TextArea {
                buffer: &self.help_buffer,
                left: PAD + 8.0,
                top: help_top + 8.0,
                scale: 1.0,
                bounds: TextBounds { left: 0, top: help_top as i32, right: w as i32, bottom: help_bottom as i32 },
                default_color: fg,
                custom_glyphs: &[],
            });
        }
        if let Some((_, r, c, _)) = preedit {
            let fg = Color::rgb(self.theme.fg[0], self.theme.fg[1], self.theme.fg[2]);
            let (x, y) = (PAD + c as f32 * self.cell_w, top + r as f32 * self.line_h);
            text_areas.push(TextArea {
                buffer: &self.preedit_buffer,
                left: x,
                top: y,
                scale: 1.0,
                bounds: TextBounds { left: x as i32, top: y as i32, right: w as i32, bottom: (y + self.line_h) as i32 },
                default_color: fg,
                custom_glyphs: &[],
            });
        }
        if let Some(p) = palette {
            let lpad = PAD + 8.0;
            let voff = ((pal_rowh - self.line_h) / 2.0).max(0.0);
            let fg = Color::rgb(self.theme.fg[0], self.theme.fg[1], self.theme.fg[2]);
            // Row 0 is the query line; rows 1.. are the results (buffer i+1).
            for (i, buf) in self.palette_buffers.iter().enumerate().take(p.rows.len() + 1) {
                let ry = pal_top + pal_rowh * i as f32;
                text_areas.push(TextArea {
                    buffer: buf,
                    left: lpad,
                    top: ry + voff,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: 0,
                        top: ry as i32,
                        right: w as i32,
                        bottom: (ry + pal_rowh) as i32,
                    },
                    default_color: fg,
                    custom_glyphs: &[],
                });
            }
        }
        if tabs.len() > 1 {
            let tabw = w as f32 / tabs.len() as f32;
            let label_top = ((TAB_BAR_H - self.line_h) / 2.0).max(0.0);
            for (i, buf) in self.tab_buffers.iter().enumerate().take(tabs.len()) {
                let seg = i as f32 * tabw;
                text_areas.push(TextArea {
                    buffer: buf,
                    left: seg + 10.0,
                    top: label_top,
                    scale: 1.0,
                    bounds: TextBounds {
                        left: seg as i32,
                        top: 0,
                        right: (seg + tabw) as i32 - 4,
                        bottom: TAB_BAR_H as i32,
                    },
                    default_color: Color::rgb(self.theme.fg[0], self.theme.fg[1], self.theme.fg[2]),
                    custom_glyphs: &[],
                });
            }
        }
        if let Err(e) = self.text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            text_areas,
            &mut self.swash_cache,
        ) {
            eprintln!("glyphon prepare: {e:?}");
            return;
        }

        self.queue.write_buffer(
            &self.quad_uniform,
            0,
            bytemuck::bytes_of(&ScreenUniform { size: [w as f32, h as f32], _pad: [0.0, 0.0] }),
        );
        // Upload/evict image textures and build a one-rect vertex buffer per image.
        let image_rects = self.sync_images(snap.offset, snap.history, top, w, h);
        let image_bufs: Vec<(u64, wgpu::Buffer)> = image_rects
            .iter()
            .map(|(id, rect)| {
                let buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("image-quad"),
                    contents: bytemuck::bytes_of(rect),
                    usage: wgpu::BufferUsages::VERTEX,
                });
                (*id, buf)
            })
            .collect();
        let mk_buf = |device: &wgpu::Device, quads: &[QuadInstance]| {
            (!quads.is_empty()).then(|| {
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("quad-instances"),
                    contents: bytemuck::cast_slice(quads),
                    usage: wgpu::BufferUsages::VERTEX,
                })
            })
        };
        let bg_buf = mk_buf(&self.device, &bg_quads);
        let deco_buf = mk_buf(&self.device, &deco_quads);

        let bg = self.color4(self.theme.bg);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sampa-frame"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Background clear at the configured opacity. Under a premultiplied
                        // surface, the color channels are scaled by alpha too.
                        load: wgpu::LoadOp::Clear({
                            let a = self.opacity as f64;
                            let s = if self.premultiplied { a } else { 1.0 };
                            wgpu::Color { r: bg[0] as f64 * s, g: bg[1] as f64 * s, b: bg[2] as f64 * s, a }
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Some(buf) = &bg_buf {
                pass.set_pipeline(&self.quad_pipeline);
                pass.set_bind_group(0, &self.quad_bind_group, &[]);
                pass.set_vertex_buffer(0, buf.slice(..));
                pass.draw(0..4, 0..bg_quads.len() as u32);
            }
            let _ = self
                .text_renderer
                .render(&self.atlas, &self.viewport, &mut pass);
            // Decorations paint over the glyphs.
            if let Some(buf) = &deco_buf {
                pass.set_pipeline(&self.quad_pipeline);
                pass.set_bind_group(0, &self.quad_bind_group, &[]);
                pass.set_vertex_buffer(0, buf.slice(..));
                pass.draw(0..4, 0..deco_quads.len() as u32);
            }
            // Inline images composite on top — but scissored to the visible grid area
            // so they never bleed into the tab bar, palette dropdown, or search bar.
            let y0 = (grid_top as u32).min(h);
            let gh = ((grid_bottom - grid_top).max(0.0) as u32).min(h - y0);
            if !image_bufs.is_empty() && gh > 0 {
                pass.set_scissor_rect(0, y0, w, gh);
                pass.set_pipeline(&self.image_pipeline);
                pass.set_bind_group(0, &self.quad_bind_group, &[]);
                for (id, buf) in &image_bufs {
                    if let Some((_, bind)) = self.image_textures.get(id) {
                        pass.set_bind_group(1, bind, &[]);
                        pass.set_vertex_buffer(0, buf.slice(..));
                        pass.draw(0..4, 0..1);
                    }
                }
            }
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        self.atlas.trim();
    }
}

/// The windowed target: a surface wrapping a `Renderer`.
struct Gfx {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    r: Renderer,
}

impl Gfx {
    #[allow(clippy::too_many_arguments)]
    async fn new(
        window: Arc<Window>,
        images: Arc<Mutex<ImageStore>>,
        theme: Theme,
        font_size: f32,
        font_family: String,
        cursor_style: CursorStyle,
        ligatures: bool,
        opacity: f32,
    ) -> Self {
        let size = window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));
        let instance = wgpu::Instance::default();
        let surface = instance.create_surface(window).expect("create surface");
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
                apply_limit_buckets: false,
            })
            .await
            .expect("request adapter");
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .expect("request device");
        let mut config = surface
            .get_default_config(&adapter, w, h)
            .expect("surface default config");
        // For a translucent background, pick an alpha-compositing surface mode the
        // platform supports (premultiplied preferred). If none, stay opaque.
        let caps = surface.get_capabilities(&adapter);
        let has = |m| caps.alpha_modes.contains(&m);
        let (opacity, premultiplied) = if opacity < 1.0 && has(wgpu::CompositeAlphaMode::PreMultiplied) {
            config.alpha_mode = wgpu::CompositeAlphaMode::PreMultiplied;
            (opacity, true)
        } else if opacity < 1.0 && has(wgpu::CompositeAlphaMode::PostMultiplied) {
            config.alpha_mode = wgpu::CompositeAlphaMode::PostMultiplied;
            (opacity, false)
        } else {
            (1.0, false) // no transparent mode available → opaque
        };
        let format = config.format;
        surface.configure(&device, &config);
        Gfx {
            surface,
            config,
            r: Renderer::new(
                device, queue, format, images, theme, font_size, font_family, cursor_style,
                ligatures, opacity, premultiplied,
            ),
        }
    }

    fn resize(&mut self, w: u32, h: u32) {
        self.config.width = w;
        self.config.height = h;
        self.surface.configure(&self.r.device, &self.config);
    }

    #[allow(clippy::too_many_arguments)]
    fn render(&mut self, snap: &Snapshot, tabs: &[String], active: usize, search: Option<&str>, palette: Option<&PaletteView>, panel: Option<&PanelView>, help: Option<&[(String, String)]>, preedit: Option<(&str, usize, usize, usize)>, bell: bool) {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            _ => return,
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.r.paint(
            snap,
            &view,
            self.config.width,
            self.config.height,
            tabs,
            active,
            search,
            palette,
            panel,
            help,
            preedit,
            bell,
        );
        self.r.queue.present(frame);
    }
}

/// Offscreen render of a color demo to a PNG. Proves the color/cursor pipeline
/// visually without a display server, and doubles as a CI screenshot test.
fn capture(path: &str) -> Result<()> {
    const DEMO: &[u8] = b"\x1b[31mRED \x1b[32mGREEN \x1b[33mYELLOW \x1b[34mBLUE \x1b[35mMAGENTA \x1b[36mCYAN\x1b[0m\r\n\x1b[1;91mBOLD-BRIGHT-RED\x1b[0m  \x1b[7mINVERSE\x1b[0m  \x1b[2mDIM\x1b[0m\r\n\x1b[38;2;255;140;0mTRUECOLOR-ORANGE\x1b[0m  \x1b[44;97m white-on-blue \x1b[0m\r\n\x1b[4mUNDERLINE\x1b[0m  \x1b[9mSTRIKETHROUGH\x1b[0m  \x1b]8;;https://example.com\x1b\\OSC8-LINK\x1b]8;;\x1b\\\r\nSEAM_OK color demo\r\n";
    let (cols, rows) = (64usize, 8usize);
    // Config-aware so the capture reflects the active theme/font (CI screenshot).
    let cfg = load_config();
    let theme = theme_from(&cfg.colors);

    let mut parser: Processor = Processor::new();
    let mut term = Term::new(TermConfig::default(), &TermSize::new(cols, rows), VoidListener);
    parser.advance(&mut term, &color_setup(&cfg.colors));
    parser.advance(&mut term, DEMO);
    // Demonstrate the selection highlight: select "RED GREEN YELLOW" on row 0.
    let mut sel = Selection::new(
        SelectionType::Simple,
        Point::new(Line(0), Column(0)),
        Side::Left,
    );
    sel.update(Point::new(Line(0), Column(15)), Side::Right);
    term.selection = Some(sel);
    let snap = build_snapshot(&term, &theme, cfg.cursor.style, true);

    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        force_fallback_adapter: false,
        compatible_surface: None,
        apply_limit_buckets: false,
    }))
    .expect("request adapter (headless)");
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .expect("request device");
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    // A small gradient image, placed below the text, to exercise the image pipeline.
    let images = Arc::new(Mutex::new(ImageStore::default()));
    {
        let (iw, ih) = (72u32, 28u32);
        let mut rgba = Vec::with_capacity((iw * ih * 4) as usize);
        for y in 0..ih {
            for x in 0..iw {
                rgba.extend_from_slice(&[
                    (x * 255 / iw) as u8,
                    (y * 255 / ih) as u8,
                    140,
                    255,
                ]);
            }
        }
        images
            .lock()
            .unwrap()
            .add(6, 0, 2, DecodedImage { width: iw, height: ih, rgba });
    }
    // Optional sixel demo: SAMPA_CAPTURE_SIXEL=<file> rasterizes a real sixel into view.
    if let Ok(path) = std::env::var("SAMPA_CAPTURE_SIXEL") {
        if let Ok(bytes) = std::fs::read(&path) {
            let mut sc = SixelScanner::new();
            for payload in sc.feed(&bytes) {
                if let Some(img) = parse_sixel(&payload) {
                    images.lock().unwrap().add(4, 0, 30, img);
                }
            }
        }
    }
    // Optional kitty demo: SAMPA_CAPTURE_KITTY=<file> decodes a real kitty APC into view.
    if let Ok(path) = std::env::var("SAMPA_CAPTURE_KITTY") {
        if let Ok(bytes) = std::fs::read(&path) {
            let mut sc = KittyScanner::new();
            for (control, payload) in sc.feed(&bytes) {
                if let Some(img) = parse_kitty(&control, &payload) {
                    images.lock().unwrap().add(4, 0, 30, img);
                }
            }
        }
    }
    let mut r = Renderer::new(
        device,
        queue,
        format,
        images,
        theme,
        cfg.font.size.clamp(6.0, 72.0),
        primary_family(&cfg.font.family),
        cfg.cursor.style,
        std::env::var("SAMPA_CAPTURE_LIGATURES").is_ok() || cfg.font.ligatures,
        load_opacity(), // straight alpha into the capture texture (PNG alpha = opacity)
        false,
    );

    // Optional visual check of the tab bar: SAMPA_CAPTURE_TABS="zsh,vim,htop".
    let demo_tabs: Vec<String> = std::env::var("SAMPA_CAPTURE_TABS")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
        .unwrap_or_default();
    let top = top_offset(demo_tabs.len());
    // Optional palette demo: SAMPA_CAPTURE_PALETTE="query,row1,row2,..." (row 0 selected).
    let demo_pal: Vec<String> = std::env::var("SAMPA_CAPTURE_PALETTE")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
        .unwrap_or_default();
    // Grow the capture so the dropdown (query row + result rows) isn't clipped.
    let pal_extra = if demo_pal.is_empty() {
        0.0
    } else {
        (r.line_h + 6.0) * demo_pal.len() as f32 + 12.0
    };
    // Optional bottom-panel demo: SAMPA_CAPTURE_MAN / SAMPA_CAPTURE_PREVIEW =
    // "cmd|line1|line2|..." (| separates lines); PREVIEW uses the preview header style.
    let is_preview = std::env::var("SAMPA_CAPTURE_PREVIEW").is_ok();
    let demo_man: Vec<String> = std::env::var("SAMPA_CAPTURE_PREVIEW")
        .or_else(|_| std::env::var("SAMPA_CAPTURE_MAN"))
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| s.split('|').map(str::to_string).collect())
        .unwrap_or_default();
    let man_extra = if demo_man.is_empty() {
        0.0
    } else {
        (r.line_h + 6.0) + (demo_man.len().saturating_sub(1)) as f32 * r.line_h + 12.0
    };
    // Optional help-overlay demo: SAMPA_CAPTURE_HELP=1 renders the shortcut list.
    let demo_help = std::env::var("SAMPA_CAPTURE_HELP")
        .is_ok()
        .then(|| Keybindings::load().help_rows());
    let help_extra = demo_help
        .as_ref()
        .map(|rows| (rows.len() as f32 + 3.0) * r.line_h + 12.0)
        .unwrap_or(0.0);

    let w = (PAD * 2.0 + cols as f32 * r.cell_w).ceil() as u32;
    let h = (top + PAD + rows as f32 * r.line_h + pal_extra + man_extra + help_extra).ceil() as u32;

    let tex = r.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("capture"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    let demo_search = std::env::var("SAMPA_CAPTURE_SEARCH").ok().filter(|s| !s.is_empty());
    // Rank the demo rows against the demo query so highlighting shows in the capture.
    let demo_matches: Vec<PaletteMatch> = if demo_pal.len() > 1 {
        filter_commands(&demo_pal[1..], &demo_pal[0], PALETTE_MAX)
    } else {
        Vec::new()
    };
    let demo_palette = (!demo_pal.is_empty()).then(|| PaletteView {
        query: &demo_pal[0],
        rows: &demo_matches,
        selected: 0,
    });
    let man_title;
    let man_body;
    let demo_manview = if demo_man.is_empty() {
        None
    } else {
        man_title = if is_preview {
            format!("preview ✓ {}   ·  Ctrl+Shift+E hides", demo_man[0])
        } else {
            format!("man {}   1–{}/{}   ·  ↑/↓ PgUp/PgDn · Esc", demo_man[0], demo_man.len() - 1, demo_man.len() - 1)
        };
        man_body = demo_man[1..].join("\n");
        Some(PanelView { title: &man_title, body: &man_body })
    };
    r.paint(
        &snap,
        &view,
        w,
        h,
        &demo_tabs,
        1.min(demo_tabs.len().saturating_sub(1)),
        demo_search.as_deref(),
        demo_palette.as_ref(),
        demo_manview.as_ref(),
        demo_help.as_deref(),
        std::env::var("SAMPA_CAPTURE_PREEDIT")
            .ok()
            .as_deref()
            .map(|t| (t, 6usize, 8usize, t.chars().count().min(2))),
        std::env::var("SAMPA_CAPTURE_BELL").is_ok(),
    );

    let bpr = (w * 4).div_ceil(256) * 256; // 256-byte row alignment
    let readback = r.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (bpr * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = r
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bpr),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    r.queue.submit(std::iter::once(enc.finish()));

    readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    r.device
        .poll(wgpu::PollType::Wait { submission_index: None, timeout: None })
        .ok();
    let data = readback
        .slice(..)
        .get_mapped_range()
        .map_err(|e| anyhow::anyhow!("map readback: {e:?}"))?;

    let mut pixels = Vec::with_capacity((w * h * 4) as usize);
    for row in 0..h {
        let start = (row * bpr) as usize;
        pixels.extend_from_slice(&data[start..start + (w * 4) as usize]);
    }
    image::RgbaImage::from_raw(w, h, pixels)
        .ok_or_else(|| anyhow::anyhow!("bad image buffer"))?
        .save(path)?;
    println!("wrote {path} ({w}x{h})");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drive(bytes: &[u8], cols: usize) -> (Term<VoidListener>,) {
        let mut parser: Processor = Processor::new();
        let mut term = Term::new(TermConfig::default(), &TermSize::new(cols, 2), VoidListener);
        parser.advance(&mut term, bytes);
        (term,)
    }
    fn fg_at(term: &Term<VoidListener>, c: usize) -> [u8; 3] {
        let colors = term.renderable_content().colors;
        resolve(term.grid()[Line(0)][Column(c)].fg, colors, DEFAULT_FG, false)
    }

    #[test]
    fn parser_writes_reach_the_grid() {
        let (term,) = drive(b"hi", 20);
        let grid = term.grid();
        let row0: String = (0..grid.columns()).map(|c| grid[Line(0)][Column(c)].c).collect();
        assert!(row0.starts_with("hi"), "grid row 0 was {row0:?}");
    }

    #[test]
    fn sgr_red_resolves_to_ansi_red() {
        let (term,) = drive(b"\x1b[31mX", 10);
        assert_eq!(fg_at(&term, 0), ANSI16[1]);
    }

    #[test]
    fn truecolor_resolves_exact() {
        let (term,) = drive(b"\x1b[38;2;10;20;30mX", 10);
        assert_eq!(fg_at(&term, 0), [10, 20, 30]);
    }

    #[test]
    fn indexed_256_uses_palette() {
        // 208 = orange in the 6x6x6 cube.
        let (term,) = drive(b"\x1b[38;5;208mX", 10);
        assert_eq!(fg_at(&term, 0), xterm256(208));
    }

    #[test]
    fn inverse_swaps_fg_and_bg() {
        let (term,) = drive(b"\x1b[7mX", 10);
        let cell = cell_vis(&term.grid()[Line(0)][Column(0)], term.renderable_content().colors, false, false, SELECTION_BG);
        assert_eq!(cell.fg, DEFAULT_BG, "inverse fg should be default bg");
        assert_eq!(cell.bg, DEFAULT_FG, "inverse bg should be default fg");
    }

    #[test]
    fn cursor_cell_is_inverted() {
        let (term,) = drive(b"X", 10);
        let colors = term.renderable_content().colors;
        let plain = cell_vis(&term.grid()[Line(0)][Column(0)], colors, false, false, SELECTION_BG);
        let curs = cell_vis(&term.grid()[Line(0)][Column(0)], colors, true, false, SELECTION_BG);
        assert_eq!(curs.fg, plain.bg);
        assert_eq!(curs.bg, plain.fg);
    }

    fn tok(s: &str) -> Vec<char> {
        s.chars().map(|c| c.to_ascii_lowercase()).collect()
    }

    #[test]
    fn score_token_tiers_are_ordered() {
        let grep = tok("grep");
        // Exact beats prefix-substring beats boundary-substring beats plain substring.
        let exact = score_token(&tok("grep"), &grep).unwrap().0;
        let prefix = score_token(&tok("grepdiff"), &grep).unwrap().0;
        let boundary = score_token(&tok("git-grep"), &grep).unwrap().0;
        let plain = score_token(&tok("egrep"), &grep).unwrap().0;
        assert_eq!(exact, 1000);
        assert!(exact > prefix && prefix > boundary && boundary > plain, "{exact} {prefix} {boundary} {plain}");
        // Any substring match must beat any subsequence-only match (spec §4).
        let subseq = score_token(&tok("gxrxexp"), &grep).unwrap().0;
        assert!(plain > subseq, "substring {plain} must beat subsequence {subseq}");
        // Non-match excludes.
        assert!(score_token(&tok("ls"), &grep).is_none());
    }

    #[test]
    fn score_token_reports_hit_indices() {
        // Substring hit = the contiguous run.
        assert_eq!(score_token(&tok("egrep"), &tok("grep")).unwrap().1, vec![1, 2, 3, 4]);
        // Subsequence hit = the individual matched indices.
        assert_eq!(score_token(&tok("g_r_e_p"), &tok("grep")).unwrap().1, vec![0, 2, 4, 6]);
    }

    #[test]
    fn filter_ranks_grep_family_and_caps() {
        let all: Vec<String> = [
            "grep", "grepdiff", "git-grep", "egrep", "fgrep", "zgrep", "pgrep",
            "ls", "cat", "gv2ray-proxy-helper",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let out = filter_commands(&all, "grep", PALETTE_MAX);
        let names: Vec<&str> = out.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names[0], "grep", "exact match first: {names:?}");
        assert_eq!(names[1], "grepdiff", "prefix next: {names:?}");
        assert_eq!(names[2], "git-grep", "word-boundary next: {names:?}");
        // Substrings (egrep/fgrep/…) rank above the scattered subsequence match.
        let subseq_pos = names.iter().position(|n| *n == "gv2ray-proxy-helper").unwrap();
        let egrep_pos = names.iter().position(|n| *n == "egrep").unwrap();
        assert!(egrep_pos < subseq_pos, "substrings before subsequence: {names:?}");
        // Non-matches excluded.
        assert!(!names.contains(&"ls") && !names.contains(&"cat"));
    }

    #[test]
    fn filter_multi_token_is_and() {
        let all: Vec<String> = ["git-grep", "grep", "docker-compose", "docker", "git"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // Both tokens must match; hits are the union.
        let out = filter_commands(&all, "git grep", PALETTE_MAX);
        assert_eq!(out.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), vec!["git-grep"]);
        assert_eq!(filter_commands(&all, "doc comp", PALETTE_MAX)[0].name, "docker-compose");
        // Empty query lists all with no hits.
        let empty = filter_commands(&all, "", PALETTE_MAX);
        assert_eq!(empty.len(), all.len());
        assert!(empty.iter().all(|m| m.hits.is_empty()));
        // Case-insensitive.
        assert_eq!(filter_commands(&all, "GREP", PALETTE_MAX)[0].name, "grep");
    }

    // §17 exit criterion: a command previewed by the native build never mutates the
    // filesystem. Exercises the exact call `schedule_preview` makes (`run_preview` with
    // the session cwd), proving the authoritative gate is preserved through our wiring.
    #[test]
    fn preview_never_mutates_the_filesystem() {
        use sampa_preview::Preview;
        let dir = std::env::temp_dir().join(format!("sampa-native-preview-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let victim = dir.join("do-not-delete.txt");
        std::fs::write(&victim, "precious").unwrap();
        let cwd = dir.to_str();

        for line in [
            "rm do-not-delete.txt",
            "rm -rf .",
            "mv do-not-delete.txt gone",
            "echo x > do-not-delete.txt",
            ": > do-not-delete.txt",
            "find . -delete",
            "ls && rm do-not-delete.txt",
        ] {
            let r = sampa_preview::run_preview(line, cwd);
            assert!(matches!(r, Preview::NotRun(_)), "{line:?} should be refused, got {r:?}");
            assert!(victim.exists(), "{line:?} deleted the file!");
            assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious", "{line:?} changed the file!");
        }
        // A read-only command does run, in the session cwd.
        match sampa_preview::run_preview("cat do-not-delete.txt", cwd) {
            Preview::Ran(out) => assert!(out.contains("precious"), "cat output: {out:?}"),
            other => panic!("cat should run, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_reports_cursor_for_any_style() {
        let mut parser: Processor = Processor::new();
        let mut term = Term::new(TermConfig::default(), &TermSize::new(10, 3), VoidListener);
        parser.advance(&mut term, b"ab"); // cursor now at row 0, col 2
        // The IME anchor (cursor_rc) is set for a block cursor too, where `cursor` is None.
        let block = build_snapshot(&term, &Theme::default(), CursorStyle::Block, true);
        assert_eq!(block.cursor, None, "block cursor is drawn by cell inversion");
        assert_eq!(block.cursor_rc, Some((0, 2)), "but the anchor position is still reported");
        // For a bar cursor both are set to the same cell.
        let bar = build_snapshot(&term, &Theme::default(), CursorStyle::Bar, true);
        assert_eq!(bar.cursor, Some((0, 2)));
        assert_eq!(bar.cursor_rc, Some((0, 2)));
        // Hidden cursor → no anchor.
        let off = build_snapshot(&term, &Theme::default(), CursorStyle::Block, false);
        assert_eq!(off.cursor_rc, None);
    }

    #[test]
    fn a11y_tree_exposes_terminal_text() {
        let up = a11y_tree("sampa — zsh", "line one\nline two\n");
        // Root window + terminal child, focus on the terminal.
        assert_eq!(up.nodes.len(), 2);
        assert_eq!(up.focus, A11Y_TERMINAL);
        assert_eq!(up.tree.as_ref().unwrap().root, A11Y_ROOT);
        let (rid, root) = &up.nodes[0];
        assert_eq!(*rid, A11Y_ROOT);
        assert_eq!(root.role(), AccessRole::Window);
        assert_eq!(root.label(), Some("sampa — zsh"));
        assert_eq!(root.children(), &[A11Y_TERMINAL]);
        let (tid, term) = &up.nodes[1];
        assert_eq!(*tid, A11Y_TERMINAL);
        assert_eq!(term.role(), AccessRole::Terminal);
        assert_eq!(term.value(), Some("line one\nline two\n")); // screen text is readable
    }

    fn b64(bytes: &[u8]) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[test]
    fn kitty_control_parsing() {
        assert_eq!(kitty_key("a=T,f=32,s=100", "f"), Some("32"));
        assert_eq!(kitty_key("a=T,f=32", "x"), None);
        assert_eq!(kitty_num("s=640,v=480", "v"), Some(480));
    }

    #[test]
    fn kitty_decodes_raw_rgba_and_rgb() {
        // f=32: two RGBA pixels (red, green), 2×1.
        let rgba = [255, 0, 0, 255, 0, 255, 0, 255];
        let img = parse_kitty("a=T,f=32,s=2,v=1", b64(&rgba).as_bytes()).unwrap();
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(img.rgba, rgba);
        // f=24: two RGB pixels → alpha filled to 255.
        let rgb = [10, 20, 30, 40, 50, 60];
        let img = parse_kitty("a=T,f=24,s=2,v=1", b64(&rgb).as_bytes()).unwrap();
        assert_eq!(img.rgba, [10, 20, 30, 255, 40, 50, 60, 255]);
    }

    #[test]
    fn kitty_requires_display_action_and_acks() {
        // a=t (transmit only, not T) isn't displayed in v1.
        assert!(parse_kitty("a=t,f=32,s=1,v=1", b64(&[1, 2, 3, 4]).as_bytes()).is_none());
        // Ack carries the id; q=2 suppresses it.
        assert_eq!(kitty_response("a=T,i=7"), Some(b"\x1b_Gi=7;OK\x1b\\".to_vec()));
        assert_eq!(kitty_response("a=T,i=7,q=2"), None);
        assert_eq!(kitty_response("a=T"), None); // no id → no ack
    }

    #[test]
    fn kitty_scanner_reassembles_chunks() {
        let mut sc = KittyScanner::new();
        // First chunk (m=1) carries the control; the continuation (m=0) only more data.
        let mut out = sc.feed(b"\x1b_Ga=T,f=32,s=2,v=1,m=1;AAAA\x1b\\");
        assert!(out.is_empty(), "m=1 chunk is buffered, not emitted");
        out = sc.feed(b"\x1b_Gm=0;BBBB\x1b\\");
        assert_eq!(out.len(), 1);
        let (control, payload) = &out[0];
        assert_eq!(control, "a=T,f=32,s=2,v=1,m=1");
        assert_eq!(payload, b"AAAABBBB"); // concatenated across chunks
    }

    #[test]
    fn image_row_rides_with_content() {
        // Inserted at screen row 5 with no scrollback, viewed at the bottom (offset 0).
        assert_eq!(image_row(5, 0, 0, 0), 5);
        // 3 lines of output scrolled in (history grew 0→3): the image rides up to row 2.
        assert_eq!(image_row(5, 0, 3, 0), 2);
        // Scrolling the view up 4 lines into history brings it back down.
        assert_eq!(image_row(5, 0, 3, 4), 6);
        // Enough new output pushes it off the top (negative row → not drawn).
        assert_eq!(image_row(5, 0, 10, 0), -5);
        // Inserted when scrollback already had 8 lines: only *later* growth moves it.
        assert_eq!(image_row(2, 8, 8, 0), 2);
        assert_eq!(image_row(2, 8, 11, 0), -1);
    }

    #[test]
    fn sixel_parses_pixels_and_color() {
        // "#1;2;100;0;0" defines register 1 = red; "@" = 0x40 → bits: 0x40-0x3f = 1 →
        // only bit 0 set → one lit pixel at the top of the band. Two "@" → a 2×1 image.
        let img = parse_sixel(b"0;0;0q#1;2;100;0;0@@").expect("valid sixel");
        assert_eq!((img.width, img.height), (2, 1));
        // Pixel (0,0) is red, opaque.
        assert_eq!(&img.rgba[0..4], &[255, 0, 0, 255]);
        assert_eq!(&img.rgba[4..8], &[255, 0, 0, 255]);
    }

    #[test]
    fn sixel_rle_newline_and_bits() {
        // "!3~" repeats 0x7e (all 6 bits) three times → a 3-wide, 6-tall column;
        // "-" drops a band; "@" lights the top pixel of the new band at x=0.
        let img = parse_sixel(b"q#0;2;0;100;0!3~-@").expect("valid sixel");
        assert_eq!(img.width, 3);
        assert_eq!(img.height, 7); // 6 (first band) + 1 (second band, row 6)
        // Column 0, all six rows lit (green) in the first band.
        for y in 0..6 {
            let idx = ((y * 3) * 4) as usize;
            assert_eq!(&img.rgba[idx..idx + 4], &[0, 255, 0, 255], "row {y}");
        }
        // The new-band pixel is at (0, 6).
        assert_eq!(&img.rgba[((6 * 3) * 4) as usize..][..4], &[0, 255, 0, 255]);
    }

    #[test]
    fn sixel_rejects_non_sixel_dcs() {
        // DECRQSS `$q…` has a non-numeric param before `q` → not a sixel.
        assert!(parse_sixel(b"$q\"p").is_none());
        // No lit pixels → nothing to show.
        assert!(parse_sixel(b"q").is_none());
    }

    #[test]
    fn sixel_scanner_extracts_dcs_payload() {
        let mut sc = SixelScanner::new();
        // ESC P q <data> ESC \
        let out = sc.feed(b"\x1bPq#1~\x1b\\");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], b"q#1~");
        // And the extracted payload rasterizes.
        assert!(parse_sixel(&out[0]).is_some());
    }

    #[test]
    fn chord_prettifying() {
        // Only the final token maps; modifiers/letters/digits stay (spec §4).
        assert_eq!(prettify_chord("Ctrl+Shift+Slash"), "Ctrl+Shift+?");
        assert_eq!(prettify_chord("Ctrl+Equal"), "Ctrl+=");
        assert_eq!(prettify_chord("Ctrl+Minus"), "Ctrl+−");
        assert_eq!(prettify_chord("Ctrl+Shift+Right"), "Ctrl+Shift+→");
        assert_eq!(prettify_chord("Ctrl+Shift+Tab"), "Ctrl+Shift+Tab"); // unknown → verbatim
        assert_eq!(prettify_chord("Ctrl+Shift+T"), "Ctrl+Shift+T");
    }

    /// A Keybindings built straight from ACTIONS defaults (no config file).
    fn default_keys() -> Keybindings {
        let map = ACTIONS
            .iter()
            .map(|(a, _, _, def)| (*a, def.to_string(), parse_chord(def)))
            .collect();
        Keybindings { map }
    }

    #[test]
    fn help_rows_are_complete_and_prettified() {
        let rows = default_keys().help_rows();
        let by_label = |lbl: &str| rows.iter().find(|(_, l)| l == lbl).map(|(c, _)| c.clone());
        // §3a actions + §3b fixed (Paste + 2 scroll + Esc).
        assert_eq!(rows.len(), (ACTIONS.len() - 1) + 4);
        assert_eq!(by_label("This help").as_deref(), Some("Ctrl+Shift+?")); // prettified
        assert_eq!(by_label("New tab").as_deref(), Some("Ctrl+Shift+T"));
        assert_eq!(by_label("Zoom out").as_deref(), Some("Ctrl+−"));
        assert_eq!(by_label("Paste").as_deref(), Some("Ctrl+Shift+V")); // fixed row present
        assert_eq!(by_label("Close this help, an overlay, or a panel").as_deref(), Some("Esc"));
        assert!(rows.iter().all(|(c, _)| !c.is_empty())); // spec §5
    }

    #[test]
    fn chord_parse_and_normalize_round_trip() {
        // Parse produces a normalized token that matches the event normalization.
        assert_eq!(parse_chord("Ctrl+Shift+T"), Some(Chord { ctrl: true, shift: true, alt: false, key: "T".into() }));
        assert_eq!(parse_chord("Ctrl+Equal").unwrap().key, "Equal");
        assert!(parse_chord("").is_none());
        assert!(parse_chord("Ctrl+").is_none());
        assert!(parse_chord("Ctrl+A+B").is_none()); // two keys
        // Shifted symbols fold to the base token so Shift is the only distinguishing bit.
        assert_eq!(normalize_key(&Key::Character("?".into())).as_deref(), Some("Slash"));
        assert_eq!(normalize_key(&Key::Character("/".into())).as_deref(), Some("Slash"));
        assert_eq!(normalize_key(&Key::Character("t".into())).as_deref(), Some("T"));
        assert_eq!(normalize_key(&Key::Named(NamedKey::Tab)).as_deref(), Some("Tab"));
    }

    #[test]
    fn action_for_matches_events() {
        let keys = default_keys();
        let ctrl_shift = ModifiersState::CONTROL | ModifiersState::SHIFT;
        // Ctrl+Shift+T (char arrives uppercased) → NewTab.
        assert_eq!(keys.action_for(&Key::Character("T".into()), ctrl_shift), Some(Action::NewTab));
        // Ctrl+Shift+/ yields "?" → Help.
        assert_eq!(keys.action_for(&Key::Character("?".into()), ctrl_shift), Some(Action::Help));
        // Ctrl+= (no shift) → ZoomIn; adding Shift breaks the match (strict modifiers).
        assert_eq!(keys.action_for(&Key::Character("=".into()), ModifiersState::CONTROL), Some(Action::ZoomIn));
        assert_eq!(keys.action_for(&Key::Character("+".into()), ctrl_shift), None);
        // A bare letter isn't an action (goes to the PTY).
        assert_eq!(keys.action_for(&Key::Character("t".into()), ModifiersState::empty()), None);
    }

    #[test]
    fn rebinding_changes_trigger_and_help_row() {
        // Spec §6: rebinding `help` changes both the match and the displayed chord.
        let mut keys = default_keys();
        for e in keys.map.iter_mut() {
            if e.0 == Action::Help {
                e.1 = "Ctrl+Shift+H".to_string();
                e.2 = parse_chord("Ctrl+Shift+H");
            }
        }
        let ctrl_shift = ModifiersState::CONTROL | ModifiersState::SHIFT;
        // New chord fires; old one no longer does.
        assert_eq!(keys.action_for(&Key::Character("H".into()), ctrl_shift), Some(Action::Help));
        assert_eq!(keys.action_for(&Key::Character("?".into()), ctrl_shift), None);
        // Help row reflects the rebind.
        let help_row = keys.help_rows().into_iter().find(|(_, l)| l == "This help").unwrap();
        assert_eq!(help_row.0, "Ctrl+Shift+H");
    }

    #[test]
    fn native_opacity_key_parsed_and_stripped() {
        let cfg = "opacity = 0.80  # background transparency\n[font]\nsize = 12.0\n";
        // The native key is read...
        assert_eq!(parse_opacity(cfg), Some(0.80));
        assert_eq!(parse_opacity("[font]\nsize = 12.0\n"), None); // absent
        // ...and stripped so the strict sampa-config parse accepts the rest.
        let stripped = strip_native_keys(cfg);
        assert!(!stripped.contains("opacity"));
        assert!(sampa_config::Config::from_toml(&stripped).is_ok());
        // With the key left in, the strict parse would reject it (deny_unknown_fields).
        assert!(sampa_config::Config::from_toml(cfg).is_err());
    }

    #[test]
    fn preedit_caret_position() {
        // Byte range start → char count before it (ASCII: byte == char).
        assert_eq!(preedit_caret_cells("hello", Some((3, 3))), 3);
        assert_eq!(preedit_caret_cells("hello", Some((0, 0))), 0);
        // No range → caret at the end of the composition.
        assert_eq!(preedit_caret_cells("hello", None), 5);
        // Multi-byte: 'あ' is 3 bytes; a start of 3 is 1 char in.
        assert_eq!(preedit_caret_cells("あい", Some((3, 3))), 1);
        // Out-of-range start is clamped (never panics on a byte boundary).
        assert_eq!(preedit_caret_cells("hi", Some((99, 99))), 2);
    }

    #[test]
    fn man_command_token() {
        assert_eq!(first_command_token("grep -rn foo"), "grep");
        assert_eq!(first_command_token("  ls  "), "ls");
        assert_eq!(first_command_token("sudo apt update"), "apt"); // skip sudo
        assert_eq!(first_command_token("command ls"), "ls");
        assert_eq!(first_command_token("\\ls -a"), "ls"); // skip a leading backslash escape
        assert_eq!(first_command_token(""), "");
        assert_eq!(first_command_token("sudo"), ""); // sudo with nothing after
    }

    #[test]
    fn palette_scroll_window() {
        // Fits entirely → always start at 0.
        assert_eq!(palette_window(7, 8, 10), 0);
        // Larger than the window → center the selection, clamped to both ends.
        assert_eq!(palette_window(0, 100, 10), 0);
        assert_eq!(palette_window(50, 100, 10), 45);
        assert_eq!(palette_window(99, 100, 10), 90); // clamped so the last row shows
    }

    #[test]
    fn search_navigation_wraps() {
        assert_eq!(search_step_index(0, 3, true), 1);
        assert_eq!(search_step_index(2, 3, true), 0); // wrap forward
        assert_eq!(search_step_index(0, 3, false), 2); // wrap backward
        assert_eq!(search_step_index(1, 3, false), 0);
    }

    #[test]
    fn search_nearest_match_pick() {
        let starts = [-30, -10, -2, 5];
        assert_eq!(nearest_match_index(&starts, -12), 1); // first at/below view top
        assert_eq!(nearest_match_index(&starts, -2), 2);
        assert_eq!(nearest_match_index(&starts, 100), 3); // none below → last
        assert_eq!(nearest_match_index(&starts, -100), 0);
    }

    #[test]
    fn search_bar_formatting() {
        assert_eq!(format_search_bar("", 0, 0), "  /\u{2582}"); // empty query: no counter
        assert_eq!(format_search_bar("err", 0, 0), "  /err\u{2582}   no matches");
        assert_eq!(format_search_bar("err", 5, 1), "  /err\u{2582}   2/5"); // 1-based counter
    }

    #[test]
    fn tab_bar_geometry() {
        // Bar hidden with ≤1 tab (top is the normal padding), shown with >1.
        assert_eq!(top_offset(1), PAD);
        assert_eq!(top_offset(3), TAB_BAR_H);
        // Equal-width segments; clicks map to the segment under the pixel, clamped.
        assert_eq!(tab_at_px(10.0, 300.0, 3), 0); // segment 0 = [0,100)
        assert_eq!(tab_at_px(150.0, 300.0, 3), 1); // segment 1 = [100,200)
        assert_eq!(tab_at_px(299.0, 300.0, 3), 2); // segment 2 = [200,300)
        assert_eq!(tab_at_px(1000.0, 300.0, 3), 2); // past the end → last tab
    }

    #[test]
    fn tab_label_formats() {
        assert_eq!(tab_label("zsh", 0), "1: zsh");
        assert_eq!(tab_label("", 1), "2: shell"); // blank → "shell"
        assert_eq!(tab_label("a-very-long-tab-title-here", 0), "1: a-very-long-tab-ti…");
    }

    #[test]
    fn tab_active_index_after_close() {
        // 4 tabs [0,1,2,3], active=2 → 3 remain after a close.
        assert_eq!(active_after_close(2, 0, 3), 1); // closed before active → shift down
        assert_eq!(active_after_close(2, 2, 3), 2); // closed active (not last) → next shifts in
        assert_eq!(active_after_close(2, 3, 3), 2); // closed after active → unchanged
        assert_eq!(active_after_close(3, 3, 3), 2); // closed active==last → clamp to new last
    }

    #[test]
    fn cursor_style_and_blink_in_snapshot() {
        let (mut term, _r, _a) = proxy_term(10, 2);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"X");
        // Block draws by inverting the cell (not recorded); bar/underline are recorded.
        let block = build_snapshot(&term, &Theme::default(), CursorStyle::Block, true);
        assert!(block.cursor.is_none());
        let bar = build_snapshot(&term, &Theme::default(), CursorStyle::Bar, true);
        assert!(bar.cursor.is_some());
        // blink off (cursor_on=false) hides the cursor entirely.
        let off = build_snapshot(&term, &Theme::default(), CursorStyle::Underline, false);
        assert!(off.cursor.is_none());
    }

    #[test]
    fn osc8_hyperlink_tracked() {
        let (mut term, _r, _a) = proxy_term(20, 2);
        let mut parser: Processor = Processor::new();
        // OSC 8 ; ; https://example.com  <X>  OSC 8 ; ;  (close)
        parser.advance(&mut term, b"\x1b]8;;https://example.com\x1b\\X\x1b]8;;\x1b\\");
        let colors = term.renderable_content().colors;
        let cell = cell_vis(&term.grid()[Line(0)][Column(0)], colors, false, false, SELECTION_BG);
        assert!(cell.hyperlink, "cell should carry an OSC-8 hyperlink");
    }

    #[test]
    fn iterm_image_decodes_and_caps() {
        use base64::Engine;
        use std::io::Cursor;
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(2, 2, image::Rgba([9, 9, 9, 255])))
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
        let payload = format!("1337;File=inline=1:{b64}").into_bytes();
        let dec = parse_iterm_image(&payload).expect("decode");
        assert_eq!((dec.width, dec.height), (2, 2));
        assert_eq!(dec.rgba.len(), 2 * 2 * 4);
        // Wrong prefix / bad base64 → None.
        assert!(parse_iterm_image(b"9999;File=x:AAAA").is_none());
        assert!(parse_iterm_image(b"1337;File=inline=1:not base64!!").is_none());
    }

    #[test]
    fn image_scanner_extracts_osc1337() {
        let mut sc = ImageScanner::new();
        let p = sc.feed(b"\x1b]1337;File=inline=1:AAAA\x07"); // BEL-terminated
        assert_eq!(p.len(), 1);
        assert!(p[0].starts_with(b"1337;File="));
        // A non-1337 OSC (title) is ignored.
        assert!(ImageScanner::new().feed(b"\x1b]0;title\x07").is_empty());
        // Split across chunks, ST-terminated.
        let mut sc2 = ImageScanner::new();
        assert!(sc2.feed(b"\x1b]1337;File=x:AA").is_empty());
        assert_eq!(sc2.feed(b"AA\x1b\\").len(), 1);
    }

    #[test]
    fn config_hex_and_family_helpers() {
        assert_eq!(parse_hex("#ff8800"), Some([0xff, 0x88, 0x00]));
        assert_eq!(parse_hex("  #000102 "), Some([0, 1, 2]));
        assert!(parse_hex("ff8800").is_none()); // missing '#'
        assert!(parse_hex("#abc").is_none()); // wrong length
        assert_eq!(primary_family("MesloLGS NF, Hack, monospace"), "MesloLGS NF");
        assert_eq!(primary_family("\"JetBrains Mono\", monospace"), "JetBrains Mono");
    }

    #[test]
    fn config_colors_load_into_the_vt_table() {
        let mut cfg = sampa_config::Config::from_toml("").unwrap();
        cfg.colors.black = "#123456".into();
        cfg.colors.foreground = "#abcdef".into();
        let (mut term, _r, _a) = proxy_term(4, 2);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, &color_setup(&cfg.colors));
        let colors = term.renderable_content().colors;
        // resolve() now returns the config values (it consults the table first).
        assert_eq!(
            resolve(AnsiColor::Named(NamedColor::Black), colors, DEFAULT_FG, false),
            [0x12, 0x34, 0x56]
        );
        assert_eq!(
            resolve(AnsiColor::Named(NamedColor::Foreground), colors, DEFAULT_FG, false),
            [0xab, 0xcd, 0xef]
        );
    }

    #[test]
    fn safe_url_scheme_gate() {
        assert!(is_safe_url("https://example.com"));
        assert!(is_safe_url("http://x"));
        assert!(!is_safe_url("file:///etc/passwd"));
        assert!(!is_safe_url("javascript:alert(1)"));
        assert!(!is_safe_url("mailto:a@b.c"));
    }

    #[test]
    fn plain_url_detection() {
        let row = "see https://rust-lang.org/tools for more";
        let i = row.find("https").unwrap();
        assert_eq!(url_at(row, i + 3).as_deref(), Some("https://rust-lang.org/tools"));
        // Leading/trailing punctuation is stripped.
        assert_eq!(url_at("(https://x.io).", 5).as_deref(), Some("https://x.io"));
        // Non-URL token / whitespace → None.
        assert!(url_at("hello world", 2).is_none());
        assert!(url_at("a https://x.io", 1).is_none());
    }

    #[test]
    fn sgr_underline_and_strike_flags() {
        let (term,) = drive(b"\x1b[4mU\x1b[0m\x1b[9mS", 10);
        let colors = term.renderable_content().colors;
        let u = cell_vis(&term.grid()[Line(0)][Column(0)], colors, false, false, SELECTION_BG);
        let s = cell_vis(&term.grid()[Line(0)][Column(1)], colors, false, false, SELECTION_BG);
        assert!(u.underline && !u.strike, "cell 0 should be underlined only");
        assert!(s.strike && !s.underline, "cell 1 should be struck only");
    }

    // --- keyboard encoding (§8.1) ---
    fn named(n: NamedKey, shift: bool, alt: bool, ctrl: bool, app: bool) -> Vec<u8> {
        encode_key(&Key::Named(n), None, shift, alt, ctrl, app)
    }
    fn chr(s: &str, shift: bool, alt: bool, ctrl: bool) -> Vec<u8> {
        encode_key(&Key::Character(s.into()), None, shift, alt, ctrl, false)
    }

    #[test]
    fn arrows_respect_decckm_and_modifiers() {
        assert_eq!(named(NamedKey::ArrowUp, false, false, false, false), b"\x1b[A".to_vec());
        assert_eq!(named(NamedKey::ArrowUp, false, false, false, true), b"\x1bOA".to_vec());
        assert_eq!(named(NamedKey::ArrowUp, true, false, false, false), b"\x1b[1;2A".to_vec());
        // A modifier forces the CSI form even in application-cursor mode.
        assert_eq!(named(NamedKey::ArrowUp, false, false, true, true), b"\x1b[1;5A".to_vec());
    }

    #[test]
    fn function_and_editing_keys() {
        assert_eq!(named(NamedKey::F1, false, false, false, false), b"\x1bOP".to_vec());
        assert_eq!(named(NamedKey::F5, false, false, false, false), b"\x1b[15~".to_vec());
        assert_eq!(named(NamedKey::F5, true, false, false, false), b"\x1b[15;2~".to_vec());
        assert_eq!(named(NamedKey::Delete, false, false, false, false), b"\x1b[3~".to_vec());
        assert_eq!(named(NamedKey::Delete, false, false, true, false), b"\x1b[3;5~".to_vec());
        assert_eq!(named(NamedKey::Home, false, false, false, true), b"\x1bOH".to_vec());
    }

    #[test]
    fn control_meta_and_backtab() {
        assert_eq!(chr("a", false, false, true), vec![1]); // Ctrl-A
        assert_eq!(named(NamedKey::Space, false, false, true, false), vec![0]); // Ctrl-Space
        assert_eq!(chr("a", false, true, false), b"\x1ba".to_vec()); // Alt-a (Meta)
        assert_eq!(chr("a", false, false, false), b"a".to_vec());
        assert_eq!(named(NamedKey::Tab, true, false, false, false), b"\x1b[Z".to_vec()); // back-tab
    }

    #[test]
    fn decckm_mode_is_tracked() {
        // `ESC [ ? 1 h` enables application-cursor mode; window_event reads exactly this.
        let (term,) = drive(b"\x1b[?1h", 10);
        assert!(term.mode().contains(TermMode::APP_CURSOR));
    }

    // --- mouse reporting (§8.2) ---
    #[test]
    fn mouse_sgr_encoding() {
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(
            mouse_report(mode, 0, 0, 0, true, false, false, false, false).unwrap(),
            b"\x1b[<0;1;1M".to_vec()
        );
        assert_eq!(
            mouse_report(mode, 0, 0, 0, false, false, false, false, false).unwrap(),
            b"\x1b[<0;1;1m".to_vec()
        );
        // Ctrl+left press at col2/row3 → button 0+16, coords 3;4.
        assert_eq!(
            mouse_report(mode, 0, 2, 3, true, false, false, false, true).unwrap(),
            b"\x1b[<16;3;4M".to_vec()
        );
    }

    #[test]
    fn no_mouse_report_without_mode() {
        assert!(mouse_report(TermMode::NONE, 0, 0, 0, true, false, false, false, false).is_none());
    }

    #[test]
    fn mode_gates_selection_and_reporting() {
        // A DECCKM/mouse-mode escape from the app flips the flags the handlers read.
        let (term,) = drive(b"\x1b[?1000h\x1b[?1006h", 10);
        assert!(term.mode().contains(TermMode::MOUSE_REPORT_CLICK));
        assert!(term.mode().contains(TermMode::SGR_MOUSE));
    }

    // --- scrollback (§6.2) ---
    #[test]
    fn scrollback_display_offset_shifts_view() {
        let mut parser: Processor = Processor::new();
        let mut term = Term::new(TermConfig::default(), &TermSize::new(10, 3), VoidListener);
        for i in 0..8 {
            parser.advance(&mut term, format!("L{i}\r\n").as_bytes());
        }
        let bottom = build_snapshot(&term, &Theme::default(), CursorStyle::Block, true).to_text();
        assert!(bottom.contains("L7"), "bottom view: {bottom:?}");
        // Scroll up into history; the bottom line should no longer be the newest.
        term.scroll_display(Scroll::Delta(3));
        let scrolled = build_snapshot(&term, &Theme::default(), CursorStyle::Block, true).to_text();
        assert_ne!(bottom, scrolled, "view should change after scrolling up");
        assert!(scrolled.contains("L4"), "scrolled view: {scrolled:?}");
    }

    #[test]
    fn search_finds_matches_across_scrollback() {
        let mut parser: Processor = Processor::new();
        let mut term = Term::new(TermConfig::default(), &TermSize::new(20, 3), VoidListener);
        // "foo" appears on the first line (scrolled into history) and the last.
        parser.advance(&mut term, b"foo one\r\n");
        for i in 0..5 {
            parser.advance(&mut term, format!("line{i}\r\n").as_bytes());
        }
        parser.advance(&mut term, b"bar foo baz");

        let m = find_matches(&term, "foo", 100);
        assert_eq!(m.len(), 2, "should find both foo occurrences: {m:?}");
        // Matches come top-to-bottom: the scrollback one first (more negative line).
        assert!(m[0].start().line.0 < m[1].start().line.0);
        // The first "foo" starts at column 0 of its line.
        assert_eq!(m[0].start().column.0, 0);
        // Regex works too; a query with no match is empty; empty query is empty.
        assert_eq!(find_matches(&term, "line[0-9]", 100).len(), 5);
        assert!(find_matches(&term, "nope", 100).is_empty());
        assert!(find_matches(&term, "", 100).is_empty());
    }

    // --- selection (§8.3) ---
    #[test]
    fn selection_range_membership() {
        let range = SelectionRange::new(
            Point::new(Line(0), Column(2)),
            Point::new(Line(0), Column(5)),
            false,
        );
        assert!(in_selection(&range, 0, 2));
        assert!(in_selection(&range, 0, 4));
        assert!(in_selection(&range, 0, 5));
        assert!(!in_selection(&range, 0, 1));
        assert!(!in_selection(&range, 1, 3));
    }

    #[test]
    fn multi_click_counting_and_granularity() {
        assert_eq!(next_click_count(0, false, 0), 1); // first click
        assert_eq!(next_click_count(1, true, 100), 2); // double
        assert_eq!(next_click_count(2, true, 100), 3); // triple
        assert_eq!(next_click_count(3, true, 100), 1); // 4th wraps to char
        assert_eq!(next_click_count(1, false, 100), 1); // different cell restarts
        assert_eq!(next_click_count(1, true, 500), 1); // too slow restarts
        assert!(matches!(selection_type_for(1), SelectionType::Simple));
        assert!(matches!(selection_type_for(2), SelectionType::Semantic));
        assert!(matches!(selection_type_for(3), SelectionType::Lines));
    }

    #[test]
    fn semantic_selection_grabs_the_word() {
        let (mut term, _r, _a) = proxy_term(20, 2);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"hello world");
        // A double-click (word) selection anchored inside "hello".
        term.selection = Some(Selection::new(
            SelectionType::Semantic,
            Point::new(Line(0), Column(2)),
            Side::Left,
        ));
        assert_eq!(term.selection_to_string().as_deref(), Some("hello"));
    }

    // --- escape hardening (§13) ---
    #[test]
    fn title_is_sanitized() {
        assert_eq!(sanitize_title("hi\x07\x1b]bye\nend"), "hi]byeend");
        assert_eq!(sanitize_title(&"x".repeat(500)).chars().count(), 256);
    }

    // Build a Term wired to an EventProxy, returning the reply (PTY) and app (UI)
    // receivers so tests can observe both channels.
    fn proxy_term(
        cols: usize,
        rows: usize,
    ) -> (
        Term<EventProxy>,
        std::sync::mpsc::Receiver<Reply>,
        std::sync::mpsc::Receiver<AppEvent>,
    ) {
        use std::sync::mpsc::channel;
        let (reply_tx, reply_rx) = channel();
        let (app_tx, app_rx) = channel();
        let term = Term::new(
            TermConfig::default(),
            &TermSize::new(cols, rows),
            EventProxy { reply_tx, app_tx },
        );
        (term, reply_rx, app_rx)
    }

    #[test]
    fn da_query_routes_a_reply() {
        let (mut term, reply_rx, _app_rx) = proxy_term(10, 2);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"\x1b[c"); // Primary Device Attributes
        let replies: Vec<Vec<u8>> =
            reply_rx.try_iter().map(|r| resolve_reply(r, &term)).collect();
        assert!(
            replies.iter().any(|b| b.starts_with(b"\x1b[?")),
            "DA query produced no device-attributes reply"
        );
    }

    #[test]
    fn osc4_color_query_reflects_set_value() {
        let (mut term, reply_rx, _app_rx) = proxy_term(4, 2);
        let mut parser: Processor = Processor::new();
        // Set palette color 1 to pure red, then query it (OSC 4 ; 1 ; ?).
        parser.advance(&mut term, b"\x1b]4;1;rgb:ff/00/00\x1b\\\x1b]4;1;?\x1b\\");
        let out: Vec<u8> = reply_rx
            .try_iter()
            .flat_map(|r| resolve_reply(r, &term))
            .collect();
        let s = String::from_utf8_lossy(&out);
        assert!(
            s.contains("rgb:ffff/0000/0000"),
            "color query should report the app-set red, got {s:?}"
        );
    }

    #[test]
    fn osc52_write_surfaces_for_gating() {
        let (mut term, _reply_rx, app_rx) = proxy_term(10, 2);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"\x1b]52;c;aGk=\x07"); // OSC 52 set clipboard to "hi"
        let events: Vec<_> = app_rx.try_iter().collect();
        assert!(
            events.iter().any(|e| matches!(e, AppEvent::ClipboardStore(_))),
            "OSC 52 write should surface as a ClipboardStore (gated in drain, default-deny)"
        );
    }

    #[test]
    fn osc52_read_never_echoes_to_pty() {
        let (mut term, reply_rx, _app_rx) = proxy_term(10, 2);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"\x1b]52;c;?\x07"); // OSC 52 *read* request
        assert!(
            reply_rx.try_iter().next().is_none(),
            "OSC 52 read must not echo clipboard contents back to the PTY"
        );
    }

    #[test]
    fn decrqcra_scanner_detects_request() {
        let mut sc = DecrqcraScanner::new();
        let reqs = sc.feed(b"\x1b[1;0;1;1;1;2*y");
        assert_eq!(reqs.len(), 1);
        let (pos, ev) = &reqs[0];
        assert_eq!(*pos, 15); // whole sequence consumed
        let ScanEvent::Decrqcra(r) = ev else { panic!("expected DECRQCRA") };
        assert_eq!(r.pid, 1);
        assert_eq!((r.top, r.left, r.bottom, r.right), (Some(1), Some(1), Some(1), Some(2)));
        // A normal CSI (cursor move) is not mistaken for either sequence.
        assert!(DecrqcraScanner::new().feed(b"\x1b[1;2H").is_empty());
    }

    #[test]
    fn dcs_scanner_and_decrqss_replies() {
        // DECRQSS `DCS $q m ST` is extracted; other DCS is ignored.
        let mut sc = DcsScanner::new();
        assert_eq!(sc.feed(b"\x1bP$qm\x1b\\"), vec![b"m".to_vec()]);
        assert!(DcsScanner::new().feed(b"\x1bPnot-decrqss\x1b\\").is_empty());

        let (mut term, _r, _a) = proxy_term(10, 2);
        let mut parser: Processor = Processor::new();
        // SGR 1 (bold), then the pen reports 0;1; DECSCL is fixed.
        parser.advance(&mut term, b"\x1b[1m");
        assert_eq!(decrqss_reply(b"m", &term), b"\x1bP1$r0;1m\x1b\\".to_vec());
        assert_eq!(decrqss_reply(b"\"p", &term), b"\x1bP1$r64;1\"p\x1b\\".to_vec());
        assert_eq!(decrqss_reply(b"+q", &term), b"\x1bP1$r0+q\x1b\\".to_vec());
        // Unsupported query → invalid.
        assert_eq!(decrqss_reply(b"zz", &term), b"\x1bP0$r\x1b\\".to_vec());
    }

    #[test]
    fn decrqm_modifiable_reports_shadow_state() {
        use std::collections::HashMap;
        let mut shadow: HashMap<(bool, u16), bool> = HashMap::new();
        // Untracked / untoggled DEC mode 5 (DECSCNM) defaults to reset (2), overriding
        // alacritty's "not recognized" (0).
        assert_eq!(decrqm_modifiable(b"\x1b[?5;0$y", &shadow), Some(b"\x1b[?5;2$y".to_vec()));
        // After DECSET(5): set (1).
        shadow.insert((true, 5), true);
        assert_eq!(decrqm_modifiable(b"\x1b[?5;0$y", &shadow), Some(b"\x1b[?5;1$y".to_vec()));
        // ANSI KAM (mode 2) tracked in the ANSI namespace.
        shadow.insert((false, 2), true);
        assert_eq!(decrqm_modifiable(b"\x1b[2;0$y", &shadow), Some(b"\x1b[2;1$y".to_vec()));
        // A mode we don't shadow is left alone.
        assert_eq!(decrqm_modifiable(b"\x1b[?1;0$y", &shadow), None);
    }

    #[test]
    fn scanner_detects_mode_set() {
        // DECSET 5 (DECSCNM) → SetMode { dec, mode: 5, set: true }.
        let ev = DecrqcraScanner::new().feed(b"\x1b[?5h");
        assert!(matches!(ev[..], [(_, ScanEvent::SetMode { dec: true, mode: 5, set: true })]));
        // ANSI RM 2 (KAM) → reset.
        let ev = DecrqcraScanner::new().feed(b"\x1b[2l");
        assert!(matches!(ev[..], [(_, ScanEvent::SetMode { dec: false, mode: 2, set: false })]));
        // A mode we don't shadow emits nothing (left to the engine).
        assert!(DecrqcraScanner::new().feed(b"\x1b[?25h").is_empty());
    }

    #[test]
    fn scanner_detects_selective_erase() {
        // DECSED: CSI ? Ps J  → SelectiveErase { line: false }.
        let ev = DecrqcraScanner::new().feed(b"\x1b[?0J");
        assert!(matches!(ev[..], [(_, ScanEvent::SelectiveErase { line: false, ps: 0 })]));
        // DECSEL: CSI ? Ps K  → SelectiveErase { line: true }.
        let ev = DecrqcraScanner::new().feed(b"\x1b[?2K");
        assert!(matches!(ev[..], [(_, ScanEvent::SelectiveErase { line: true, ps: 2 })]));
        // Default (no param): CSI ? J → ps 0.
        let ev = DecrqcraScanner::new().feed(b"\x1b[?J");
        assert!(matches!(ev[..], [(_, ScanEvent::SelectiveErase { line: false, ps: 0 })]));
        // The NON-private CSI Ps J (plain ED) is alacritty's job — not our event.
        assert!(DecrqcraScanner::new().feed(b"\x1b[0J").is_empty());
    }

    #[test]
    fn scanner_detects_decstr() {
        let ev = DecrqcraScanner::new().feed(b"\x1b[!p");
        assert_eq!(ev.len(), 1);
        assert!(matches!(ev[0].1, ScanEvent::Decstr));
        // DECRQM (`CSI $ p`) must NOT be mistaken for DECSTR.
        assert!(DecrqcraScanner::new().feed(b"\x1b[4$p").is_empty());
    }

    #[test]
    fn scanner_detects_resize() {
        let ev = DecrqcraScanner::new().feed(b"\x1b[8;25;80t");
        assert_eq!(ev.len(), 1);
        assert!(matches!(
            ev[0].1,
            ScanEvent::Resize { rows: Some(25), cols: Some(80) }
        ));
        // CSI 18 t (text-area chars) is left to the engine, not re-emitted here.
        assert!(DecrqcraScanner::new().feed(b"\x1b[18t").is_empty());
    }

    #[test]
    fn scanner_resize_distinguishes_omitted_zero_and_value() {
        let ev = |s: &[u8]| {
            let out = DecrqcraScanner::new().feed(s);
            assert_eq!(out.len(), 1, "{:?}", std::str::from_utf8(s));
            match out[0].1 {
                ScanEvent::Resize { rows, cols } => (rows, cols),
                _ => panic!("expected Resize"),
            }
        };
        // Explicit value in both dimensions.
        assert_eq!(ev(b"\x1b[8;10;90t"), (Some(10), Some(90)));
        // Omitted width (`CSI 8;H t`) keeps columns; omitted height (`CSI 8;;W t`) keeps rows.
        assert_eq!(ev(b"\x1b[8;10t"), (Some(10), None));
        assert_eq!(ev(b"\x1b[8;;90t"), (None, Some(90)));
        // Explicit 0 maximizes to the display, distinct from omitted.
        assert_eq!(ev(b"\x1b[8;0;90t"), (Some(DISPLAY_ROWS), Some(90)));
        assert_eq!(ev(b"\x1b[8;10;0t"), (Some(10), Some(DISPLAY_COLS)));
        // DECSLPP: `CSI Ps t`, Ps ≥ 24 sets the line count and keeps columns.
        assert_eq!(ev(b"\x1b[30t"), (Some(30), None));
    }

    #[test]
    fn scanner_detects_pixel_resize() {
        let out = DecrqcraScanner::new().feed(b"\x1b[4;200;360t");
        assert_eq!(out.len(), 1);
        assert!(matches!(
            out[0].1,
            ScanEvent::ResizePixels { h: Some(200), w: Some(360) }
        ));
    }

    #[test]
    fn scanner_detects_winop_reports() {
        for &op in &[11u16, 13, 14, 15, 16, 19] {
            let out = DecrqcraScanner::new().feed(format!("\x1b[{op}t").as_bytes());
            assert_eq!(out.len(), 1, "op {op}");
            assert!(matches!(out[0].1, ScanEvent::WinopReport(o) if o == op), "op {op}");
        }
        // The `CSI 14;2 t` (shell-window) variant still reports as op 14.
        let out = DecrqcraScanner::new().feed(b"\x1b[14;2t");
        assert!(matches!(out[0].1, ScanEvent::WinopReport(14)));
    }

    #[test]
    fn scanner_translates_save_restore_cursor_mode() {
        // ?1048 set/reset → DECSC/DECRC translation events.
        let save = DecrqcraScanner::new().feed(b"\x1b[?1048h");
        assert_eq!(save.len(), 1);
        assert!(matches!(save[0].1, ScanEvent::SaveRestoreCursor { save: true }));
        let restore = DecrqcraScanner::new().feed(b"\x1b[?1048l");
        assert!(matches!(restore[0].1, ScanEvent::SaveRestoreCursor { save: false }));
        // Other private modes (?1049 alt-screen, ?25 cursor) are left to the engine.
        assert!(DecrqcraScanner::new().feed(b"\x1b[?1049h").is_empty());
        assert!(DecrqcraScanner::new().feed(b"\x1b[?25l").is_empty());
        // A non-private 1048 (`CSI 1048 h`) is not the private mode and is ignored.
        assert!(DecrqcraScanner::new().feed(b"\x1b[1048h").is_empty());
    }

    #[test]
    fn scanner_detects_decdsr_and_replies() {
        // `CSI ? 6 n` (DECXCPR) is detected as a private DSR carrying its Ps.
        let out = DecrqcraScanner::new().feed(b"\x1b[?6n");
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0].1, ScanEvent::Decdsr { ps: 6, pid: None }));
        // `CSI ? 63 ; 123 n` (DECCKSR) carries the Pid.
        let out = DecrqcraScanner::new().feed(b"\x1b[?63;123n");
        assert!(matches!(out[0].1, ScanEvent::Decdsr { ps: 63, pid: Some(123) }));
        // A non-private DSR (`CSI 6 n`, CPR) is left to the engine.
        assert!(DecrqcraScanner::new().feed(b"\x1b[6n").is_empty());

        // Reply values: DECXCPR reports the live cursor without a page (VT level 2);
        // the rest are the fixed legal "feature absent" reports esctest accepts.
        assert_eq!(decdsr_reply(6, None, 6, 5), Some(b"\x1b[?6;5R".to_vec()));
        assert_eq!(decdsr_reply(15, None, 1, 1), Some(b"\x1b[?13n".to_vec()));
        assert_eq!(decdsr_reply(25, None, 1, 1), Some(b"\x1b[?20n".to_vec()));
        assert_eq!(decdsr_reply(26, None, 1, 1), Some(b"\x1b[?27;1n".to_vec()));
        assert_eq!(decdsr_reply(55, None, 1, 1), Some(b"\x1b[?50n".to_vec()));
        assert_eq!(decdsr_reply(56, None, 1, 1), Some(b"\x1b[?57;0n".to_vec()));
        assert_eq!(decdsr_reply(62, None, 1, 1), Some(b"\x1b[0*{".to_vec()));
        assert_eq!(decdsr_reply(63, Some(123), 1, 1), Some(b"\x1bP123!~0000\x1b\\".to_vec()));
        assert_eq!(decdsr_reply(75, None, 1, 1), Some(b"\x1b[?70n".to_vec()));
        assert_eq!(decdsr_reply(85, None, 1, 1), Some(b"\x1b[?83n".to_vec()));
        // Unknown Ps → no reply.
        assert_eq!(decdsr_reply(99, None, 1, 1), None);
    }

    #[test]
    fn decrqm_permanently_reset_modes_rewrite_zero_to_four() {
        // ANSI GATM (1) and DEC DECHCCM (?60): 0 (not recognized) → 4 (perm reset).
        assert_eq!(decrqm_perm_reset(b"\x1b[1;0$y"), Some(b"\x1b[1;4$y".to_vec()));
        assert_eq!(decrqm_perm_reset(b"\x1b[?60;0$y"), Some(b"\x1b[?60;4$y".to_vec()));
        // A non-zero state (already answered) is left as-is.
        assert_eq!(decrqm_perm_reset(b"\x1b[1;2$y"), None);
        // Modes not in the list pass through: ANSI IRM (4, engine-supported), DEC
        // cursor-keys (?1, distinct namespace from ANSI GATM), modifiable SRM (12).
        assert_eq!(decrqm_perm_reset(b"\x1b[4;1$y"), None);
        assert_eq!(decrqm_perm_reset(b"\x1b[?1;0$y"), None);
        assert_eq!(decrqm_perm_reset(b"\x1b[12;0$y"), None);
        // Non-DECRQM replies (DA, CPR) are untouched.
        assert_eq!(decrqm_perm_reset(b"\x1b[?62;1;6c"), None);
        assert_eq!(decrqm_perm_reset(b"\x1b[3;6R"), None);
    }

    #[test]
    fn winop_report_formats_match_xterm() {
        // Text-area px == chars × cell px, so 14/16/18 stay mutually consistent.
        assert_eq!(winop_report(11, 80, 24), b"\x1b[1t".to_vec());
        assert_eq!(winop_report(13, 80, 24), b"\x1b[3;0;0t".to_vec());
        assert_eq!(
            winop_report(14, 80, 24),
            format!("\x1b[4;{};{}t", 24 * CELL_H_PX, 80 * CELL_W_PX).into_bytes()
        );
        assert_eq!(
            winop_report(16, 80, 24),
            format!("\x1b[6;{};{}t", CELL_H_PX, CELL_W_PX).into_bytes()
        );
        assert_eq!(
            winop_report(19, 80, 24),
            format!("\x1b[9;{};{}t", DISPLAY_ROWS, DISPLAY_COLS).into_bytes()
        );
    }

    #[test]
    fn decrqcra_checksum_matches_sum() {
        let (mut term, _reply_rx, _app_rx) = proxy_term(4, 2);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"AB");
        // 'A'(0x41) + 'B'(0x42) = 0x83 over the 1×2 rectangle at the top-left.
        let req = Decrqcra { pid: 1, top: Some(1), left: Some(1), bottom: Some(1), right: Some(2) };
        assert_eq!(compute_decrqcra(&term, &req), b"\x1bP1!~0083\x1b\\".to_vec());
        // Empty cells count as space (0x20): a fresh 1×2 area on row 2 = 0x40.
        let empty = Decrqcra { pid: 0, top: Some(2), left: Some(1), bottom: Some(2), right: Some(2) };
        assert_eq!(compute_decrqcra(&term, &empty), b"\x1bP0!~0040\x1b\\".to_vec());
    }

    // --- app-matrix smoke (§17) ---
    // Drives real programs through a real PTY into the VT engine (no GPU) and checks
    // the rendered grid. Ignored by default (spawns processes, ~seconds); run with:
    //   cargo test -- --ignored app_matrix_smoke
    #[test]
    #[ignore = "spawns real programs; run with --ignored"]
    fn app_matrix_smoke() {
        use std::sync::mpsc::{channel, RecvTimeoutError};
        use std::time::{Duration, Instant};

        fn have(cmd: &str) -> bool {
            std::env::var("PATH").unwrap_or_default().split(':').any(|d| {
                std::path::Path::new(d).join(cmd).exists()
            })
        }

        // Spawn `cmd args` on a PTY, feed `input` after startup, pump output into a
        // Term for up to `ms`, and return the rendered grid text.
        fn grid_after(cmd: &str, args: &[&str], input: &[u8], ms: u64) -> String {
            let (tx, rx) = channel();
            let mut pty = spawn(
                SpawnConfig {
                    shell: cmd.to_string(),
                    args: args.iter().map(|s| s.to_string()).collect(),
                    cwd: None,
                    cols: 80,
                    rows: 24,
                    env: vec![("TERM".into(), "xterm-256color".into())],
                },
                tx,
            )
            .expect("spawn");
            let mut parser: Processor = Processor::new();
            let mut term =
                Term::new(TermConfig::default(), &TermSize::new(80, 24), VoidListener);
            let start = Instant::now();
            let mut fed = input.is_empty();
            loop {
                match rx.recv_timeout(Duration::from_millis(40)) {
                    Ok(PtyEvent::Output(b)) => parser.advance(&mut term, &b),
                    Ok(PtyEvent::Exit(_)) => break,
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                if !fed && start.elapsed() >= Duration::from_millis(300) {
                    let _ = pty.write(input);
                    fed = true;
                }
                if start.elapsed() >= Duration::from_millis(ms) {
                    break;
                }
            }
            let _ = pty.kill();
            build_snapshot(&term, &Theme::default(), CursorStyle::Block, true).to_text()
        }

        // (name, cmd, args, input, wait_ms, any-of expected markers)
        let cases: &[(&str, &str, &[&str], &[u8], u64, &[&str])] = &[
            ("echo", "sh", &["-c", "echo hello-matrix"], b"", 600, &["hello-matrix"]),
            ("ls", "ls", &["--color=always", "/"], b"", 800, &["usr", "bin", "etc"]),
            ("seq/wrap", "seq", &["1", "60"], b"", 800, &["60"]),
            ("python", "python3", &["-c", "print('py-ok-42')"], b"", 900, &["py-ok-42"]),
            ("vim/alt-screen", "vim", &["-u", "NONE", "-N"], b"", 1100, &["~"]),
            ("htop", "htop", &[], b"", 1300, &["CPU", "Mem", "Load", "PID", "Tasks"]),
        ];

        let mut failed = Vec::new();
        for (name, cmd, args, input, ms, expect) in cases {
            if !have(cmd) {
                println!("[app-matrix] {name:<16} SKIP (‘{cmd}’ not installed)");
                continue;
            }
            let grid = grid_after(cmd, args, input, *ms);
            let ok = expect.iter().any(|m| grid.contains(m));
            let first = grid.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
            println!(
                "[app-matrix] {name:<16} {}   first-line: {:?}",
                if ok { "PASS" } else { "FAIL" },
                &first[..first.len().min(60)]
            );
            if !ok {
                failed.push(*name);
            }
        }
        assert!(failed.is_empty(), "app-matrix cases rendered no expected marker: {failed:?}");
    }

    // The "real terminal" signal contract (§3.2): the kernel line discipline turns a
    // typed ^C into SIGINT for the foreground process group. We only write bytes.
    #[test]
    #[ignore = "spawns a real process; run with --ignored"]
    fn ctrl_c_sends_sigint() {
        use std::sync::mpsc::{channel, RecvTimeoutError};
        use std::time::{Duration, Instant};

        let (tx, rx) = channel();
        let mut pty = spawn(
            SpawnConfig {
                shell: "sleep".into(),
                args: vec!["30".into()],
                cwd: None,
                cols: 80,
                rows: 24,
                env: vec![],
            },
            tx,
        )
        .expect("spawn");

        std::thread::sleep(Duration::from_millis(300));
        pty.write(b"\x03").expect("write ^C"); // → line discipline → SIGINT

        let mut exited = false;
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(PtyEvent::Exit(_)) => {
                    exited = true;
                    break;
                }
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        let _ = pty.kill();
        assert!(exited, "^C did not terminate `sleep` — SIGINT was not delivered");
    }

    // The resize contract (§3.3): `pty.resize` sets the slave winsize (TIOCSWINSZ),
    // the kernel delivers SIGWINCH, and the child observes the new dimensions.
    #[test]
    #[ignore = "spawns a real process; run with --ignored"]
    fn resize_reaches_child() {
        use std::sync::mpsc::{channel, RecvTimeoutError};
        use std::time::{Duration, Instant};

        let (tx, rx) = channel();
        let mut pty = spawn(
            SpawnConfig {
                shell: "sh".into(),
                args: vec!["-c".into(), "while true; do stty size; sleep 0.2; done".into()],
                cwd: None,
                cols: 80,
                rows: 24,
                env: vec![("TERM".into(), "xterm-256color".into())],
            },
            tx,
        )
        .expect("spawn");

        std::thread::sleep(Duration::from_millis(400));
        pty.resize(100, 30, 0, 0).expect("resize"); // cols=100, rows=30

        let mut out = String::new();
        let deadline = Instant::now() + Duration::from_millis(1400);
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(PtyEvent::Output(b)) => out.push_str(&String::from_utf8_lossy(&b)),
                Ok(PtyEvent::Exit(_)) => break,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        let _ = pty.kill();
        assert!(out.contains("24 80"), "expected initial 24x80; got {out:?}");
        assert!(out.contains("30 100"), "child never saw the resize; got {out:?}");
    }
}
