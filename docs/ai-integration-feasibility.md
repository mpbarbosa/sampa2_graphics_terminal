# Feasibility — integrating the shell with the Claude API

> **Mirrored from the `sampa_graphics_terminal` reference build**, where this is
> **implemented** (the plan in §5 was approved and built): `crates/ai` (`sampa-ai`), the
> `suggest_command` bridge command, the `Ctrl+Shift+A` overlay, and the `[ai]` config
> block. It is carried here as the design rationale for *why* the feature is opt-in and how
> the guardrails are shaped; the file/keybinding references describe that build. If/when the
> native build grows the same feature, the headless-service + insert-never-run + env-key
> shape applies unchanged.

**Question:** can Sampa integrate with the Claude (Anthropic) API — e.g. describe a
command's output and get back the command line that produces it?

**Short answer:** Technically yes, and it fits Sampa's existing *insert-never-run* UX and
headless-core seam cleanly. But it crosses the one line the project drew on purpose —
**zero network surface** (DESIGN.md §13; "features are in-process function calls, never
ports") — and it sends terminal content to a third party. So it's feasible as a strictly
**opt-in, disabled-by-default, key-gated, consented** feature, not as a v1 default.

---

## 1. What the feature is

Two directions, both a **single `POST /v1/messages` call**:

- **NL → command** (the common case): "list files bigger than 100 MB" → `find . -size +100M`.
- **Output → command** (the asked example): paste/point at some output → the command that
  likely produced it. A reverse/"explain" variant of the same call.

In both, the result is a **suggested command line**, plus a short explanation.

## 2. How it fits the architecture

It maps onto patterns Sampa already has, so the renderer-agnostic core stays clean:

- **New headless crate `crates/ai` (`sampa-ai`)** — GUI-free, like the other service crates.
  Rust has no official Anthropic SDK, so it calls the Messages API over **raw HTTPS** —
  *as built*, via **`ureq`** (lighter than `reqwest` for a single blocking POST): `POST
  https://api.anthropic.com/v1/messages`, headers `x-api-key` + `anthropic-version:
  2023-06-01`. The request-build/response-parse core is pure and tested against a fake
  **`Transport` trait** (7 tests, **no network**); the real `UreqTransport` is the sole
  socket. **No Tauri imports** — same rule as `pty-core`/`config`/`preview`.
- **Bridge command** `suggest_command(prompt, context) -> { command, explanation }`
  (`src-tauri/src/lib.rs`). Gates on `[ai] enabled` + the env key, then runs the blocking
  call off the async runtime via `tokio::task::spawn_blocking`.
- **Structured output** — send `output_config.format` with a `json_schema` for
  `{ command: string, explanation: string }` so parsing is guaranteed (supported on
  `claude-opus-5`, `claude-sonnet-5`, `claude-haiku-4-5`). *As built*, `max_tokens = 2048`
  (headroom for thinking-on models like `claude-opus-5`); no assistant prefill (removed on
  current models).
- **Frontend UX = the palette pattern.** An overlay (`Ctrl+Shift+A`) takes a
  natural-language prompt; the returned command is **inserted at the prompt, never
  auto-run** — exactly the §10.1 / §13 boundary the command palette already honors. If the
  user then hits preview (`Ctrl+Shift+R`), the existing `sampa_preview::classify` gate still
  applies. The service stays in the core; only the overlay lives in `src/main.ts`.
- **Config** — a new `[ai]` block, **off by default**: `enabled = false`, `model`
  (default `claude-opus-5`; `claude-haiku-4-5`/`claude-sonnet-5` for lower latency/cost),
  `endpoint` (override for a proxy or a **local** model), and *as built* a
  **`send_context` bool** (§4) — off by default, gating whether recent output/cwd is attached.

## 3. The real issue — it breaks "zero network surface"

Everything above is straightforward. The weight of this decision is the security/privacy
posture, because Sampa deliberately has **no outbound network today**:

1. **Network egress.** This is the first feature that opens a socket to the outside world.
   It must be **opt-in** (`[ai] enabled` default `false`) and every request is a "send
   content to an external service" action, because sending publishes. *As built*, the
   consent is the overlay itself — it carries an explicit egress warning ("Enter sends your
   text to the Claude API") and pressing Enter is the deliberate send, rather than a
   separate confirm-modal per request.
2. **The API key is a secret.** Never in the repo, never in `config.toml`. Read it from the
   environment (`ANTHROPIC_API_KEY`); the app never stores or prompts for it in plaintext.
   (Handling credentials in-app is exactly the class of action Sampa should push back to the
   user.)
3. **Terminal content is sensitive.** The description — and any context you attach (recent
   output, cwd, shell) — can contain tokens, keys, PII, hostnames. Default to sending the
   **least** possible: the user's typed request only. Attaching scrollback/output should be
   explicit (a keybinding or a selection), never automatic, and worth a redaction pass for
   obvious secret shapes before egress. This is the inverse of the preview gate: preview
   keeps data *local*; this feature is the one place data leaves.
4. **It stays advisory.** Reuse the insert-never-run rule: the model's output is a
   suggestion placed at the prompt. The security boundary that a typed `rm` is
   filesystem-verified-never-run (§13) is unchanged — the AI never executes anything.

## 4. Trade-offs

**Gains**
- A genuinely useful NL→command / explain-output capability, riding UX Sampa already has.
- Clean architectural fit — a headless service behind the existing seam; no core rewrite.

**Costs / risks**
- Opens the project's first network surface; adds an API-key dependency and a per-request
  cost + latency (a network round-trip vs. the palette's instant local filter).
- Sends terminal content to a third party — the privacy surface has to be designed, not
  bolted on.
- Degrades offline; adds an external dependency to a tool whose whole point is the local
  shell.

**Privacy-preserving alternative worth calling out:** because the Messages API shape is
simple and the `endpoint` is configurable, the *same* `crates/ai` can point at a **local
model** (e.g. an Ollama/OpenAI-compatible endpoint, or a self-hosted proxy). That keeps the
"nothing leaves the machine" posture while still delivering NL→command — a strong default
for privacy-sensitive users, with the hosted Claude API as an opt-in upgrade.

## 5. Recommendation

Feasible and a natural feature — **but ship it opt-in, not as a v1 default.** Concretely:

1. A headless `crates/ai` service (raw HTTPS via `ureq` to `POST /v1/messages`, structured
   `{command, explanation}` output, model + endpoint from config), unit-tested against a
   fake `Transport`. No GUI imports.
2. Off by default (`[ai] enabled = false`); API key from the environment, never stored.
3. Minimal, **consented** context — typed request only unless the user opts into
   `send_context`; the overlay's egress warning + Enter is the consent.
4. Result **inserted at the prompt, never auto-run** (reuse the §10.1/§13 boundary); the
   preview gate still governs any subsequent run.
5. Make `endpoint` configurable so a **local model** preserves the zero-third-party-egress
   posture for users who want the feature without the network trade-off.

Treat it as an M6/post-v1 optional capability. It is the first feature that trades Sampa's
zero-network-surface guarantee for functionality, so the guardrails above are the feature —
the API call itself is the easy part.

## 6. As-built: enabling it & the credential-wiring gotchas

Getting the feature *running* turned out to be entirely about the environment, since the
key deliberately lives outside the app. The traps below cost the most time in practice — a
key concern anyone enabling this will hit:

- **The backend reads the key from the *process* environment**, not from a tab's shell.
  `suggest_command` calls `std::env::var("ANTHROPIC_API_KEY")` in the Rust process, whose
  environment is fixed **at launch** from the shell that started Sampa. Sourcing an env file
  *inside* a running Sampa tab does nothing for the backend. **Launch Sampa from a shell
  that already has the key exported.**
- **The variable must be `export`ed, not just set.** A bare `ANTHROPIC_API_KEY=…` line in a
  sourced file is a *shell-local* variable that child processes don't inherit — `echo
  "${#ANTHROPIC_API_KEY}"` shows it, but `env | grep` (and the launched app) don't. Use
  `export ANTHROPIC_API_KEY=…` (or `set -a; source file; set +a`). Verify with
  `env | grep -c ANTHROPIC_API_KEY` (should be `1`), not `echo` — `env` lists exported vars only.
- **`ZDOTDIR` decides which rc file zsh reads.** If it's set (common with powerlevel10k),
  `~/.zshrc` is ignored and `$ZDOTDIR/.zshrc` is the real file. Put the source line there.
- **A running process can't pick up a later export** — after fixing any of the above, fully
  relaunch Sampa from a fresh shell.

Recommended shape: keep the secret in a private `~/.config/sampa/sampa.env` (`chmod 600`,
`export ANTHROPIC_API_KEY=…`, outside any repo), sourced from the correct zsh rc so every
new shell — and any Sampa launched from it — has it. **The key never goes in `config.toml`
or the repo.** A key pasted anywhere shared (chat, a tracked dotfile) should be considered
burned — rotate it.

## 7. As-built: the second direction — explain an existing command

§1 framed two *command-producing* directions (NL→command, output→command). A third,
**command → description**, now ships alongside them and rides the exact same machinery
with the polarity flipped: instead of turning a request *into* a command, it takes a
command the user has **already typed** and returns a plain-language description of what it
does.

- **Trigger.** A dedicated keybinding, `keybindings.explain` (default **`Ctrl+Shift+X`**).
  It reads the current command line from the **tracked keystrokes** (`tab.typed`) — the
  same keystroke source the man panel and preview use — so it never scrapes the grid.
- **Core.** `sampa_ai::explain` (+ `explain_over_network`) builds one `POST /v1/messages`
  with an `ExplainRequest { command, os, shell }`. Unlike `suggest`, it asks for **free
  prose**, not a `json_schema` — an explanation is naturally prose — and `parse_text_response`
  returns the first text block. The system prompt instructs the model to lead with the
  overall effect and to **call out destructive/irreversible commands** prominently. Same
  `Transport` seam, so it is unit-tested with a fake transport; the key is never in the body.
- **Bridge.** `explain_command(command)` — the same gate as `suggest_command` (inert unless
  `[ai] enabled`; key from `ANTHROPIC_API_KEY`), the same `spawn_blocking` off the async
  runtime.
- **UX.** The result appears in a **read-only popup** (`#explain`) showing the command and
  its description. **Nothing is executed** — this direction has no command to insert, so the
  insert-never-run boundary is moot here; the only side effect is the network call itself.
- **Consent.** There is no separate egress modal: **pressing the shortcut *is* the deliberate
  send** (the typed command line leaves the machine). This mirrors the suggester overlay's
  "Enter is the send" model. As with the suggester, `[ai] endpoint` can point at a local
  model to avoid third-party egress entirely.

Everything from §3 (secret handling), §5 (opt-in, local-model escape hatch), and §6
(credential wiring) applies unchanged — this is the same one network surface, used in a new
direction, not a second one.

- **Native (Path C) — implemented.** `Ctrl+Shift+X` (`keybindings.explain`) reads the current
  command line via `grid_command_line` (the native command source the man panel/preview use),
  calls `sampa_ai::explain_over_network` on a background thread with `ExplainRequest { command,
  os, shell }`, and renders the reply in the shared AI card as a new read-only `Explanation`
  state (command + prose, `c` copies, `Esc` closes — no insert). Same gates as the suggester:
  inert unless `[ai] enabled`, key from `ANTHROPIC_API_KEY`, superseded by the `ai_gen` token.
