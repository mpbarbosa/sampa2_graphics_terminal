# Spec — Command-palette search matching

> **Provenance.** Copied from the reference
> [`sampa_graphics_terminal`](https://github.com/mpbarbosa/sampa_graphics_terminal)
> build, where this behavior is implemented in `src/main.ts`
> (`scoreMatch`/`scoreToken`). It is the **contract this Rust build should implement**
> for the command palette; §-numbered references point at that repo's `docs/DESIGN.md`.

- **Status:** target behavior — refine this repo's native command palette
  (`crates/sampa-native`, feature N4) so its matcher meets this contract.
- **Applies to:** the `Ctrl+Shift+P` command palette (DESIGN.md §10.1). This spec is the
  **language-agnostic behavioral contract** for the fuzzy matcher so any frontend
  (webview or native Rust) ranks and highlights identically.

## 1. Purpose

Given a query and the list of `$PATH` executables, rank the commands by relevance and
report which characters matched (for highlighting). The matcher must be *flexible*
(typing `grep` finds `egrep`, `git-grep`, …) without being *noisy* (scattered
letter-soup matches must not outrank real ones).

## 2. Inputs & normalization

- `commands: string[]` — executable names (from the palette enumeration service).
- `query: string` — the palette input.
- **Case-insensitive:** compare on lowercased forms of both. Highlight indices refer to
  the original-case command (same length, so indices align).

## 3. Tokenization

- Split `query` on runs of ASCII whitespace; drop empty tokens.
- **Every token must match** the command (logical AND). A command's score is the **sum**
  of its per-token scores; its highlight set is the **union** of per-token hit indices.
- Empty query (no tokens) → every command matches with score `0` and no hits (the palette
  shows the full list).

## 4. Per-token scoring (tiers)

For a command `cmd` and a single whitespace-free token `t` (both lowercased), evaluate the
**first** tier that applies. Higher score = better. A contiguous match must always beat a
scattered one, so every substring tier scores far above the subsequence tier.

| Tier | Condition | Score | Hit indices |
|------|-----------|-------|-------------|
| 1. Exact | `cmd == t` | `1000` | all of `[0, len(t))` |
| 2. Substring | `t` occurs in `cmd` at first index `s` | `200 - min(s, 100)`, then **+100 if `s == 0`** (prefix), **else +60 if `cmd[s-1]` is a word-boundary char** | the run `[s, s + len(t))` |
| 3. Subsequence | each char of `t` finds a later occurrence in `cmd` (greedy, first-fit) | see below | the matched indices |
| — | none of the above | **no match** (exclude command) | — |

**Word-boundary characters:** `-`  `_`  `.`  `/`  `@`  `+`  (so `grep` scores as a token
start in `git-grep`, `ast-grep`).

**Subsequence scoring (tier 3):** walk the token left to right; for each char find its
next occurrence in `cmd` at or after the running cursor.
- if not found → **no match** (return null for the whole command);
- `score -= (gap)` where gap is the distance skipped since the cursor (gap penalty);
- `+5` if this char is immediately adjacent to the previous matched char (contiguity);
- `+10` if this char matched at index `0` (prefix);
- advance the cursor past the matched index; record the index as a hit.

## 5. Aggregate score & ranking

1. For each command, `final = sum(token scores) - len(cmd) * 0.1` (mild preference for
   shorter names as a tiebreak). Exclude commands where any token failed to match.
2. Sort by `final` descending. (Ties may retain input order.)
3. Cap the visible results at **`PALETTE_MAX = 60`**.

## 6. Highlighting

Emphasize exactly the aggregated hit indices for each shown command — no re-derivation.
A substring hit therefore underlines the contiguous run; a subsequence hit underlines the
individual matched characters.

## 7. Worked examples

Query `grep` against a representative `$PATH` ranks (high → low):

```
grep(≈1000)  grepdiff(≈299, prefix)  ast-grep / git-grep(≈255, word-boundary)
egrep / fgrep / zgrep / pgrep / rgrep / bzgrep(≈198, substring)  …  gv2ray-proxy-helper(<0, subsequence)
```

Multi-token (space = AND):

```
git grep   → git-grep
doc comp   → docker-compose
```

## 8. Acceptance criteria

- `grep` returns and ranks the grep family (`grep`, `egrep`, `fgrep`, `zgrep`, `pgrep`,
  `rgrep`, `grepdiff`, `git-grep`, …) above any command that only matches as a scattered
  subsequence.
- Every substring match outranks every subsequence-only match for the same query.
- `git grep` matches `git-grep`; `doc comp` matches `docker-compose`.
- Empty query lists all commands (score 0).
- Matching is case-insensitive; highlight indices align with the displayed name.
- No more than `PALETTE_MAX` results are shown.
- The palette **only inserts** the chosen command at the prompt; it never auto-runs it.

## 9. Non-goals

- Ranking by usage/frequency or recency (could be a later enhancement).
- Typo tolerance / edit-distance matching beyond the subsequence fallback.
- Matching against descriptions/man summaries — this matches command **names** only.
