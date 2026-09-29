# Sampa terminal — zsh shell integration (OSC 7 cwd + OSC 133 semantic prompt).
#
# Enable it from your ~/.zshrc, either by evaluating the binary's copy:
#   eval "$(sampa2 --shell-integration zsh)"
# or by sourcing an installed / checked-out file:
#   [[ -r /usr/share/sampa/sampa.zsh ]] && source /usr/share/sampa/sampa.zsh
#   source /path/to/sampa/shell-integration/sampa.zsh
#
# It makes the command palette / man panel / preview features robust by giving the
# terminal exact prompt boundaries and the working directory. It is a no-op outside
# Sampa and safe to source unconditionally.

[[ "$TERM_PROGRAM" == "sampa-terminal" ]] || return 0
[[ -o interactive ]] || return 0

# OSC 7: report the cwd as a file:// URL.
__sampa_osc7() {
  printf '\e]7;file://%s%s\e\\' "${HOST}" "${PWD}"
}

# OSC 133 marks.
__sampa_precmd() {
  local ret=$?
  # D: previous command finished (with its exit code). A: a new prompt starts.
  printf '\e]133;D;%s\e\\' "$ret"
  __sampa_osc7
  printf '\e]133;A\e\\'
}
__sampa_preexec() {
  # C: the command is about to run; its output begins here.
  printf '\e]133;C\e\\'
}

autoload -Uz add-zsh-hook
add-zsh-hook precmd __sampa_precmd
add-zsh-hook preexec __sampa_preexec

# B: end of prompt / start of command input. Appended zero-width to the prompt.
PROMPT="${PROMPT}%{"$'\e]133;B\e\\'"%}"
