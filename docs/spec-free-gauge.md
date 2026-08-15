# Spec — `free` memory gauge

- **Status (this native/Path C build):** the headless **`sampa-freemem` core is available** —
  wired in as a git-pinned dependency of `sampa-native` (per ADR 0002). The feature ships
  end-to-end in the `sampa_graphics_terminal` reference (Tauri) build (`crates/freemem`, the
  `run_free` bridge, the `#freegauge` overlay); the **native gauge UI that consumes the
  parsed stats is Path C's follow-up**. Mirrored here as the behavioral contract; the
  file/keybinding references describe the reference build.
- **Applies to:** visualising RAM and swap usage as proportional gauges, opened from the
  keyboard. Language-agnostic behavioral contract so any frontend (webview or native Rust)
  behaves identically.

## 1. Purpose

`free`'s two-row table of raw numbers makes proportions hard to read at a glance — how much
RAM is *really* in use versus reclaimable cache, how close swap is to full. Sampa renders
the same data as **proportional segmented gauges** so used / buff-cache / free (and swap)
are immediately legible. It is **purely informational**: unlike the `ps` / `cd` / `du`
views, there is no command to compose, so this view has **no insert action** — it only
displays.

## 2. Trigger

- Bound to the **shared enhance shortcut** — `keybindings.enhance_ps`, default
  **`Ctrl+Shift+E`** — dispatched on the tracked command line (`tab.typed`, keystroke-derived).
  The first token selects the view: **`cd` → tree picker, `du` → treemap, `free` → this
  gauge, anything else → the `ps` output decorator.**
- A modal overlay; **Esc**, **Enter**, its **✕**, or a **backdrop click** dismiss it and
  return focus to the terminal.

## 3. Data

- On trigger the emulator runs a **read-only `free -k`** (kibibytes; portable). `free` reads
  `/proc/meminfo` and returns instantly, so — unlike `du` — **no timeout is needed**. No
  shell; nothing is executed on the user's behalf.
- The core parses the table into `FreeInfo`:
  - **RAM** (`Mem:`): total, used, free, shared, buff/cache, available.
  - **Swap** (`Swap:`, optional): total, used, free.
- **Fails safe:** output without a usable `Mem:` row yields nothing and the overlay shows a
  short message. An older `free` that omits the `available` column falls back to
  `free + buff/cache`.

## 4. The gauges

- **RAM** — a segmented bar whose segments (**used / buff-cache / free**) are sized in
  proportion to total, with a legend giving each segment's size and percentage.
  **`available`** is shown in the legend only (a green marker), not as a segment, because it
  overlaps used/cache (it is free plus reclaimable cache — the kernel's estimate of what a
  new workload can use without swapping).
- **Swap** — shown only when swap exists (`total > 0`): a **used / free** segmented bar with
  the same legend treatment.
- Sizes are formatted in human units (K/M/G). Colours are redundant with segment length —
  no state is conveyed by hue alone.

## 5. Architecture mapping

- **`crates/freemem` (`sampa-freemem`)** — headless parse core. `parse_free(output) -> FreeInfo`
  reads the `Mem:` (required) and `Swap:` (optional) rows and **fails safe to `None`**,
  mirroring `ps-decorate` / `dumap`. Pure `std` + serde — **no shell, no Tauri**. Tested
  against sample and real `free` output.
- **Bridge** — `run_free()` runs the read-only `free -k` and returns the parsed stats. No
  timeout (instant); no arguments.
- **Frontend** — the `#freegauge` overlay owns the proportional bar rendering and the
  legend. Numbers reach the DOM via `textContent` only.

## 6. Relationship to existing docs

Peer to `spec-ps-output-enhancement.md`, `spec-cd-tree-picker.md`, `spec-du-treemap.md`, and
the palette/help specs. It reuses the **keystroke-derived command dispatch** on the shared
`Ctrl+Shift+E` shortcut and the **fail-safe-to-`None`** parse discipline. It is the one
enhance view **without** an insert-never-run action — memory stats don't map to a command —
so it is a read-only visualization end to end.
