# Spec — `du` disk-usage treemap

- **Status (this native/Path C build):** the headless **`sampa-dumap` core is available** —
  wired in as a git-pinned dependency of `sampa-native` (per ADR 0002). The feature ships
  end-to-end in the `sampa_graphics_terminal` reference (Tauri) build (`crates/dumap`, the
  `run_du` bridge, the `#dumap` overlay); the **native treemap UI (squarify + zoom) that
  consumes the parsed tree is Path C's follow-up**. Mirrored here as the behavioral
  contract; the file/keybinding references describe the reference build.
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
  with a **wall-clock timeout** (default 6 s); on expiry the child is killed and the overlay
  shows a short message. Nothing is retried automatically.
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
- **Bridge** — `run_du(path)` runs the read-only, timeout-bounded `du` off the async runtime
  (`spawn_blocking`; child killed on expiry) and returns the parsed tree.
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
