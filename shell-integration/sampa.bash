# Sampa terminal — bash shell integration (OSC 7 cwd + OSC 133 semantic prompt).
#
# Enable it from your ~/.bashrc, either by evaluating the binary's copy:
#   eval "$(sampa2 --shell-integration bash)"
# or by sourcing an installed / checked-out file:
#   [[ -r /usr/share/sampa/sampa.bash ]] && source /usr/share/sampa/sampa.bash
#
# No-op outside Sampa and safe to source unconditionally. Bash lacks zsh's precmd/
# preexec hooks, so this uses PROMPT_COMMAND (precmd) and a DEBUG trap (preexec);
# the DEBUG-trap `C` mark is best-effort.

[[ "$TERM_PROGRAM" == "sampa-terminal" ]] || return 0
case "$-" in *i*) ;; *) return 0 ;; esac  # interactive only

__sampa_osc7() {
  printf '\e]7;file://%s%s\e\\' "${HOSTNAME}" "${PWD}"
}

# precmd: D (previous exit) + cwd + A (prompt start). Runs via PROMPT_COMMAND.
__sampa_precmd() {
  local ret=$?
  printf '\e]133;D;%s\e\\' "$ret"
  __sampa_osc7
  printf '\e]133;A\e\\'
  __sampa_preexec_armed=1
}

# preexec: C (output start). The DEBUG trap fires before every command; we gate it
# so it only fires once per prompt (for the user's command, not for PROMPT_COMMAND).
__sampa_preexec_armed=0
__sampa_debug() {
  [[ -n "$COMP_LINE" ]] && return                 # skip during completion
  [[ "$BASH_COMMAND" == "__sampa_precmd" ]] && return
  if [[ "$__sampa_preexec_armed" == 1 ]]; then
    __sampa_preexec_armed=0
    printf '\e]133;C\e\\'
  fi
}
trap '__sampa_debug' DEBUG

case "$PROMPT_COMMAND" in
  *__sampa_precmd*) ;;
  *) PROMPT_COMMAND="__sampa_precmd${PROMPT_COMMAND:+; $PROMPT_COMMAND}" ;;
esac

# B: end of prompt / start of command input.
PS1="${PS1}\[\e]133;B\e\\\\\]"
