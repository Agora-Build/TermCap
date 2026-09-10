# tcap shell integration for zsh.
#   eval "$(tcap init zsh)"
#
# Records what the terminal cannot tell us: the command text, its exit code,
# the working directory, how long it took, and — inside tmux — where the
# output starts and ends in the scrollback.

[[ -n "$_TCAP_LOADED" ]] && return 0
_TCAP_LOADED=1

# EPOCHREALTIME, for sub-second command durations.
zmodload zsh/datetime 2>/dev/null
autoload -Uz add-zsh-hook

_tcap_preexec() {
  # $1 is the command line as typed, before alias/history expansion.
  _TCAP_CMD=$1
  _TCAP_CWD=$PWD
  _TCAP_START=$EPOCHREALTIME
  _TCAP_ANCHOR=

  # Sample the cursor's absolute scrollback position before the command runs.
  # By the time preexec fires the terminal has already echoed the newline, so
  # the cursor sits on the first row of the command's output.
  #
  # `#{e|+:a,b}` is tmux format arithmetic: it gets both numbers and their sum
  # in one subprocess instead of two, which matters on every single prompt.
  if [[ -n "$TMUX" ]]; then
    _TCAP_ANCHOR=$(command tmux display-message -p -t "$TMUX_PANE" \
      '#{e|+:#{history_size},#{cursor_y}}' 2>/dev/null)
  fi
}

_tcap_precmd() {
  # MUST be the first line: anything else clobbers the exit status.
  local exit_code=$?

  # No command ran (e.g. an empty line, or the first prompt of the session).
  [[ -z "$_TCAP_CMD" ]] && return 0

  local -a args
  args=(--command "$_TCAP_CMD" --exit "$exit_code")
  [[ -n "$_TCAP_CWD"    ]] && args+=(--cwd "$_TCAP_CWD")
  [[ -n "$_TCAP_START"  ]] && args+=(--start-time "$_TCAP_START")
  [[ -n "$_TCAP_ANCHOR" ]] && args+=(--start-anchor "$_TCAP_ANCHOR")

  # The command text goes through as a single argv element, so quotes,
  # backslashes and newlines in it need no escaping.
  command tcap __record "${args[@]}" 2>/dev/null

  _TCAP_CMD=
}

add-zsh-hook preexec _tcap_preexec
add-zsh-hook precmd _tcap_precmd
