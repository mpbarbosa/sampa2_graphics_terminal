# AI command-suggester — manual sign-off checklist

Manual verification for the opt-in Claude command suggester
([spec-ai-overlay.md](spec-ai-overlay.md)). Separate from the
[N1 checklist](N1-manual-checklist.md) — this is a post-N4 optional feature, not part
of the N1 terminal-correctness gate.

Most of the flow can be checked **without a real API key** using a local endpoint (see
§F); only the live round-trip (§E) needs `ANTHROPIC_API_KEY` and a hosted-API opt-in.

## A. Off by default (no config)

- [ ] With no `[ai]` block in `config.toml`, press **Ctrl+Shift+A** — the overlay opens
      in the editing state showing `Ask AI ▸ ▉` and the egress warning.
- [ ] Type a request and press **Enter** — it does **not** call the network; the overlay
      shows the error `AI suggester is off — set [ai] enabled = true in config.toml.`
- [ ] **Esc** closes the overlay; the shell prompt is untouched.

## B. Gating & credentials (`[ai] enabled = true`, no key)

Set `[ai] enabled = true` in `$XDG_CONFIG_HOME/sampa2/config.toml`, launch from a shell
with **no** `ANTHROPIC_API_KEY` exported.

- [ ] Ctrl+Shift+A, type a request, Enter — the overlay shows
      `Set ANTHROPIC_API_KEY in your shell and relaunch Sampa.` and makes **no** request.
- [ ] Confirm the key never appears in `config.toml` or any log.

## C. Editing & overlay behavior

- [ ] The overlay **captures the keyboard**: typing edits the prompt, not the shell.
- [ ] Backspace edits the query; the block caret `▉` tracks the end.
- [ ] The egress warning ("Enter sends your text to the Claude API") is visible while editing.
- [ ] Only one overlay at a time: with the AI overlay open, the palette/man/preview
      keys don't stack a second overlay.

## D. Result rendering & insert-never-run

(Use a real key §E or the local endpoint §F to reach a result.)

- [ ] The header reads **Ask AI — suggestion** (accent color).
- [ ] The **command** is shown highlighted (accent, bold) on its own `$ …` line; the
      **explanation** is shown in italic; hints are muted.
- [ ] Press **Enter** — the command is inserted **at the shell prompt** and is **not
      executed** (no output, no new prompt line); the overlay closes.
- [ ] Press **c** on a result — the command is copied to the clipboard (paste elsewhere
      to confirm); nothing is executed.
- [ ] A stale reply is dropped: open, submit, **Esc** before it returns — the late result
      does not pop the overlay back open.

## E. Live API round-trip (needs a key)

Export `ANTHROPIC_API_KEY` in the launching shell (see
[ai-integration-feasibility.md §6](ai-integration-feasibility.md) for the env gotchas),
`[ai] enabled = true`, launch Sampa from that shell.

- [ ] "list files bigger than 100 MB" → a plausible `find … -size +100M` suggestion returns.
- [ ] A network failure (disconnect, or a bad `model`) surfaces as a readable error in the
      overlay — **no API key** appears in the message.
- [ ] Lower-latency model: set `model = "claude-haiku-4-5"` and confirm it still works.

## F. Local endpoint (no hosted egress)

Point `endpoint` at a local/OpenAI-compatible/proxy URL to keep data on the machine:

```toml
[ai]
enabled = true
endpoint = "http://127.0.0.1:PORT"
```

- [ ] With a local server returning a `{command, explanation}` Messages-shaped response,
      a request produces a Result exactly as in §D — no hosted-API traffic.

## G. Context redaction (`send_context = true`)

Set `send_context = true`. Put a fake secret on screen first, e.g.
`echo TOKEN=sk-live-EXAMPLE123abcDEF`.

- [ ] Trigger a suggestion and capture the outgoing request (local endpoint §F). The
      terminal context is attached but the secret appears as `TOKEN=‹redacted›` on **both**
      the command line and its echo output; the raw secret is **absent** from the body.
- [ ] A normal path / git SHA in the same context is **not** mangled.
- [ ] With `send_context = false` (default), no terminal context is attached at all.

---

**Sign-off:** the feature is manually verified when A–D and G pass (via §F if no key),
and E passes at least once against the hosted API.

- Tested: _____________  by: _____________
- Known issues: _____________
