# N1 exit-criterion — manual sign-off checklist

The headless smokes (`cargo test -- --ignored`) already prove rendering + the
SIGINT/SIGWINCH contracts for the installed program set. The items below are the
**interactive** parts that can only be verified by driving the live GUI window. Tick
each off in the actual `sampa` window; note anything that misbehaves.

## Launch

```bash
cargo run --release            # or: cargo run
```

A window titled **“Sampa (native) — N1”** should open running your `$SHELL`, with a
block cursor at the prompt.

## A. Rendering & resize

- [ ] Prompt renders with correct colors (your theme's prompt looks right).
- [ ] `ls --color=auto` (or `--color=always`) shows directory/file colors.
- [ ] `printf '\033[1mbold \033[3mitalic \033[4munderline \033[9mstrike\033[0m\n'` — each attribute is visible.
- [ ] **Resize the window** (drag an edge): text **reflows**, the prompt stays intact, no corruption or overrun. `stty size` reports the new dimensions.
- [ ] Very small and very wide sizes don't panic or garble.

## B. Full-screen apps (alt-screen)

- [ ] `vim README.md` — opens, `~` lines show, editing works; `:q` restores the shell screen (no leftover vim content).
- [ ] `htop` (if installed) — meters + process list render with color; `q` exits cleanly.
- [ ] `less /etc/services` — scroll with arrows/PageUp/PageDown; `q` exits.
- [ ] `tmux` (if installed) — status bar renders; a split (`Ctrl-b "`), then `exit`.

## C. Keyboard correctness

- [ ] In `vim`: arrows/Home/End/PgUp/PgDn move the cursor; F-keys and `Ctrl`/`Alt` chords behave.
- [ ] Backspace, Tab, and Enter work at the shell; `Ctrl-A`/`Ctrl-E` jump to line start/end.
- [ ] Type a UTF-8/emoji character (e.g. `é`, `→`) — it appears correctly.
- [ ] Application-cursor mode: inside `vim`, arrow keys still navigate (DECCKM path).

## D. Signals & job control (§3.2)

- [ ] Run `sleep 30`, press **Ctrl-C** — it stops and returns to the prompt.
- [ ] Run `cat` (no args, it blocks), press **Ctrl-D** — EOF returns to the prompt.
- [ ] Run `vim`, press **Ctrl-Z** — suspends to the shell; `fg` resumes it; `jobs` lists it.

## E. Mouse & selection

- [ ] **Click-drag** over text — a selection highlight appears.
- [ ] Release — the selection is copied; middle-click or **Ctrl-Shift-V** pastes it elsewhere; **Ctrl-Shift-C** also copies.
- [ ] Paste a **multi-line** clipboard payload — it appears as text and does **not** auto-execute (bracketed paste).
- [ ] In `htop`/`vim` (mouse mode on): clicking interacts with the app; holding **Shift** while dragging still selects locally.

## F. Scrollback

- [ ] `seq 1 200` then **scroll the wheel up** — earlier lines appear; **Shift+PageUp/PageDown** page through history.
- [ ] Start typing — the view **snaps back to the live prompt**.
- [ ] Inside `vim` (alt-screen), the wheel does **not** scroll scrollback.

## G. Clean exit

- [ ] `exit` (or Ctrl-D at the prompt) — the window closes.
- [ ] A non-zero exit (`exit 3`) is reported before close.

---

**Sign-off:** N1 is done when A–G are all ✔ against vim, and at least one of
htop/tmux, plus less. Record the date and any known issues here:

- Tested: _____________  by: _____________
- Known issues: _____________
