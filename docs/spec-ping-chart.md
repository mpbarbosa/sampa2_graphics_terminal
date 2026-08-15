# Spec — `ping` latency chart

- **Status (this native/Path C build):** the headless **`sampa-pingdec` core is available** —
  wired in as a git-pinned dependency of `sampa-native` (per ADR 0002). The feature ships
  end-to-end in the `sampa_graphics_terminal` reference (Tauri) build (`crates/pingdec`, the
  `run_ping` bridge, the `#pingchart` overlay); the **native chart UI that consumes the
  parsed report is Path C's follow-up**. Mirrored here as the behavioral contract; the
  file/keybinding references describe the reference build.
- **Applies to:** visualising per-packet ping latency as a bar chart, opened from the
  keyboard. Language-agnostic behavioral contract so any frontend (webview or native Rust)
  behaves identically.

## 1. Purpose

`ping`'s output is a wall of near-identical reply lines; the *shape* of the latency —
jitter, spikes, and dropped packets — is invisible until you read every `time=` value.
Sampa runs a **bounded** ping and draws the per-packet round-trip times as a **bar chart**
with an average baseline and a summary, so the shape is obvious at a glance. Informational:
nothing is composed or run beyond the ping the user asked for.

## 2. Trigger

- Bound to the **shared enhance shortcut** — `keybindings.enhance_ps`, default
  **`Ctrl+Shift+E`** — dispatched on the tracked command line (`tab.typed`, keystroke-derived).
  The first token selects the view: `cd` → tree, `du` → treemap, `free` → gauge, **`ping` →
  this chart**, anything else → the `ps` output decorator.
- A modal overlay; **Esc**, **Enter**, its **✕**, or a **backdrop click** dismiss it.

## 3. Running the ping

- The **host** is taken from the typed line — the last non-flag token (ping's host is
  normally last). No host → nothing happens.
- On trigger the emulator runs a **bounded, read-only** ping: `ping -c 20 -i 0.2 -w 8
  <host>` — ~20 packets at 0.2 s spacing (~4 s), with an 8 s hard deadline and a process
  kill at 12 s as a backstop. Bounded (unlike a bare `ping`, which runs until Ctrl+C) so the
  overlay isn't waiting forever. It runs off the UI/async thread.
- `host` is passed as a **lone argv** (no shell — nothing is interpolated into a shell), and
  a leading `-` is rejected so it can't be read as a ping option. Sending ICMP echoes to a
  host the user named is a normal diagnostic, not a third-party data egress.

## 4. The chart

- Built from the parsed report: each reply's `icmp_seq` + `time`, plus the
  transmitted/received/loss and `rtt min/avg/max/mdev` summary.
- **One bar per packet**, indexed by sequence 1..max: bar **height ∝ RTT** (scaled to the
  max), **coloured by latency band** (green < 30 ms, yellow < 100, orange < 200, red ≥ 200).
  A **missing sequence (packet loss)** is a short **red floor tick**, so loss is visible in
  place. An **average baseline** (dashed) crosses the chart; a summary line shows host/ip,
  sent/received/loss, and min/avg/max/mdev. Colour is redundant with height — never the sole
  signal.
- The layout depends on the panel's pixel size, so it is computed in the **frontend**; the
  headless core only parses `ping` into the series + summary.

## 5. Architecture mapping

- **`crates/pingdec` (`sampa-pingdec`)** — headless parse core. `parse_ping(output) ->
  PingReport` extracts the replies and summary and **fails safe to `None`** when the text is
  neither replies nor a stats line, mirroring `ps-decorate` / `dumap` / `freemem`. Pure
  `std` + serde — **no shell, no Tauri**. Tested against sample and real `ping` output.
- **Bridge** — `run_ping(host)` runs the bounded ping off the async runtime (killed on the
  backstop timeout) and returns the parsed report.
- **Frontend** — the `#pingchart` SVG overlay owns the bar layout, colour bands, loss ticks,
  average baseline, and summary. Host/ip/text reach the DOM via `textContent` only.

## 6. Relationship to existing docs

Peer to `spec-ps-output-enhancement.md`, `spec-cd-tree-picker.md`, `spec-du-treemap.md`, and
`spec-free-gauge.md`. It reuses the **keystroke-derived command dispatch** on the shared
`Ctrl+Shift+E` shortcut, the **fail-safe-to-`None`** parse discipline, and the **bounded,
read-only external command** pattern (like `du`). A possible alternative worth noting:
decorate a `ping` the user *already ran* by scraping the scrollback (as the `ps` decorator
does), instead of running a fresh bounded ping.
