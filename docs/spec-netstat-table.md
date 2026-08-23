# Spec — `netstat` connections table

- **Status:** implemented (reference: Tauri+xterm.js build — `crates/netdec`
  (`sampa-netdec`), the `run_netstat` bridge command, and the `#netpanel` overlay in
  `src/main.ts`).
- **Applies to:** visualising the host's open network sockets as a state-coloured table,
  opened from the keyboard. Language-agnostic behavioral contract so any frontend (webview or
  native Rust) behaves identically.

## 1. Purpose

`netstat`/`ss` output is a dense wall of addresses and cryptic state names. Sampa runs the
query and draws the sockets as a **table coloured by connection state** — listening sockets
and established connections at a glance, with the owning process where the kernel exposes it.
Purely informational: nothing is composed or run.

## 2. Trigger

- Bound to the **shared enhance shortcut** — `keybindings.enhance_ps`, default
  **`Ctrl+Shift+E`** — dispatched on the tracked command line (`tab.typed`, keystroke-derived).
  The first token selects the view: `cd` → tree, `du` → treemap, `free` → gauge, `ping` →
  chart, `df` → gauge, `uptime` → gauge, **`netstat` *or* `ss` → this table**, anything else →
  the `ps` output decorator.
- A modal overlay; **Esc**, its **✕**, or a **backdrop click** dismiss it.

## 3. Data

- The data source is **`ss`** (iproute2), *not* `netstat`. `netstat` (net-tools) is deprecated
  and often absent on modern distros, and its udp rows drop the State column — an irregular
  layout. `ss` is the maintained replacement with a uniform table, so **both** typed commands
  (`netstat` and `ss`) route to an `ss` query.
- On trigger the emulator runs a **read-only** `ss -tunap` (`-t` tcp, `-u` udp, `-n` numeric,
  `-a` all states, `-p` process). It is **timeout-bounded** — the child is killed at 5s — and
  runs off the async runtime. No shell.
- The core parses each row into `Conn { proto, state, local, peer, process }`. The process
  name and pid are pulled from the `users:(("name",pid=N,fd=M))` field when present (`-p`
  requires privilege to see *other* users' processes; absent → `None`). **Fails safe:** output
  without the `Netid`/`State … Local Address` header, or any malformed row, → nothing.

## 4. The table

- One row per socket: **proto · state · local address · peer address · process**. Rows are
  **coloured by state** — LISTEN / UNCONN green (waiting), ESTAB blue (active), `*WAIT` /
  `CLOS*` orange (tearing down) — colour redundant with the state text, never the sole signal.
- Sorted **listening-first** so servers surface above the churn of client connections, then a
  summary line: total sockets · listening · established.
- The table is scrollable and pixel-dependent, so it lives in the **frontend**; the core only
  parses.

## 5. Architecture mapping

- **`crates/netdec` (`sampa-netdec`)** — headless parse core. `parse_ss(output) ->
  Option<Vec<Conn>>`, fail-safe-to-`None`, mirroring `ps-decorate` / `dumap` / `freemem` /
  `uptimedec`. Pure `std` + serde — **no shell, no Tauri**. Tested against a sample and real
  `ss -tunap`.
- **Bridge** — `run_netstat()` runs the read-only, timeout-bounded `ss -tunap` off the async
  runtime and parses it.
- **Frontend** — the `#netpanel` overlay owns the table rendering, the state colouring, the
  listening-first sort, and the summary. Text reaches the DOM via `textContent` only.

## 6. Relationship to existing docs

Peer to `spec-ps-output-enhancement.md`, `spec-cd-tree-picker.md`, `spec-du-treemap.md`,
`spec-free-gauge.md`, `spec-ping-chart.md`, `spec-df-gauge.md`, and `spec-load-gauge.md` — the
eighth `Ctrl+Shift+E` view. It reuses the keystroke-derived command dispatch and the
fail-safe-to-`None` parse discipline. Note the **data-source substitution** (`ss` for the
typed `netstat`) is the load-bearing detail: net-tools is deprecated and irregular, so the
core is only ever handed `ss`'s uniform layout.
