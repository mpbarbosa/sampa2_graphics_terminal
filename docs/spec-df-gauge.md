# Spec — `df` disk-free gauge

- **Status (this native/Path C build):** the headless **`sampa-dfdec` core is available** —
  wired in as a git-pinned dependency of `sampa-native` (per ADR 0002). The feature ships
  end-to-end in the `sampa_graphics_terminal` reference (Tauri) build (`crates/dfdec`, the
  `run_df` bridge, the `#dfgauge` overlay); the **native gauge UI that consumes the parsed
  rows is Path C's follow-up**. Mirrored here as the behavioral contract; the file/keybinding
  references describe the reference build.
- **Applies to:** visualising per-filesystem disk usage as proportional gauges, opened from
  the keyboard. Language-agnostic behavioral contract so any frontend (webview or native
  Rust) behaves identically.

## 1. Purpose

`df`'s columns of numbers make it hard to see, at a glance, which mount is nearly full.
Sampa runs `df` and draws **one proportional bar per filesystem** — used / reserved / free,
coloured by use% and sorted fullest-first — so a near-full disk is obvious. Purely
informational: nothing is composed or run.

## 2. Trigger

- Bound to the **shared enhance shortcut** — `keybindings.enhance_ps`, default
  **`Ctrl+Shift+E`** — dispatched on the tracked command line (`tab.typed`, keystroke-derived).
  The first token selects the view: `cd` → tree, `du` → treemap, `free` → gauge, `ping` →
  chart, **`df` → this gauge**, anything else → the `ps` output decorator.
- A modal overlay; **Esc**, **Enter**, its **✕**, or a **backdrop click** dismiss it.

## 3. Data

- On trigger the emulator runs a **read-only** `df -k` (kibibytes; portable). Because `df`
  stats every mount and can **block on a stale network mount**, it runs off the UI/async
  thread with a **wall-clock timeout** (6 s; the child is killed on expiry). No shell.
- The core parses the table into one `FsUsage` per filesystem: `filesystem`, `size_kb`,
  `used_kb`, `avail_kb`, `use_pct`, `mount`. The header (`Filesystem … Mounted on`) is
  required; the mount path (which may contain spaces) is the row's remainder. Rows with too
  few columns or a non-numeric size / `-` use% are skipped. **Fails safe:** a non-`df`
  header or no valid rows yields nothing.

## 4. The gauges

- One row per filesystem, **sorted by use% descending** (fullest first). Each is a
  proportional segmented bar summing to the filesystem size:
  - **used** — coloured by a use% band (green < 70%, yellow < 85, orange < 95, red ≥ 95);
  - **reserved** — `size − used − avail` (root-only blocks), a dim slice when non-zero;
  - **free** — available.
- A label shows the mount (with the device on hover) and `use% · used / size · free`. Colour
  is redundant with bar length — never the sole signal.
- The layout is pixel-dependent, so it lives in the **frontend**; the core only parses.

## 5. Architecture mapping

- **`crates/dfdec` (`sampa-dfdec`)** — headless parse core. `parse_df(output) ->
  Vec<FsUsage>`, header-gated and fail-safe-to-`None`, mirroring `ps-decorate` / `dumap` /
  `freemem`. Pure `std` + serde — **no shell, no Tauri**. Tested against sample and real
  `df` output.
- **Bridge** — `run_df()` runs the read-only, timeout-bounded `df -k` off the async runtime
  and returns the parsed rows.
- **Frontend** — the `#dfgauge` overlay owns the per-filesystem bar rendering, use% colour
  bands, and sort. Mount/device text reaches the DOM via `textContent` only.

## 6. Relationship to existing docs

Peer to `spec-ps-output-enhancement.md`, `spec-cd-tree-picker.md`, `spec-du-treemap.md`,
`spec-free-gauge.md`, and `spec-ping-chart.md` — the sixth `Ctrl+Shift+E` view. It reuses
the keystroke-derived command dispatch, the fail-safe-to-`None` parse discipline, and the
bounded, read-only external-command pattern (`du`, `ping`).
