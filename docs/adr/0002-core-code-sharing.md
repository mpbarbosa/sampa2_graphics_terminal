# ADR 0002 — Shared core code: git dependency now, defer the `sampa-core` repo

- **Status:** Accepted (2026-08-02)
- **Context:** [PLAN.md §2](../PLAN.md) "[Decision] Core consumption"; the feasibility
  note's "consume the core cross-repo" recommendation
  ([rust-only-feasibility.md §5](../rust-only-feasibility.md)). This repo (Path C,
  native) reuses the seven headless crates that live in the origin repo
  [`sampa_graphics_terminal`](https://github.com/mpbarbosa/sampa_graphics_terminal)
  (Path B, Tauri + xterm.js).

## Decision

Consume the seven shared crates **cross-repo via a git dependency pinned to a rev**
(a temporary `path` dep is fine for local N0 development). **Do not** extract a
dedicated `sampa-core` repo yet. Revisit the extraction at the **end of N1/N2**, and
pull the trigger **only if** both repos turn out to actively co-edit the crates.

## Why

The common surface is exactly the seven headless crates — **~1,875 src LOC**
(`pty-core`, `sampa-config`, `sampa-cli`, `sampa-shellint`, `sampa-palette`,
`sampa-man`, `sampa-preview`), verified GUI-free (the only `tauri`/`webview` mentions
are doc comments, not dependencies). Everything else diverges:

| | LOC | Shared? |
|---|---:|---|
| 7 headless crates | ~1,875 | **Yes, 100%** |
| Tauri glue (`src-tauri/src`) | ~497 | No — IPC bridge replaced by direct calls |
| Frontend (`main.ts` + HTML) | ~1,034 | No — replaced by the native renderer/input |

1. **The shared set will not grow.** The native build's new code — VT integration,
   wgpu renderer, glyph atlas, cosmic-text, winit input, image decode, a11y — has
   **no Path B counterpart**, because Path B delegates all of that to xterm.js. Those
   are precisely the parts that *can't* be shared. A dedicated repo would wrap a
   fixed, non-growing set.

2. **The shared fraction shrinks.** The native frontend will reach ~8,000–20,000 LOC
   by v1. Against a fixed ~1,875 shared, that is ~15% at N0 falling to <10% at v1 —
   release ceremony to protect a stable, well-tested *minority* of the code.

3. **There is effectively one active editor, not two.** Path B has shipped M0–M5 and
   is in maintenance; Path C is where change happens. The "no drift between two
   co-evolving consumers" argument — the main case for a shared repo — barely applies.

4. **The core API will churn during N0–N1.** Going native *collapses the seam*: the
   VT layer moves into the core and `pty-core`'s event API may need native-loop
   tweaks. Locking an unstable API behind a repo boundary + release cadence is exactly
   when a shared repo hurts most — every core tweak becomes a publish-then-bump dance.

Extraction is cheap and mechanical **later** (`git filter-repo` / `git subtree split`
preserves history); it is premature **now**.

## Consequences

- This repo's Cargo workspace depends on the seven crates via a **git dependency
  pinned to a rev** of `sampa_graphics_terminal` (bump the rev deliberately). Local
  N0 work may use a `path` dep against a sibling checkout, but committed builds pin a
  rev so CI is reproducible.
- If a core change is needed for the native build, make it in the origin repo, pin the
  new rev here. Keep the crates headless — no winit/wgpu/cosmic-text types leak in.
- **Revisit trigger (end of N1/N2):** if, in practice, both repos are landing changes
  to the shared crates, extract **`sampa-core`** — a Cargo workspace of the crates,
  history preserved via `git filter-repo`; both apps then pin a tagged version. If
  Path B stays frozen, the git dep carries this repo to v1 and no third repo is
  needed.
- New Path-C-only core logic (e.g. a native VT/grid layer, escape-hardening +
  DECRQCRA) lives in *this* repo's crates, **not** the shared set — it has no Path B
  consumer.
