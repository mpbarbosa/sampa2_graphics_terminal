# Spec — AI window analysis (screenshot → review)

- **Status (native/Path C build):** **implemented.** `Ctrl+Shift+G` captures the app's own
  window to a PNG and asks the Claude API to review it, showing the reply in the AI card.
- **Applies to:** the native build specifically — the capture is a **wgpu framebuffer
  readback**, so it has no display-server dependency. The multimodal API call itself is the
  shared `sampa_ai::analyze` core (build-agnostic).

## 1. Purpose

Automates the "screenshot the app → ask Claude what's wrong → get fixes" loop by hand. On
the shortcut, Sampa grabs its own rendered window and sends it to the model with a prompt to
identify visual/UX issues and suggest a fix for each. The model returns **text** — it can't
edit the running app — so the result is **advisory**, shown read-only in the AI card.

## 2. Trigger & gating

- Bound to `keybindings.analyze_window`, default **`Ctrl+Shift+G`**, dispatched like the
  other actions.
- **Opt-in and consented, exactly like the text AI paths:** inert unless `[ai] enabled =
  true`; the API key is read from `ANTHROPIC_API_KEY` (never config); pressing the shortcut
  is the deliberate egress. A screenshot is a **heavier egress than a typed command** — it
  carries *whatever is on screen* (output, secrets, PII) — so it must never fire without the
  explicit keypress, and the image is never persisted or logged.

## 3. Capture

- Because capturing needs the GPU (which lives in the render path), the shortcut only
  **arms** the request (`pending_analyze`). The next `render_now` renders the frame to an
  **offscreen texture** (`RENDER_ATTACHMENT | COPY_SRC`) at the window size, copies it to a
  mapped buffer (256-byte row alignment), and encodes a **PNG** (`image` crate) — the same
  readback the CI `capture` uses. Surface `BGRA` is swapped to `RGBA` for the PNG.
- The capture omits the **AI card** (which is showing "Analyzing…"), so the shot is the
  terminal as it was. Self-contained: no `grim`/`scrot`, no capturing other windows.

## 4. Call & display

- The PNG is base64-encoded and sent on a background thread via `sampa_ai::analyze` — one
  `POST /v1/messages` with an **image content block + prompt** (Sampa's only network
  surface). A `gen` token drops stale replies.
- The AI card shows **Pending** during the call, then the model's prose in the read-only
  **`Explanation`** state (Esc/Enter dismiss). Errors (gated off, missing key, HTTP,
  refusal) surface in the card's `Error` state. **Nothing is applied** — the analysis is
  advisory; the developer acts on it.

## 5. Architecture mapping

- **Core** — `sampa_ai::analyze` / `analyze_over_network` + `AnalyzeRequest` (image + prompt
  → prose), git-pinned from the origin repo per ADR 0002. Pure, tested against the fake
  transport; the key never appears in the request body.
- **Frontend (`sampa-native`)** — `Gfx::paint_to_png` (offscreen render + readback),
  `analyze_window_open` (gate + arm), the `render_now` capture hook, `spawn_analyze`
  (base64 + background call), and `ai_analyze_ready` (deliver into the AI card). Reuses the
  existing `AiState` card — no new overlay.

## 6. Notes / follow-ups

- The result is text only; it cannot edit code. A future step could offer to copy the
  analysis or open it in an editor.
- The capture is the window's client area at its current size; multi-monitor / HiDPI scaling
  is whatever the surface reports.
