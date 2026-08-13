# Command preview — shell integration (OSC 133)

The command preview (`Ctrl+Shift+E`) and the man panel (`Ctrl+Shift+M`) read the command
**off the terminal grid** rather than reconstructing it from keystrokes, so paste, history
recall (↑), accepted autosuggestions (→), and Tab-completion all preview correctly.

To find *where* the command starts on the prompt line, Sampa uses two strategies, best
first:

1. **OSC 133 shell-integration markers (exact).** If your shell emits the FinalTerm/FTCS
   `OSC 133 ; B` "command-start" marker, Sampa records the exact cursor position and reads
   the command from there to the cursor — across wrapped lines, with **no prompt guessing**
   and regardless of what your prompt looks like.
2. **Prompt heuristic (fallback).** With no markers, Sampa strips a recognized prompt
   prefix (`❯ ➜ » ▶`, or `$ # %`) from the cursor row. Works for common prompts; can be
   fooled by exotic ones (a prompt with no recognizable sigil, or a marker char inside it).

Both need no configuration to *work* — but enabling OSC 133 makes the preview exact.

## Enabling OSC 133

### Powerlevel10k (recommended — you already use it)

Add to your `~/.config/zsh/.zshrc` (or wherever `$ZDOTDIR/.zshrc` lives), **before** the
`p10k.zsh` source line:

```zsh
POWERLEVEL9K_TERM_SHELL_INTEGRATION=true
```

Reload (`exec zsh`) or open a new tab. That's it — p10k emits the OSC 133 A/B/C/D markers.

### Plain zsh (no framework)

Emit the markers from the prompt and a `preexec` hook:

```zsh
# Prompt-start (A) and command-start (B) markers wrapped as non-printing (%{ %}).
PROMPT=$'%{\e]133;A\e\\%}'"$PROMPT"$'%{\e]133;B\e\\%}'
# Command-executed (C) right before the command runs.
preexec() { print -n $'\e]133;C\e\\' }
```

### bash

Use [ble.sh] or the common `PROMPT_COMMAND` snippet that emits `\e]133;A`…`\e]133;B`, or
any existing shell-integration script (iTerm2's, VS Code's) — they all speak OSC 133.

## Verifying

Open the preview (`Ctrl+Shift+E`) and **paste** a command, or recall one with ↑. The
header should show `preview ✓ <your command>` with its output. With OSC 133 on, this holds
even for prompts the heuristic can't parse.

## Limitations (current)

- The grid read is **single-region**: a command that wraps is handled with OSC 133 (it
  spans the recorded start → cursor), but the heuristic fallback reads only the cursor row.
- A stale marker (e.g. background output scrolled the prompt off-screen mid-edit) falls
  back to the heuristic automatically.
