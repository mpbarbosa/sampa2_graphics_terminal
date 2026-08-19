# Spec — `uptime` load gauge

- **Status (this native/Path C build):** the headless **`sampa-uptimedec` core is available**
  — wired in as a git-pinned dependency of `sampa-native` (per ADR 0002). The feature ships
  end-to-end in the `sampa_graphics_terminal` reference (Tauri) build (`crates/uptimedec`, the
  `run_uptime` bridge, the `#loadgauge` overlay); the **native gauge UI that consumes the
  parsed loads is Path C's follow-up** (and must likewise run `uptime` with `LC_ALL=C`).
  Mirrored here as the behavioral contract; the file/keybinding references describe the
  reference build.
- **Applies to:** visualising the 1/5/15-minute load averages relative to the CPU core count,
  opened from the keyboard. Language-agnostic behavioral contract so any frontend (webview or
  native Rust) behaves identically.

## 1. Purpose

A bare load average — `0.74, 0.30, 0.17` — is meaningless without the core count: on a
14-core machine that's idle, on a single-core box it's overloaded. Sampa runs `uptime` and
draws the three load averages as bars **scaled to the CPU core count** (100% = load == cores),
coloured by that ratio, so "how busy is this machine, really" is answered at a glance. Purely
informational: nothing is composed or run.

## 2. Trigger

- Bound to the **shared enhance shortcut** — `keybindings.enhance_ps`, default
  **`Ctrl+Shift+E`** — dispatched on the tracked command line (`tab.typed`, keystroke-derived).
  The first token selects the view: `cd` → tree, `du` → treemap, `free` → gauge, `ping` →
  chart, `df` → gauge, **`uptime` → this gauge**, anything else → the `ps` output decorator.
- A modal overlay; **Esc**, **Enter**, its **✕**, or a **backdrop click** dismiss it.

## 3. Data

- On trigger the emulator runs a **read-only** `uptime`. **`LC_ALL=C` is forced**, and this
  matters: under some locales `uptime` prints the load averages with a decimal **comma**
  (`load average: 0,74, 0,30, 0,17`), where the comma is both the decimal point *and* the
  list separator — unparseable. In the C locale it is `0.74, 0.30, 0.17`. `uptime` reads
  `/proc/loadavg` and returns instantly, so no timeout. No shell.
- The core parses the load line into `load1` / `load5` / `load15`, plus a **best-effort**
  uptime-duration (`up 1 day,  3:11`) and user count for display. **Fails safe:** no
  `load average` line → nothing.
- The bridge attaches the **CPU core count** (`available_parallelism`) alongside, so the
  frontend can express each load as a fraction of cores.

## 4. The gauge

- Three horizontal bars — **1 min / 5 min / 15 min** — each filled to `min(load / cores, 1)`,
  **coloured by the load ÷ cores ratio**: green < 0.7, yellow < 1.0, orange < 1.5, red ≥ 1.5
  (≥ 1.0 means the machine is at/over capacity for that window). Each row shows the raw load
  and its percentage of cores. A subtitle shows `up <duration> · N users · M cores`. Colour
  is redundant with bar length — never the sole signal.
- The layout is pixel-dependent, so it lives in the **frontend**; the core only parses.

## 5. Architecture mapping

- **`crates/uptimedec` (`sampa-uptimedec`)** — headless parse core. `parse_uptime(output) ->
  UptimeInfo`, fail-safe-to-`None`, mirroring `ps-decorate` / `dumap` / `freemem`. **Expects
  C-locale output** (the bridge forces it). Pure `std` + serde — **no shell, no Tauri**.
  Tested against C-locale samples and real `uptime`.
- **Bridge** — `run_uptime()` runs `uptime` with `LC_ALL=C`, parses it, and returns the loads
  plus the CPU `cores` count.
- **Frontend** — the `#loadgauge` overlay owns the bar rendering, the load-÷-cores scaling,
  and the colour bands. Text reaches the DOM via `textContent` only.

## 6. Relationship to existing docs

Peer to `spec-ps-output-enhancement.md`, `spec-cd-tree-picker.md`, `spec-du-treemap.md`,
`spec-free-gauge.md`, `spec-ping-chart.md`, and `spec-df-gauge.md` — the seventh
`Ctrl+Shift+E` view. It reuses the keystroke-derived command dispatch and the
fail-safe-to-`None` parse discipline. Note the **locale normalization** at the bridge
(`LC_ALL=C`) is a small but load-bearing detail — the core is only ever handed dot-decimal
output.
