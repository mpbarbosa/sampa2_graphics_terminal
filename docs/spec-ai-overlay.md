# Spec — AI command-suggester overlay

> **Provenance.** The behavioral contract and guardrails come from
> [`ai-integration-feasibility.md`](ai-integration-feasibility.md) (mirrored from the
> reference [`sampa_graphics_terminal`](https://github.com/mpbarbosa/sampa_graphics_terminal)
> Tauri build, where the feature is implemented as a `src-tauri` bridge command + a
> `src/main.ts` overlay). This spec **retargets that design to the native Rust build**
> (`crates/sampa-native`): there is no webview and no IPC bridge, so the "backend" is an
> in-process background thread and the overlay is drawn by the native renderer. The
> headless [`sampa-ai`](https://github.com/mpbarbosa/sampa_graphics_terminal) crate and
> the `[ai]` config block are **reused unchanged** from the shared core (ADR 0002).

- **Status:** implemented through Phase 3. Phase 0–1 (deps, config surface, keybinding,
  background call, gating) + Phase 2 chrome (accent header, **highlighted command**,
  italic explanation, muted hints, **`c` copies**) + Phase 3 (`send_context` **redaction**
  in `sampa-ai`, applied to context before egress). See [PLAN.md](PLAN.md).
- **A note on "centered box":** the renderer clips the grid behind an overlay with a
  single bounds rect, so every overlay is a **full-width top/bottom band** — a floating
  centered card would need a grid clip with a hole (a larger renderer change). The AI
  overlay is therefore a styled bottom band, not a centered modal. Deferred, not lost.
- **Applies to:** a `Ctrl+Shift+A` overlay that turns a natural-language request into a
  **suggested** shell command, inserted at the prompt and **never auto-run**.

## 1. Purpose

One `POST /v1/messages` call, two directions of the same feature:

- **NL → command** (common): "list files bigger than 100 MB" → `find . -size +100M`.
- **Output → command**: describe some output, get the command that likely produced it.

The result is always a **suggested command line + a one-line explanation**. It is placed
at the shell prompt for the user to review, edit, and run — the model never executes
anything. This is the same **insert-never-run** boundary the command palette honors
(DESIGN §10.1 / §13).

## 2. Trigger & lifecycle

- Bound to `keybindings.ai`, **default `Ctrl+Shift+A`** (native hand-parser + `ACTIONS`
  table; the shared `sampa-config` also carries this default).
- The binding **toggles** the overlay; **Esc** also closes it. Only one modal overlay is
  open at a time (help / man / palette / search / ai are mutually exclusive).
- The overlay owns the keyboard while open — keys edit the prompt or drive the overlay,
  and never reach the PTY.

## 3. States

The overlay is a small state machine (`AiState`):

| State | What's shown | Enter does |
|---|---|---|
| **Editing** | the typed prompt + the egress warning | submit → **Pending** (gated, §5) |
| **Pending** | "Contacting the Claude API…" | nothing (request in flight) |
| **Result** | `command` (highlighted) + `explanation` | **insert `command` at the prompt**, close |
| **Error** | a safe, key-free message (§5) | return to **Editing** to retry |

- **Backspace/text** edit the prompt only in **Editing**.
- In **Result**, **`c`** copies the suggested command to the clipboard (no execution).
- A **generation counter** (`ai_gen`) guards against stale responses: a response whose
  gen ≠ the current gen is dropped (same idiom as the command preview).

## 4. The egress consent (this is the feature)

Sampa has **zero outbound network today** (DESIGN §13). This overlay is the *only* place
that changes, so the consent must be explicit and per-send:

- The overlay carries a visible warning: **"Enter sends your text to the Claude API."**
  Pressing Enter in **Editing** *is* the deliberate send — no request is ever made just
  by opening the overlay.
- **Least-data default.** Only the typed prompt is sent. Recent output/cwd is attached
  **only** when `[ai] send_context = true` (off by default), because terminal content can
  contain secrets and this is the one place data leaves the machine.
- **Redaction (defense in depth).** When context *is* attached, `sampa-ai::redact` masks
  obvious secret shapes before egress — secret-named assignments (`TOKEN`/`SECRET`/
  `PASSWORD`/…, `Authorization:` headers), known credential prefixes (`sk-`, `ghp_`,
  `AKIA…`, `xox…`, …), PEM private-key blocks, and long high-entropy tokens. It is
  conservative (ordinary output, hex digests, and git SHAs are left intact) and is a
  safety net, not a substitute for `send_context` defaulting off.
- **Advisory only.** The result is inserted, never run. Any subsequent run still passes
  the existing preview/classify gate.

## 5. Gating & credentials

On Enter in **Editing**, in order:

1. `[ai] enabled` must be `true` (default `false`) — else **Error**: "AI suggester is off
   (set `[ai] enabled = true`)."
2. The API key is read from the **process** environment: `ANTHROPIC_API_KEY`. Missing →
   **Error**: "Set ANTHROPIC_API_KEY and relaunch." The app never stores, prompts for, or
   logs the key, and it never appears in `config.toml` or the repo.
3. Otherwise: build a `sampa_ai::Params` (model + endpoint from `[ai]`) and `Request`
   (prompt, os = `linux`, shell from `$SHELL`, `context` only if `send_context`), then run
   `sampa_ai::suggest_over_network` on a **background thread** (blocking `ureq` POST must
   not block the winit UI thread), posting `UserEvent::AiReady { gen, result }` back via
   the event-loop proxy — the same pattern as `ManReady` / `PreviewReady`.

Errors surfaced to the user are the crate's `AiError` `Display` strings, which are
constructed to never echo the key.

## 6. Rendering

Renders through the bottom **`PanelView`** band (shared with man/preview) — no new GPU
pass. `PanelView.body_spans` lets the AI overlay supply **colored spans** while man/preview
keep their single-color body:

- **header** (accent) = "Ask AI", "Ask AI — suggestion", or "Ask AI — error".
- **body**, state-dependent:
  - *Editing* — `▸ {query}▉` then the egress warning (muted).
  - *Pending* — "Contacting the Claude API…" (italic).
  - *Result* — the `explanation` (italic), then the **`command` in the accent color,
    bold**, then "Enter inserts it (never runs it) · c copies · Esc" (muted).
  - *Error* — the message (warn color) + a retry hint (muted).

The `command`/`explanation` colors and the copy affordance are the Phase 2 upgrade; the
contract in §2–§5 is unchanged. A future centered-card treatment stays possible (see the
status note) but needs a renderer change, not just this overlay.

## 7. Config

```toml
[ai]                # opt-in Claude-API command suggester — Sampa's ONLY network surface (§13)
enabled = false     # master switch; false = inert, no request is ever made
model = "claude-opus-5"          # or "claude-haiku-4-5" / "claude-sonnet-5" for lower latency/cost
endpoint = "https://api.anthropic.com/v1/messages"   # point at a local/proxy URL to keep data on-device
send_context = false             # attach recent output/cwd? off — that data would leave the machine

[keybindings]
ai = "Ctrl+Shift+A"
```

The key lives **outside** the app — keep it in a private `~/.config/sampa/sampa.env`
(`chmod 600`, `export ANTHROPIC_API_KEY=…`) sourced from your shell rc, and **launch
Sampa from a shell that already has it exported** (a running process can't pick up a later
export). See [ai-integration-feasibility.md §6](ai-integration-feasibility.md) for the
credential-wiring gotchas.

## 8. Non-goals (this spec)

- No streaming, no multi-turn chat, no history. One request → one suggestion.
- No in-app key entry or storage (that class of action stays with the user).
- No automatic context capture beyond the opt-in `send_context` toggle.
