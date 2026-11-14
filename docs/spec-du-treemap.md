# Spec — `du` disk-usage treemap

- **Status (this native/Path C build):** **implemented.** Typed `du` + `Ctrl+Shift+D`
  scans the session cwd with a read-only, progressively-streamed `du -k -x --max-depth=4` on a
  background thread (`run_du`), parses it with `sampa_dumap::parse_du`, and draws a
  **squarified treemap** (native `squarify`) as coloured, labelled quads over the grid — a
  breadcrumb strip on top, click-to-zoom, `Backspace` up a level, `Enter` composes `cd
  <path>` at the prompt (insert-never-run), `Esc` closes. The keybinding is the native ps
  chord (`Ctrl+Shift+D`), dispatched on the typed command like the `cd` picker.
- **Second layout:** the same view also draws as **rings** (a sunburst) — `t` toggles.
  Scan, tree, zoom stack and `cd` composition are shared; see
  [spec-du-rings.md](spec-du-rings.md).
- **Applies to:** visualising the current directory's disk usage as a treemap, opened from
  the keyboard. Language-agnostic behavioral contract so any frontend (webview or native
  Rust) behaves identically.

## 1. Purpose

`du` answers "what's using my disk", but its flat wall of `<size>\tpath` lines makes the
big consumers hard to spot and the hierarchy invisible. Sampa renders the same data as a
**squarified treemap** — each directory a box sized by its disk usage — so the largest
consumers are obvious at a glance and the tree is navigable. The view is read-only; the one
side effect it offers is composing a `cd` at the prompt (**inserted, never executed** —
DESIGN.md §10.1 / §13).

## 2. Trigger

- Bound to the **shared enhance shortcut** — `keybindings.enhance_ps`, default
  **`Ctrl+Shift+E`** — dispatched on the tracked command line (`tab.typed`, keystroke-derived).
  The first token selects the view: **`cd` → directory tree picker, `du` → this treemap,
  anything else → the `ps` output decorator.**
- A modal overlay; **Esc**, its **✕**, or a **backdrop click** dismiss it and return focus
  to the terminal.

## 3. Scanning

- On trigger, the emulator runs a **read-only `du`** on the **session cwd** (obtained like
  the other cwd-aware features: OSC 7 if present, else `/proc/<pid>/cwd`). The command is
  `du -k -x --max-depth=4 <cwd>`: kibibytes (portable), one filesystem (`-x`, so it doesn't
  wander into mounts), depth-capped to bound the output.
- **`du` traverses the whole subtree and can be slow**, so it runs off the UI/async thread
  and its output is **streamed into the view as it arrives**. `du` prints each directory as
  it finishes it — measured on a 142 GB `$HOME`, 10 924 rows spread evenly across 91 s: 11 %
  of them within 9 s — so the overlay builds a tree from the rows it has and redraws every
  `DU_PROGRESS_MS` (250 ms). Something is on screen about a second in, and the breadcrumb
  says `scanning… N dirs` while sizes are still rising.
- A directory whose own row has not arrived is sized by the children that have: a **lower
  bound that only rises**, never a number that jumps around. The strip's total is therefore
  "so far", not the answer, until the count stops.
- `DU_TIMEOUT_SECS` (600 s) is now only a **backstop** against a `du` wedged on a dead
  mount, not the limit on what the view can show. If it fires, whatever arrived is kept and
  the strip says the view is partial. Closing the overlay or re-scanning **kills the child**
  rather than leaving it walking a large tree unseen. Nothing is retried automatically.
- `du` only stats the filesystem — no writes, and the path is never interpolated into a
  shell.

## 4. The treemap

- Built from the parsed tree (`<size>\tpath` → nested, size-aggregated `DuNode`, children
  largest-first). The **squarified layout** (Bruls et al.) keeps each box's aspect ratio
  near 1 so areas are comparable; box **area ∝ disk usage**.
- Each level shows the current directory's child directories, plus a **`(files here)`**
  remainder box for the bytes in the directory itself (its size minus the children's) so
  the boxes sum to the whole. Boxes are labelled (name + human size) when large enough; all
  boxes carry a hover tooltip.
- The layout depends on the panel's pixel size, so it is computed in the **frontend**; the
  headless core only parses `du` into the tree.

## 5. Navigation & selection

| Input | Action |
|---|---|
| Click a box | **Zoom** into that directory (if it has children) |
| Breadcrumb segment | Jump back to that ancestor level |
| `Backspace` | Up one level |
| `Enter` | **`cd` to the directory currently in view** — compose `cd <path>` at the prompt and close |
| `Esc` / ✕ / backdrop | Close |

`Enter` composes the command and **never appends a newline**: the path is shell-quoted when
it contains whitespace/metacharacters, the tracked line is erased first, and `cd <path> ` is
written for the user to run themselves — the insert-never-run boundary the palette, `cd`
picker, and AI features all honor.

## 6. Architecture mapping

- **`crates/dumap` (`sampa-dumap`)** — headless parse core. `parse_du(output) -> DuNode`
  builds the size-aggregated tree (children sorted largest-first) and **fails safe to
  `None`** on malformed input (no tab / non-numeric size), mirroring `ps-decorate`. Pure
  `std` + serde — **no shell, no Tauri**. Tested against sample and real `du` output.
- **Bridge** — in this native build, `stream_du(cwd, gen, …)` owns the child on its own
  thread and posts `DuProgress` every 250 ms, `DuReady` at the end, or `DuTruncated` at the
  backstop. A reader thread owns the pipe so the budget holds even if `du` stops printing,
  and `DuBuild` turns rows into the tree from a root known up front (the parser infers one,
  which only works on complete output). The web build's `run_du(path)` remains the
  batch form: `spawn_blocking`, child killed on expiry, parsed tree returned.
- **Frontend** — the `#dumap` SVG overlay owns the squarify layout, the zoom stack +
  breadcrumb, and the compose-and-insert step. Directory names reach the DOM via
  `textContent` only.

## 7. Relationship to existing docs

Peer to `spec-ps-output-enhancement.md`, `spec-cd-tree-picker.md`, the palette, and help
specs. It reuses the **keystroke-derived command dispatch** on the shared `Ctrl+Shift+E`
shortcut and the **insert-never-run** rule, and it borrows the **fail-safe-to-`None`** parse
discipline from `ps-decorate` and the **read-only, timeout-bounded external command** pattern
from `preview`. Possible follow-ups: honor a path argument typed after `du` (root the scan
there rather than the cwd), and top-N-per-level aggregation for very large trees.
