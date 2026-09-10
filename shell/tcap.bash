# tcap shell integration for bash.
#   eval "$(tcap init bash)"

[[ -n "$_TCAP_LOADED" ]] && return 0
_TCAP_LOADED=1

_tcap_begin() {
  _TCAP_CMD="$1"
  _TCAP_CWD="$PWD"
  # EPOCHREALTIME needs bash 5+. macOS ships bash 3.2, where `date` has no %N
  # either, so older shells fall back to whole seconds.
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

_tcap_finish() {
  local exit_code="$1"
  [[ -z "$_TCAP_CMD" ]] && return 0

  local -a args
  args=(--command "$_TCAP_CMD" --exit "$exit_code")
  [[ -n "$_TCAP_CWD"    ]] && args+=(--cwd "$_TCAP_CWD")
  [[ -n "$_TCAP_START"  ]] && args+=(--start-time "$_TCAP_START")
  [[ -n "$_TCAP_ANCHOR" ]] && args+=(--start-anchor "$_TCAP_ANCHOR")

  # The command goes through as a single argv element, so quotes, backslashes
  # and newlines in it need no escaping.
  command tcap __record "${args[@]}" 2>/dev/null

  _TCAP_CMD=
}

# bash-preexec, if present, already owns the DEBUG trap and multiplexes hooks
# through these arrays. Installing our own trap alongside it captures its
# internals (__bp_interactive_mode) instead of the user's command, so cooperate
# rather than compete. It ships with iTerm2 shell integration, Atuin and
# starship, so this path is common.
if [[ -n "${bash_preexec_imported:-}${__bp_imported:-}" ]]; then
  _tcap_preexec_hook() { _tcap_begin "$1"; }
  # bash-preexec restores $? before each precmd function, so this is the real
  # exit status of the command.
  _tcap_precmd_hook() { local ec=$?; _tcap_finish "$ec"; }

  preexec_functions+=(_tcap_preexec_hook)
  precmd_functions+=(_tcap_precmd_hook)
else
  _tcap_armed=0

  _tcap_debug() {
    [[ -n "$COMP_LINE" ]] && return 0
    # Skip prompt machinery so it is never mistaken for a user command.
    case "$BASH_COMMAND" in
      _tcap_*|__bp_*) return 0 ;;
    esac
    # A pipeline fires DEBUG per stage; only the first is the command as typed.
    (( _tcap_armed )) && return 0
    _tcap_armed=1

    # $BASH_COMMAND is only the current *simple* command, so `make; echo done`
    # would record just `make`. The history entry is the whole line as typed.
    local hist num line
    hist=$(HISTTIMEFORMAT= builtin history 1 2>/dev/null)
    [[ -n "$hist" ]] && builtin read -r num line <<< "$hist"

    _tcap_begin "${line:-$BASH_COMMAND}"
  }

  _tcap_precmd() {
    # MUST be first: anything else clobbers the exit status.
    local ec=$?
    _tcap_armed=0
    _tcap_finish "$ec"
  }

  # Prepend, so the status we read is the command's and not another hook's.
  case ";${PROMPT_COMMAND};" in
    *";_tcap_precmd;"*) ;;
    *) PROMPT_COMMAND="_tcap_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}" ;;
  esac

  # Armed last, so nothing above is caught by our own trap and recorded as if
  # the user had typed it.
  trap '_tcap_debug' DEBUG
fi
