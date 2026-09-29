# Performance — VT ingest throughput (N6)

> "The native ceiling is the payoff." — [PLAN.md](PLAN.md) §N6

Going native drops the webview compositor out of the paint path, so the terminal's ceiling is
now set by our own VT engine and renderer rather than by xterm.js in a WebView. This page
documents the **VT ingest** benchmark: the parse-and-grid hot path — `Processor::advance` into
the `alacritty_terminal` `Term` — which is the throughput of `cat`-ing a large file **minus**
GPU present. (Present is vsync-bounded and measured separately; ingest is the CPU-side ceiling
that a fast `cat`/build-log dump actually hits.)

## Running it

```bash
cargo run --release -- --bench             # default 50 MiB workload
cargo run --release -- --bench 100         # 100 MiB
cargo run --release -- --bench 50 \
  --baseline 85 --out metrics.json         # print a %Δ trend vs 85 MiB/s; write metrics JSON
```

`--baseline <MiB/s>` adds a trend line comparing this run to an earlier throughput, and
`--out <file>` writes the metrics as one JSON line (`mib_per_s`, `lines_per_s`, `rss_kib`, …).
Both are what the CI trend job uses (below); neither changes the measurement.

It builds a **deterministic** workload representative of real output — plain text, `ls
--color`-style SGR runs, log lines, a compiler error, and mixed-width UTF-8 (accents / CJK /
emoji) — feeds it through an 80×24 grid with a 100k-line scrollback in 64 KiB chunks (like the
real reader thread), and reports throughput, line rate, and the resident-memory delta.

`--bench` is release-only in spirit: a debug build is ~10× slower and not a meaningful number.

## Baseline (record trend, don't gate)

Measured on the development machine (Linux, release build). These are **reference numbers to
watch for regressions**, not a hard gate — hardware varies:

| Workload | Elapsed | Throughput | Line rate | RSS delta |
|---|---|---|---|---|
| 50 MiB  | ~0.61 s | ~82 MiB/s | ~1.17 M lines/s | ~193 MiB |
| 100 MiB | ~1.15 s | ~87 MiB/s | ~1.24 M lines/s | ~193 MiB |

Notes:

- **RSS plateaus** across the 50/100 MiB runs because the 100k-line scrollback fills in both;
  the delta (~193 MiB for 100k × 80 styled cells ≈ 24 B/cell) is the **scrollback-memory**
  figure, stable regardless of how much more is streamed through it.
- Throughput is dominated by the escape parser + grid writes + scroll; SGR-heavy content is
  slower than plain text, which is why the workload mixes both.

## What this does and doesn't cover

- **Covers:** the CPU ingest ceiling — parser, grid mutation, scrollback eviction. This is the
  number that moves when we touch the VT seam, so it belongs in the trend line.
- **Doesn't cover:** GPU present latency (vsync-bounded; the payoff of dropping the webview is
  *added-input-latency < one frame*, measured with a typometer against the live window), and
  glyph shaping/atlas upload (amortized by the render cache). Those are separate metrics.

## Wiring into CI (trend, non-gating)

The `bench` job in [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) **trends** this
rather than gating on it:

1. It runs `sampa2 --bench 50 --out perf-metrics.json` on `ubuntu-latest` and uploads
   `perf-metrics.json` as an artifact every run.
2. Before benching, it downloads the most recent **successful run on the base branch** (same
   runner class, so the numbers are comparable) and passes that run's `mib_per_s` as
   `--baseline`. The run Summary then shows the signed %Δ.
3. A drop of more than **10 %** is marked `⚠ regression (advisory)` — printed, never failed on.
   The job carries no `-D`/exit-on-regression; it is a signal, not a gate.

Because the baseline is the previous CI run (not a number baked into the repo), the trend
follows the runner's own hardware and needs no manual upkeep. The **first** run on a branch —
and a fork PR whose token can't read Actions — simply has no baseline and skips the trend line.
The `~82–87 MiB/s` figure above is the local reference; CI's absolute numbers depend on the
runner and are meaningful **relative to the previous run**, which is the point of a trend.
