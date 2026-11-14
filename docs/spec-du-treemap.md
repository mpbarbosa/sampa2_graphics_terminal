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
  the boxes sum to the whole. Boxes are labelled in place (name + human size) when large
  enough. The native build has no tooltip and no hover state over the boxes — the strip
  above the chart reads out the keyboard-marked cell instead (§5).
- The layout depends on the panel's pixel size, so it is computed in the **frontend**; the
  headless core only parses `du` into the tree.

## 5. Navigation & selection

| Input | Action |
|---|---|
| Click a box | **Zoom** into that directory (if it has children) |
| `←` `→` `↑` `↓` | Move the marker **to the neighbouring cell in that direction** |
| `Space` | **Zoom** into the marked cell — the keyboard's click |
| `Backspace` | Up one level (clearing the marker) |
| `t` | Swap to the rings (and back) |
| `Enter` | **`cd`** — to the marked cell if there is one, else to the directory in view; composed at the prompt, never run |
| `Esc` / ✕ / backdrop | Close |

The marked cell is drawn lifted toward the foreground with a bright surround, and the strip
(§4) reads out its full path and size — the same contract the rings' marker has, so `Enter`
and the readout can never disagree about which directory is meant.

**The four arrow keys mean four directions here, and something else in the rings.** The
rings have two axes to spend them on — around a ring, and across to the ring inside or
outside it — whereas a treemap shows a single level tiling a rectangle, where the only
axes are the screen's. So the two layouts do not share a step type: `DuDir` has `Left`,
`Right`, `Up`, `Down`, and `DuStep` (spec-du-rings.md §5) has `Prev`, `Next`, `In`, `Out`.
Folding them into one enum would make one set of names a lie about what the keys do.

Stepping is computed from the layout alone (`rect_step`), as the rings' is. A squarified
treemap is a *tiling, not a grid* — rows run full-width or full-height, but the cells
within a row have unequal extent, so "the cell to the right" is not a column index.
Candidates are the cells whose centre lies in the direction of travel, ranked by whether
they overlap the current cell on the axis *across* the travel, then by distance along the
travel, then by offset across it. That first key is what keeps a step along a row from
leaving the row: a cell sharing an edge with this one is the one the eye calls its
neighbour, even where a cell in the row above happens to have a nearer centre. Overlap is a
preference rather than a requirement, so a key still moves where nothing sits directly
alongside — refusing it would strand the marker in a corner. Cells too thin to draw are
skipped, since marking one would read out a path with nothing visible beside it.

**The end-stops differ from the rings', and deliberately.** At the edge of the map the
marker stays where it is: a rectangle has no seam to wrap around, so there is no sideways
wrap, and nothing corresponds to the rings' `↑`-from-the-hub step that clears the marker —
every cell here sits at the same level. `Backspace` is what clears a treemap marker, by
leaving the level the cells belonged to.

**The marker is re-pointed by identity after every relayout**, for the reason set out in
spec-du-rings.md §5: it is an index into the laid-out cells, §3's progressive scan rebuilds
them four times a second, and a directory finishing its walk shifts every later cell along.
Within one level a name is unique, so the name is the identity — paired with whether the
cell is the `(files here)` remainder, which carries the view's own path and so would be
indistinguishable by path, and which a real directory could even share a name with. A cell
that is genuinely gone drops the marker.

The pointer does not mark treemap cells: the boxes are labelled in place, so there is
nothing a hover readout would add that the layout does not already show. (The rings need
one because a sector is too narrow to label.)

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
