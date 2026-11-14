# Spec — `du` disk-usage rings (sunburst)

- **Status (this native/Path C build):** **implemented.** A second layout for the `du`
  enhance view — `t` toggles between it and the squarified treemap
  ([spec-du-treemap.md](spec-du-treemap.md)). Same scan, same tree, same navigation and the
  same insert-never-run `cd`; only the geometry differs.
- **Applies to:** reading the current directory's disk usage as nested rings — every
  scanned depth on screen at once, rather than one level at a time.

## 1. Purpose

The treemap answers "what is biggest here?" well, but it shows **one level per screen**: the
hierarchy is something you reach by zooming, not something you see. The scan already
collects four levels (`du --max-depth=4`), so three of them are discarded at every draw.

The rings render that same tree radially — a **sunburst**: one band per depth, each
directory a sector whose angle is its share of its parent. Depth becomes distance from the
centre and the whole subtree is legible at a glance, which is what makes a deep chain of
single big directories (`.cache/ollama/models/blobs`) obvious instead of four zooms away.

It is the same read-only view as the treemap: the one side effect it offers is composing a
`cd` at the prompt, **inserted, never executed** (DESIGN.md §10.1 / §13).

## 2. Trigger

- Opened exactly like the treemap: type `du`, press the enhance chord (**`Ctrl+Shift+D`** in
  this build — `Ctrl+Shift+E` is `toggle_preview` here, see the chord table in `main.rs`).
- **`t`** toggles rings ↔ treemap while the view is open. The mode is remembered for the
  session, so the next `du` opens in whichever layout was last used.
- `Esc` closes, as with every overlay.

## 3. Scanning

Unchanged — the same read-only `du -k -x --max-depth=4 <cwd>` on a background thread, now
**streamed into the view as it arrives** (spec-du-treemap.md §3). **Nothing extra is
scanned to draw the rings**; they are a second reading of the data the treemap already had.

So the rings **do** fill in progressively, the way a dedicated analyser such as `baobab`
does — the bands appear within a second or so and thicken as rows land, with the breadcrumb
showing `scanning… N dirs` until the walk ends. Sizes are lower bounds while that count is
moving: a sector can only grow, and the ring it sits in can only gain neighbours.

The tree is built by `DuBuild` rather than `sampa_dumap::parse_du`, which infers the root
from the rows it is given. Mid-scan that inference lands four levels down, because `du`
prints children before parents — so the partial view would be rooted in the wrong place and
re-root itself repeatedly. `DuBuild` is told the root up front (it is the cwd being
scanned) and links each row to it through every ancestor immediately, so a subtree is
visible while it is still being walked. The finished tree is identical to the parser's,
which is asserted by a test rather than assumed.

## 4. The rings

- **Angle ∝ size.** A full turn is the view directory. Each child takes the fraction of its
  parent's sweep that its `size_kb` is of the parent's, laid out largest-first clockwise
  from 12 o'clock, and recursing into each child's own sweep. One level's sectors therefore
  tile their parent's exactly, as the treemap's boxes tile its rect.
- **Radius = depth.** Band `n` holds the directories `n` levels below the view. The number
  of bands is the **view's own depth** (`subtree_depth`, capped at `DU_RING_DEPTH` = 4), not
  a constant — so zooming to the bottom of the scan gives one full-thickness ring rather
  than a thin one adrift in a circle sized for levels that aren't there.
- **`(files here)`** closes each ring: the bytes in that directory itself (its size minus
  its children's), so every level sums to the whole. Muted, and not zoomable — there is no
  directory behind it. The radial counterpart of the treemap's remainder box.
- **Colour** comes from the same `DU_PALETTE`, assigned at depth 0 and **inherited down the
  branch**, lightened a step per level, so a subtree reads as one family and the eye can
  follow it outwards.
- **Slivers are dropped.** A sector narrower than `DU_MIN_SWEEP` (~0.7°) is sub-pixel at the
  outer ring; it and its subtree are skipped rather than emitting invisible geometry.
- The hub is left empty for the view directory's own name and size — the directory `Enter`
  would `cd` to.

## 5. Navigation & selection

| Input | Action |
|---|---|
| Hover a sector | Its **full path and size** in the strip (see §6) |
| Click a sector | **Zoom** to that directory — from any ring, descending every level between in one step |
| `←` / `→` | Move the marker **along the current ring**, wrapping at 12 o'clock |
| `↑` / `↓` | Move it **across rings** — in toward the hub, or out into the sector below this one |
| `Space` | **Zoom** to the marked sector — the keyboard's click |
| `Backspace` | Up one level |
| `t` | Swap to the treemap (and back) |
| `Enter` | **`cd`** — to the marked sector if there is one, else to the directory in view; composed at the prompt, never run |
| `Esc` | Close |

Clicking an outer ring descends several levels at once: each sector carries its whole
child-index path from the view node, which is the navigational advantage of showing the
hierarchy. Clicking `(files here)`, or a directory with no children, does nothing.

**The pointer and the arrows share one marker**, so the strip never has two things to read
out and `Enter` never has two directories to choose between. Arrowing away from a hovered
sector continues from where the pointer left off; moving the pointer onto a sector takes
the marker back.

Stepping is computed from the layout alone (`arc_step`), not from stored tree state: a ring
is the set of sectors sharing an `r0`, and because one ring's sectors tile their parent's
sweep exactly, "the sector below this one" is the one in the next ring out whose sweep
contains the current sector's mid-angle. So a relayout — a resize, or one of the mid-scan
updates in §3 — cannot leave the marker describing a tree that is no longer on screen.

The two end-stops differ deliberately. `↑` from the innermost ring **drops the marker**, so
the strip reads the whole view again; `↓` from the outermost ring, or out of `(files
here)`, **leaves it where it is** rather than silently losing it. Along a ring there is no
end-stop at all: a ring is a circle, and stopping at the 12 o'clock seam would strand the
sectors either side of it.

Rings only. The treemap shows one level at a time and is navigated by clicking into it.

## 6. Labels, and why there is a hover readout

Labels are **horizontal only**. glyphon 0.12 has no per-text-area transform, so text cannot
be rotated to follow a ring — a sunburst's usual radial/tangential labelling is not
available. A sector is therefore labelled only where a horizontal label fits inside it: the
**chord at its mid radius** by the **ring thickness** is the whole budget (name alone in a
thin band, name + size where two lines fit), centre-aligned on the sector's centroid.

The chord is an **estimate, not a bound** — through a sector facing east a horizontal line
runs radially, where the room is the band thickness — and its **sweep is clamped to half a
turn** before the chord is taken. Unclamped, `sin(sweep/2)` falls back to zero at a full
turn, so the sectors with the most room were the ones refused a label: a directory holding
one dominant child drew that child unlabelled while labelling its own smaller children.

That leaves most outer sectors unlabelled, so the breadcrumb strip doubles as a **hover
readout**: the sector under the pointer puts its full path and size in the strip, and lifts
toward the foreground so the readout is tied to something visible. This is how `baobab` and
most sunburst UIs behave too, for the same reason.

## 7. Architecture mapping

- **`crates/dumap` (`sampa-dumap`)** — unchanged. The headless parse core knows nothing
  about either layout.
- **Layout** — `sunburst` / `push_ring` in `main.rs`, peer to `squarify`: pure geometry over
  a `&DuNode`, returning `DuArc` sectors. `subtree_depth` sizes the bands; `arc_hit`,
  `arc_label_box` and `arc_bbox` are its hit-test, label-placement and bounding-box helpers.
  Unit-tested like `squarify` (proportions, nesting, sliver and depth bounds, hit-test
  agreement, bbox coverage).
- **Renderer** — the quad pass places **axis-aligned rectangles only**, so a sector cannot be
  drawn as geometry. `ARC_SHADER` is a second pipeline: each sector is submitted as its
  bounding quad (`arc_bbox`) and carved out per-fragment by a polar test. Radial and angular
  edges are eroded by a fixed number of **pixels** (not a fixed angle), so seams stay the
  same visual width on every ring, and `smoothstep` antialiases all four edges — which is
  what keeps thin outer rings legible. It shares the quad pass's group-0 screen uniform; the
  fragment stage needs no uniform, taking its pixel as a varying.

## 8. Relationship to existing docs

A sibling of [spec-du-treemap.md](spec-du-treemap.md), which owns the scan, the tree, the
zoom stack and the `cd` composition this view reuses verbatim; see
[spec-enhance-views.md](spec-enhance-views.md) for how `du` is dispatched among the enhance
views. Both follow-ups recorded here are now **implemented**: the progressive scan (§3) and
keyboard navigation within the rings (§5).
