# Spec — Keyboard-shortcut help overlay

> **Provenance.** Copied from the reference
> [`sampa_graphics_terminal`](https://github.com/mpbarbosa/sampa_graphics_terminal)
> build (implemented there in `src/main.ts` + `index.html` + `src/style.css`). It is a
> **target for this Rust build** (`crates/sampa-native`). Note this repo's actual
> keybindings may differ from the reference's (e.g. preview is `Ctrl+Shift+E` here vs
> `Ctrl+Shift+R` there) — the §3a table lists the **actions** to include; bind and
> label them to *this* build's real keys, driven from its own config.

- **Status:** to implement — the native build has the individual shortcuts (tabs,
  search, palette, man, preview) but not yet a single overlay that lists them.
- **Applies to:** a `Ctrl+Shift+?` (or chosen) help overlay. This spec is the
  language-agnostic behavioral contract so any frontend (webview or native Rust)
  behaves identically.

## 1. Purpose

Give the user a discoverable, always-accurate list of the terminal's keyboard
shortcuts, opened from the keyboard itself. "Always-accurate" is the key requirement:
the list is generated from the **live keybinding config**, never hardcoded, so it can
never drift from what the keys actually do.

## 2. Trigger & lifecycle

- Bound to a configurable keybinding `keybindings.help`, **default `Ctrl+Shift+Slash`**
  (i.e. `Ctrl+Shift+?` on a US layout — `?` is Shift+`/`, physical code `Slash`).
- The binding **toggles** the overlay (open if closed, close if open).
- The overlay also closes on: **Esc**, a click on its **backdrop** (outside the box),
  and its **✕** button.
- It is a modal overlay drawn above the terminal (below no other overlay it needs to
  coexist with); it does not steal terminal focus permanently — closing returns focus
  to the active terminal.
- The Esc-to-close handler must be **scoped to when the overlay is open** so it does not
  swallow Esc for other overlays (command palette, search) or the terminal.

## 3. Content

Rendered as a titled dialog ("Keyboard shortcuts") containing a list of rows, each a
**chord chip + a human description**.

**3a. Configurable actions** — one row per keybinding, in this order, read from config:

| Config key | Label |
|------------|-------|
| `new_tab` | New tab |
| `close_tab` | Close tab |
| `next_tab` | Next tab |
| `prev_tab` | Previous tab |
| `copy` | Copy selection |
| `search` | Find in terminal |
| `palette` | Command palette |
| `toggle_man` | Toggle man-page panel |
| `toggle_preview` | Toggle command preview |
| `zoom_in` | Zoom in |
| `zoom_out` | Zoom out |
| `zoom_reset` | Reset zoom |
| `help` | This help |

**3b. Fixed rows** — built-ins that aren't config keybindings but are worth showing,
appended after the configurable ones:

| Chord | Label |
|-------|-------|
| `Ctrl+Shift+V` | Paste (multi-line pastes ask first) |
| `Esc` | Close this help, an overlay, or a panel |

An implementation may extend 3a/3b as new shortcuts are added — the list is the single
source of truth the user sees, so keep it complete.

## 4. Chord prettifying

Config chord strings are token lists joined by `+` (e.g. `Ctrl+Shift+Slash`). For
display, map the final key token to a symbol; leave modifiers and letter/digit tokens
as-is:

| Token | Display |
|-------|---------|
| `Slash` | `?` |
| `Equal` | `=` |
| `Plus` | `+` |
| `Minus` | `−` |
| `Right` | `→` |
| `Left` | `←` |
| `Up` | `↑` |
| `Down` | `↓` |

So `Ctrl+Shift+Slash` → `Ctrl+Shift+?`, `Ctrl+Shift+Equal` → `Ctrl+Shift+=`,
`Ctrl+Shift+Right` → `Ctrl+Shift+→`. Unknown tokens display verbatim.

## 5. Robustness

- Build the row list **each time the overlay opens** (not once at startup), so a
  live config reload is reflected without restart.
- A missing/empty binding string must not crash: display it as blank/omit it, and the
  corresponding chord simply never fires (the parser treats an empty binding as
  "never matches").

## 6. Acceptance criteria

- Pressing the `help` chord opens the overlay; pressing it again (or Esc, or backdrop
  click, or ✕) closes it.
- Every configurable action from §3a appears with its **current** chord (rebinding
  `help` to, say, `Ctrl+Shift+H` changes both the trigger and the displayed row).
- Chords are prettified per §4 (the help row shows `Ctrl+Shift+?`, not
  `Ctrl+Shift+Slash`).
- Opening/closing help never breaks Esc handling for the palette or search overlays.
- A partial config (a keybinding key absent) does not white-screen the app.

## 7. Non-goals

- Editing keybindings from the overlay (it is read-only/display).
- Searching/filtering within the help list.
- Documenting shell or application shortcuts — this lists the **terminal emulator's**
  own shortcuts only.
