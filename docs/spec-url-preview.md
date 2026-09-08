# Spec — URL link-preview

- **Status:** implemented (reference: Tauri+xterm.js build — `crates/urlpreview`
  (`sampa-urlpreview`), the `preview_url` bridge command + `GuardedFetch`, the `[url_preview]`
  config, and the `#urlpreview` overlay in `src/main.ts`, bound to `keybindings.preview_url`).
  The native (sampa2) build is a follow-up.
- **Applies to:** previewing the content behind an `http(s)` URL — a Slack-style unfurl — inside
  the terminal, opened from the keyboard. Language-agnostic behavioral contract so any frontend
  (webview or native Rust) behaves identically.

## 1. Purpose

A URL in the scrollback (or that you're about to open) is opaque — you can't tell what's there
without leaving the terminal. Sampa fetches the page and shows a compact **unfurl**: title,
description, site name, a preview image, and a short text snippet. Purely informational —
nothing is composed or run.

Fetching a URL is **network egress**, so this is Sampa's **second deliberate network surface**
after the AI feature (§13). It is **opt-in and off by default**; when disabled, no URL is ever
fetched.

## 2. Trigger

- Bound to **`keybindings.preview_url`** (default **`Ctrl+Shift+L`** — *not* `Ctrl+Shift+U`, which
  a GTK/IME layer grabs for Unicode-codepoint entry), **not** auto-fired: the
  frontend detects a URL on the tracked line — a bare `http(s)://…` token, or the argument of a
  `curl`/`wget`/`open`/`xdg-open` command — and only fetches when the user presses the shortcut.
  Egress happens **only on that explicit keypress**, never on keystroke as-you-type.
- A modal overlay; **Esc** / its **✕** dismiss it. Any link inside the card is opened through the
  existing `open_url` confirm modal, never fetched implicitly.

## 3. Data

- On trigger the emulator calls `preview_url(url)`. It is **inert unless `[url_preview] enabled =
  true`**. The fetch runs off the async runtime (`spawn_blocking`) behind `GuardedFetch`, which
  enforces the security boundary the pure core cannot:
  - **http(s) only** — any other scheme (`file:`, `ftp:`, `data:`) is rejected up front.
  - **SSRF guard** — the host is resolved and the request refused unless **every** resolved
    address is globally routable (`sampa_urlpreview::ip_is_public`): loopback, RFC-1918 private,
    link-local (incl. the `169.254.169.254` cloud-metadata IP), CGNAT, ULA, multicast,
    unspecified, and reserved ranges are all rejected. Checked on **every redirect hop**.
  - **byte cap** (`max_bytes`, default 2 MB) — the body reader is truncated so a hostile server
    can't OOM the app.
  - **timeout** (`timeout_ms`, default 5 s) — connect + read.
  - **bounded, re-vetted redirects** (`max_redirects`, default 5) — auto-following is disabled;
    each `Location` is resolved and its host re-vetted before the next request.
- The core classifies the `Content-Type` and builds the `Preview`: HTML → unfurl (OpenGraph →
  Twitter-card → `<title>`/meta, body-snippet fallback); `text/plain` → a snippet; `image/*` →
  the URL for inline display; anything else → the bare URL + type. No cookies/credentials are
  sent. **Fails safe:** a fetch error surfaces as a message; a fetched-but-unparseable page is a
  best-effort card, never garbage.

## 4. Display

- Rendered as an unfurl **card**: title (bold), site name, description or text snippet, built
  entirely via DOM + `textContent` (the fetched page is untrusted). No HTML/CSS/JS from the page
  is ever rendered — the panel is text, never a browser.
- **The preview image loads inline on click, through the guard.** A remote `<img>` is never used
  — that would be a *second, unguarded* fetch that bypasses the SSRF guard and leaks the user's IP
  to the image host. Instead the card shows a "🖼 Show image" link; clicking it calls the
  `preview_image` bridge command, which fetches the bytes through the same `GuardedFetch` (SSRF /
  size / timeout / redirects), accepts only `image/*`, and returns a `data:` URI the webview
  renders with no request of its own. The load is deliberate — it happens only on that click.

## 5. Security & privacy

- **Opt-in egress.** Off by default; a preview is a deliberate, per-invocation user action, like
  the AI egress and the screenshot capture. Never auto-fetch.
- **SSRF is the load-bearing guard** — see §3. The IP-vet runs inside a custom `ureq::Resolver`
  (`GuardedResolver`), so ureq connects to **exactly** the addresses it vetted (and re-invokes it
  per redirect hop) — there is a single resolution used for both the vet and the connection, which
  **closes the DNS-rebind window** a separate pre-check would leave open. TLS still validates
  against the URL's hostname.
- **No credential leakage** — no cookies, auth headers, or `Referer`; the URL is treated as
  data. The fetched content is untrusted and only ever shown as inert text/image.
- **Bounded resources** — byte cap + timeout + redirect cap keep a hostile endpoint from hanging
  or exhausting the app.

## 6. Architecture mapping

- **`crates/urlpreview` (`sampa-urlpreview`)** — headless core. The impure fetch is behind a
  `Fetch` trait, so the parse/orchestration is unit-tested against a fake transport with **no
  network** (mirroring `sampa-ai`). `fetch_preview<F: Fetch>` builds the `Preview`; the pure SSRF
  predicates `http_host` / `ip_is_public` and the `resolve_url` resolver live here too (so CI
  tests them). Pure `std` + serde — no shell, no Tauri, no socket.
- **Config** — `[url_preview]` (`sampa_config::UrlPreview`): `enabled` (default false),
  `max_bytes`, `timeout_ms`, `max_redirects`.
- **Bridge** — `preview_url(url)` gates on `enabled`, then runs `GuardedFetch` (the **only** place
  the feature opens a socket) off the async runtime. `GuardedFetch` does the manual redirect loop
  + capped read; its `GuardedResolver` (a `ureq::Resolver`) does the DNS resolution + SSRF vet in
  one step so ureq connects to exactly the vetted IPs. `preview_image(url)` fetches a preview image
  through the same guard, accepts only `image/*`, and returns a `data:` URI for inline display. The
  pure predicates come from the core.
- **Frontend** — `keybindings.preview_url` (default `Ctrl+Shift+L`) detects a URL on the tracked
  line (`detectUrl`: a bare `http(s)` token or the arg of a `curl`/`wget`/`open`/`xdg-open`),
  calls `preview_url`, and renders the `#urlpreview` unfurl card (text via `textContent`; the
  preview image loads inline on click via `preview_image` → a `data:` URI, never a remote `<img>`
  — see §4).

## 7. Relationship to existing docs

Sits alongside the AI overlay (`sampa-ai`) as the project's **second** network surface, and reuses
its discipline: opt-in + off by default, the one-socket-place isolation, and a pure core tested
against a fake transport. Distinct from the `Ctrl+Shift+E` decorators and the man-panel
cheat-sheets, which are all **local** (subprocess/`/proc`) — this is the first *outbound* preview.
The escape-hardening rules (`textContent`, no page scripts) are the same as the man/preview panels.
