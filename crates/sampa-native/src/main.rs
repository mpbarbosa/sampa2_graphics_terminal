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
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionRange, SelectionType};
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
use wgpu::util::DeviceExt;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

// --- Fixed metrics (N1: single monospace font) -------------------------------
const FONT_SIZE: f32 = 15.0;
const LINE_HEIGHT: f32 = 18.0;
const PAD: f32 = 6.0;

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
                    0x40..=0x7e => {
                        self.push_param(); // finalize the trailing parameter
                        if !self.bad {
                            if b == b'y' && self.star && !self.bang {
                                // DECRQCRA: CSI … * y
                                out.push((i + 1, ScanEvent::Decrqcra(self.request())));
                            } else if b == b'p' && self.bang && !self.star {
                                // DECSTR: CSI ! p
                                out.push((i + 1, ScanEvent::Decstr));
                            } else if b == b't' && !self.star && !self.bang {
                                // XTWINOPS (`CSI … t`): resize / DECSLPP / size reports.
                                if let Some(ev) = self.winop() {
                                    out.push((i + 1, ev));
                                }
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

/// One decoded image placed on the grid. `anchor` is an absolute grid line at insert
/// time; the renderer draws it at `anchor - display_offset`. `id` keys the GPU texture.
struct PlacedImage {
    id: u64,
    anchor: i32,
    col: usize,
    width: u32,
    height: u32,
    rgba: Option<Vec<u8>>, // taken by the renderer on first upload
}

/// Live inline images, shared between the parser thread (adds) and the renderer
/// (uploads + composites). Capped at `MAX_IMAGES`, oldest evicted.
#[derive(Default)]
struct ImageStore {
    images: Vec<PlacedImage>,
    next_id: u64,
}

impl ImageStore {
    fn add(&mut self, anchor: i32, col: usize, img: DecodedImage) {
        let id = self.next_id;
        self.next_id += 1;
        self.images.push(PlacedImage {
            id,
            anchor,
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
    dcs: DcsScanner,
}

#[derive(Debug, Clone)]
enum UserEvent {
    Redraw,
    Exit(String),
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

    let (cols, rows) = (80u16, 24u16);
    let (app_tx, app_rx) = channel();
    let (reply_tx, reply_rx) = channel::<Reply>();
    let state = Arc::new(Mutex::new(TermState {
        parser: Processor::new(),
        term: Term::new(
            TermConfig::default(),
            &TermSize::new(cols as usize, rows as usize),
            EventProxy { reply_tx, app_tx },
        ),
        decrqcra: DecrqcraScanner::new(),
        image_scanner: ImageScanner::new(),
        dcs: DcsScanner::new(),
    }));
    let images = Arc::new(Mutex::new(ImageStore::default()));

    let (tx, rx) = channel();
    let pty = Arc::new(Mutex::new(spawn(
        SpawnConfig {
            shell,
            args: shell_args,
            cwd,
            cols,
            rows,
            env: vec![],
        },
        tx,
    )?));

    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    thread::spawn({
        let (state, pty, images) = (Arc::clone(&state), Arc::clone(&pty), Arc::clone(&images));
        move || pump(rx, state, proxy, reply_rx, pty, images)
    });

    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App {
        state,
        pty,
        cols,
        rows,
        window: None,
        gfx: None,
        modifiers: ModifiersState::empty(),
        dumped: false,
        mouse_col: 0,
        mouse_row: 0,
        left_down: false,
        clipboard: arboard::Clipboard::new().ok(),
        app_rx,
        osc52_allow: std::env::var("SAMPA_OSC52").map(|v| v == "allow").unwrap_or(false),
        title: win_title,
        images,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}

fn pump(
    rx: Receiver<PtyEvent>,
    state: Arc<Mutex<TermState>>,
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    reply_rx: Receiver<Reply>,
    pty: Arc<Mutex<PtyHandle>>,
    image_store: Arc<Mutex<ImageStore>>,
) {
    for ev in rx {
        match ev {
            PtyEvent::Output(bytes) => {
                // Collect every reply this chunk produces, in stream order, then write
                // them back to the PTY synchronously (a query must be answered before
                // the next output byte is processed — that's what apps block on).
                let mut replies: Vec<Vec<u8>> = Vec::new();
                let mut pty_resize: Option<(u16, u16)> = None;
                let mut image_adds: Vec<(i32, usize, DecodedImage)> = Vec::new();
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
                            let rows = ((img.height as f32 / LINE_HEIGHT).ceil() as usize).max(1);
                            g.parser.advance(&mut g.term, "\r\n".repeat(rows).as_bytes());
                            image_adds.push((anchor, col, img));
                        }
                    }
                }
                if !image_adds.is_empty() {
                    if let Ok(mut store) = image_store.lock() {
                        for (anchor, col, img) in image_adds {
                            store.add(anchor, col, img);
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
                let _ = proxy.send_event(UserEvent::Exit(info.detail));
                break;
            }
        }
    }
}

struct App {
    state: Arc<Mutex<TermState>>,
    pty: Arc<Mutex<PtyHandle>>,
    cols: u16,
    rows: u16,
    window: Option<Arc<Window>>,
    gfx: Option<Gfx>,
    modifiers: ModifiersState,
    dumped: bool,
    // mouse / selection
    mouse_col: usize,
    mouse_row: usize,
    left_down: bool,
    clipboard: Option<arboard::Clipboard>,
    app_rx: Receiver<AppEvent>,
    osc52_allow: bool,
    title: String,
    images: Arc<Mutex<ImageStore>>,
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes().with_title(&self.title);
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        let gfx = pollster::block_on(Gfx::new(Arc::clone(&window), Arc::clone(&self.images)));
        self.window = Some(window);
        self.gfx = Some(gfx);
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
            UserEvent::Exit(detail) => {
                if let Some(w) = &self.window {
                    w.set_title(&format!("Sampa (native) — [{detail}]"));
                }
                self.request_redraw();
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                if let Ok(mut p) = self.pty.lock() { let _ = p.kill(); }
                event_loop.exit();
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::Resized(size) => {
                self.resize(size.width.max(1), size.height.max(1));
                self.request_redraw();
            }
            WindowEvent::RedrawRequested => self.render_now(),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let m = self.modifiers;
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
                // App keybindings (Ctrl+Shift+C/V) never reach the PTY.
                if m.control_key() && m.shift_key() {
                    if let Key::Character(s) = &event.logical_key {
                        match s.to_lowercase().as_str() {
                            "c" => {
                                self.copy_selection();
                                return;
                            }
                            "v" => {
                                self.paste_clipboard();
                                return;
                            }
                            _ => {}
                        }
                    }
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
                    self.pty_write(&bytes);
                    self.scroll(Scroll::Bottom); // typing snaps to the live prompt
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

impl App {
    fn cell_metrics(&self) -> (f32, f32) {
        self.gfx
            .as_ref()
            .map(|g| (g.r.cell_w, g.r.line_h))
            .unwrap_or((FONT_SIZE * 0.6, LINE_HEIGHT))
    }

    fn cell_at(&self, x: f64, y: f64) -> (usize, usize, Side) {
        let (cw, lh) = self.cell_metrics();
        let fx = (x as f32 - PAD) / cw;
        let col = (fx.max(0.0).floor() as usize).min(self.cols.saturating_sub(1) as usize);
        let row = (((y as f32 - PAD) / lh).max(0.0).floor() as usize)
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
                if let Ok(mut g) = self.state.lock() {
                    let d = g.term.grid().display_offset() as i32;
                    g.term.selection = Some(Selection::new(
                        SelectionType::Simple,
                        Point::new(Line(row as i32 - d), Column(col)),
                        Side::Left,
                    ));
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
        let cols = (((w as f32 - 2.0 * PAD) / cell_w).floor() as u16).max(1);
        let rows = (((h as f32 - 2.0 * PAD) / line_h).floor() as u16).max(1);
        if let Some(gfx) = &mut self.gfx {
            gfx.resize(w, h);
        }
        if cols != self.cols || rows != self.rows {
            self.cols = cols;
            self.rows = rows;
            if let Ok(mut g) = self.state.lock() {
                g.term.resize(TermSize::new(cols as usize, rows as usize));
            }
            if let Ok(p) = self.pty.lock() { let _ = p.resize(cols, rows, w as u16, h as u16); }
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

    /// Apply VT-raised side effects on the main thread (§13): route query replies to
    /// the PTY, sanitize + set the window title, and gate OSC-52 clipboard writes.
    fn drain_app_events(&mut self) {
        while let Ok(ev) = self.app_rx.try_recv() {
            match ev {
                AppEvent::Title(s) => {
                    if let Some(w) = &self.window {
                        w.set_title(&sanitize_title(&s));
                    }
                }
                AppEvent::ClipboardStore(s) => {
                    // OSC-52 write gate: denied by default (SAMPA_OSC52=allow to permit).
                    if self.osc52_allow {
                        if let Some(clip) = self.clipboard.as_mut() {
                            let _ = clip.set_text(s);
                        }
                    }
                }
                AppEvent::Bell => {}
            }
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

    fn render_now(&mut self) {
        let snap = match self.state.lock() {
            Ok(g) => build_snapshot(&g.term),
            Err(_) => return,
        };
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
        if let Some(gfx) = &mut self.gfx {
            gfx.render(&snap);
        }
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

/// Extract the visible grid + attributes + cursor into a `Snapshot`.
fn build_snapshot<L: EventListener>(term: &Term<L>) -> Snapshot {
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
    let cursor_abs = (!matches!(cursor.shape, CursorShape::Hidden))
        .then_some((cursor.point.line.0, cursor.point.column.0));

    let mut cells = Vec::with_capacity(rows * cols);
    for r in 0..rows {
        let abs = r as i32 - offset;
        for c in 0..cols {
            let selected = selection
                .as_ref()
                .is_some_and(|range| in_selection(range, abs, c));
            cells.push(cell_vis(
                &grid[Line(abs)][Column(c)],
                colors,
                cursor_abs == Some((abs, c)),
                selected,
            ));
        }
    }
    Snapshot { cols, rows, offset, cells }
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

/// Resolve one grid cell (color + attributes + cursor inversion) for display.
fn cell_vis(
    cell: &alacritty_terminal::term::cell::Cell,
    colors: &Colors,
    is_cursor: bool,
    selected: bool,
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
        bg = SELECTION_BG;
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
    quad_pipeline: wgpu::RenderPipeline,
    quad_uniform: wgpu::Buffer,
    quad_bind_group: wgpu::BindGroup,
    // inline images
    image_pipeline: wgpu::RenderPipeline,
    image_bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    image_textures: HashMap<u64, (wgpu::Texture, wgpu::BindGroup)>,
    images: Arc<Mutex<ImageStore>>,
}

impl Renderer {
    fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        format: wgpu::TextureFormat,
        images: Arc<Mutex<ImageStore>>,
    ) -> Self {
        let srgb = format.is_srgb();
        let mut font_system = FontSystem::new();
        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);

        // Measure the monospace advance so the quad grid lines up with the glyphs.
        let cell_w = {
            let mut probe = Buffer::new(&mut font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));
            probe.set_size(Some(4096.0), Some(LINE_HEIGHT));
            probe.set_text(
                "MMMMMMMMMMMMMMMMMMMM",
                &Attrs::new().family(Family::Monospace),
                Shaping::Advanced,
                None,
            );
            probe.shape_until_scroll(&mut font_system, false);
            probe
                .layout_runs()
                .next()
                .map(|r| r.line_w / 20.0)
                .filter(|w| *w > 0.1)
                .unwrap_or(FONT_SIZE * 0.6)
        };
        let buffer = Buffer::new(&mut font_system, Metrics::new(FONT_SIZE, LINE_HEIGHT));

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
            line_h: LINE_HEIGHT,
            font_system,
            swash_cache,
            viewport,
            atlas,
            text_renderer,
            buffer,
            quad_pipeline,
            quad_uniform,
            quad_bind_group,
            image_pipeline,
            image_bgl,
            sampler,
            image_textures: HashMap::new(),
            images,
        }
    }

    /// Upload any newly-added images to GPU textures and drop textures for evicted
    /// images. Returns the per-image draw rects (in pixels) for the current frame.
    fn sync_images(&mut self, offset: i32, w: u32, h: u32) -> Vec<(u64, [f32; 4])> {
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
            let y = PAD + (img.anchor + offset) as f32 * self.line_h;
            if x < w as f32 && y < h as f32 && y + img.height as f32 > 0.0 {
                rects.push((img.id, [x, y, img.width as f32, img.height as f32]));
            }
        }
        rects
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
    fn paint(&mut self, snap: &Snapshot, view: &wgpu::TextureView, w: u32, h: u32) {
        // Background/cursor quads (drawn under the text) and decoration quads
        // (underline/strikethrough, drawn over it).
        let mut bg_quads: Vec<QuadInstance> = Vec::new();
        let mut deco_quads: Vec<QuadInstance> = Vec::new();
        for r in 0..snap.rows {
            for c in 0..snap.cols {
                let cell = snap.cell(r, c);
                let x = PAD + c as f32 * self.cell_w;
                let y = PAD + r as f32 * self.line_h;
                if cell.bg != DEFAULT_BG {
                    bg_quads.push(QuadInstance {
                        rect: [x, y, self.cell_w + 0.5, self.line_h],
                        color: self.color4(cell.bg),
                    });
                }
                if cell.underline || cell.hyperlink {
                    deco_quads.push(QuadInstance {
                        rect: [x, y + self.line_h - 2.0, self.cell_w, 1.5],
                        color: self.color4(cell.fg),
                    });
                }
                if cell.strike {
                    deco_quads.push(QuadInstance {
                        rect: [x, y + self.line_h * 0.45, self.cell_w, 1.5],
                        color: self.color4(cell.fg),
                    });
                }
            }
        }

        // Foreground text as per-cell colored rich-text spans.
        let base = Attrs::new().family(Family::Monospace);
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
            .set_size(Some(w as f32 - 2.0 * PAD), Some(h as f32 - 2.0 * PAD));
        self.buffer.set_rich_text(
            spans.iter().map(|(s, fg, bold, italic)| {
                let mut a = Attrs::new()
                    .family(Family::Monospace)
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
            Shaping::Advanced,
            None,
        );
        self.buffer.shape_until_scroll(&mut self.font_system, false);

        self.viewport
            .update(&self.queue, Resolution { width: w, height: h });
        let text_area = TextArea {
            buffer: &self.buffer,
            left: PAD,
            top: PAD,
            scale: 1.0,
            bounds: TextBounds { left: 0, top: 0, right: w as i32, bottom: h as i32 },
            default_color: Color::rgb(DEFAULT_FG[0], DEFAULT_FG[1], DEFAULT_FG[2]),
            custom_glyphs: &[],
        };
        if let Err(e) = self.text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            [text_area],
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
        let image_rects = self.sync_images(snap.offset, w, h);
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

        let bg = self.color4(DEFAULT_BG);
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
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: bg[0] as f64,
                            g: bg[1] as f64,
                            b: bg[2] as f64,
                            a: 1.0,
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
            // Inline images composite on top.
            if !image_bufs.is_empty() {
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
    async fn new(window: Arc<Window>, images: Arc<Mutex<ImageStore>>) -> Self {
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
        let config = surface
            .get_default_config(&adapter, w, h)
            .expect("surface default config");
        let format = config.format;
        surface.configure(&device, &config);
        Gfx { surface, config, r: Renderer::new(device, queue, format, images) }
    }

    fn resize(&mut self, w: u32, h: u32) {
        self.config.width = w;
        self.config.height = h;
        self.surface.configure(&self.r.device, &self.config);
    }

    fn render(&mut self, snap: &Snapshot) {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            _ => return,
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.r.paint(snap, &view, self.config.width, self.config.height);
        self.r.queue.present(frame);
    }
}

/// Offscreen render of a color demo to a PNG. Proves the color/cursor pipeline
/// visually without a display server, and doubles as a CI screenshot test.
fn capture(path: &str) -> Result<()> {
    const DEMO: &[u8] = b"\x1b[31mRED \x1b[32mGREEN \x1b[33mYELLOW \x1b[34mBLUE \x1b[35mMAGENTA \x1b[36mCYAN\x1b[0m\r\n\x1b[1;91mBOLD-BRIGHT-RED\x1b[0m  \x1b[7mINVERSE\x1b[0m  \x1b[2mDIM\x1b[0m\r\n\x1b[38;2;255;140;0mTRUECOLOR-ORANGE\x1b[0m  \x1b[44;97m white-on-blue \x1b[0m\r\n\x1b[4mUNDERLINE\x1b[0m  \x1b[9mSTRIKETHROUGH\x1b[0m  \x1b]8;;https://example.com\x1b\\OSC8-LINK\x1b]8;;\x1b\\\r\nSEAM_OK color demo\r\n";
    let (cols, rows) = (64usize, 8usize);

    let mut parser: Processor = Processor::new();
    let mut term = Term::new(TermConfig::default(), &TermSize::new(cols, rows), VoidListener);
    parser.advance(&mut term, DEMO);
    // Demonstrate the selection highlight: select "RED GREEN YELLOW" on row 0.
    let mut sel = Selection::new(
        SelectionType::Simple,
        Point::new(Line(0), Column(0)),
        Side::Left,
    );
    sel.update(Point::new(Line(0), Column(15)), Side::Right);
    term.selection = Some(sel);
    let snap = build_snapshot(&term);

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
            .add(6, 2, DecodedImage { width: iw, height: ih, rgba });
    }
    let mut r = Renderer::new(device, queue, format, images);

    let w = (PAD * 2.0 + cols as f32 * r.cell_w).ceil() as u32;
    let h = (PAD * 2.0 + rows as f32 * r.line_h).ceil() as u32;

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
    r.paint(&snap, &view, w, h);

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
        let cell = cell_vis(&term.grid()[Line(0)][Column(0)], term.renderable_content().colors, false, false);
        assert_eq!(cell.fg, DEFAULT_BG, "inverse fg should be default bg");
        assert_eq!(cell.bg, DEFAULT_FG, "inverse bg should be default fg");
    }

    #[test]
    fn cursor_cell_is_inverted() {
        let (term,) = drive(b"X", 10);
        let colors = term.renderable_content().colors;
        let plain = cell_vis(&term.grid()[Line(0)][Column(0)], colors, false, false);
        let curs = cell_vis(&term.grid()[Line(0)][Column(0)], colors, true, false);
        assert_eq!(curs.fg, plain.bg);
        assert_eq!(curs.bg, plain.fg);
    }

    #[test]
    fn osc8_hyperlink_tracked() {
        let (mut term, _r, _a) = proxy_term(20, 2);
        let mut parser: Processor = Processor::new();
        // OSC 8 ; ; https://example.com  <X>  OSC 8 ; ;  (close)
        parser.advance(&mut term, b"\x1b]8;;https://example.com\x1b\\X\x1b]8;;\x1b\\");
        let colors = term.renderable_content().colors;
        let cell = cell_vis(&term.grid()[Line(0)][Column(0)], colors, false, false);
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
        let u = cell_vis(&term.grid()[Line(0)][Column(0)], colors, false, false);
        let s = cell_vis(&term.grid()[Line(0)][Column(1)], colors, false, false);
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
        let bottom = build_snapshot(&term).to_text();
        assert!(bottom.contains("L7"), "bottom view: {bottom:?}");
        // Scroll up into history; the bottom line should no longer be the newest.
        term.scroll_display(Scroll::Delta(3));
        let scrolled = build_snapshot(&term).to_text();
        assert_ne!(bottom, scrolled, "view should change after scrolling up");
        assert!(scrolled.contains("L4"), "scrolled view: {scrolled:?}");
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
            build_snapshot(&term).to_text()
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
