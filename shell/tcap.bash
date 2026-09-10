# tcap shell integration for bash.
#   eval "$(tcap init bash)"
#
# bash has no preexec hook, so this uses the standard trap-DEBUG technique:
# DEBUG fires before every simple command, and a latch keeps us to the first
# one of each prompt cycle.

[[ -n "$_TCAP_LOADED" ]] && return 0
_TCAP_LOADED=1

_tcap_armed=0

_tcap_debug() {
  # Skip completion machinery and our own bookkeeping.
  [[ -n "$COMP_LINE" ]] && return 0
  [[ "$BASH_COMMAND" == _tcap_precmd* ]] && return 0
  # Only the first command of a prompt cycle; a pipeline fires DEBUG per stage.
  (( _tcap_armed )) && return 0
  _tcap_armed=1

  _TCAP_CMD="$BASH_COMMAND"
  _TCAP_CWD="$PWD"
  # EPOCHREALTIME needs bash 5+. macOS ships bash 3.2, where `date` has no %N
  # either, so older shells fall back to whole seconds and lose sub-second
  # precision in the reported duration.
  if [[ -n "${EPOCHREALTIME:-}" ]]; then
    _TCAP_START="$EPOCHREALTIME"
  else
    _TCAP_START="$(command date +%s)"
  fi

  _TCAP_ANCHOR=
  if [[ -n "$TMUX" ]]; then
    _TCAP_ANCHOR=$(command tmux display-message -p -t "$TMUX_PANE" \
      '#{e|+:#{history_size},#{cursor_y}}' 2>/dev/null)
  fi
}

_tcap_precmd() {
  # MUST be first: anything else clobbers the exit status.
  local exit_code=$?

  _tcap_armed=0
  [[ -z "$_TCAP_CMD" ]] && return 0

  local -a args
  args=(--command "$_TCAP_CMD" --exit "$exit_code")
  [[ -n "$_TCAP_CWD"    ]] && args+=(--cwd "$_TCAP_CWD")
  [[ -n "$_TCAP_START"  ]] && args+=(--start-time "$_TCAP_START")
  [[ -n "$_TCAP_ANCHOR" ]] && args+=(--start-anchor "$_TCAP_ANCHOR")

  command tcap __record "${args[@]}" 2>/dev/null

  _TCAP_CMD=
}

trap '_tcap_debug' DEBUG

# Prepend, so the exit status we read is the command's and not another hook's.
case ";${PROMPT_COMMAND};" in
  *";_tcap_precmd;"*) ;;
  *) PROMPT_COMMAND="_tcap_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}" ;;
esac
