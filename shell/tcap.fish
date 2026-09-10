# tcap shell integration for fish.
#   tcap init fish | source
#
# fish has first-class events for this, so no trap or latch is needed.

if set -q _TCAP_LOADED
    exit 0
end
set -g _TCAP_LOADED 1

function _tcap_preexec --on-event fish_preexec
    set -g _TCAP_CMD $argv[1]
    set -g _TCAP_CWD $PWD
    set -g _TCAP_START (command date +%s)
    set -e _TCAP_ANCHOR

    # See tcap.zsh for why this is sampled here and what the sum means.
    if set -q TMUX
        set -g _TCAP_ANCHOR (command tmux display-message -p -t "$TMUX_PANE" \
            '#{e|+:#{history_size},#{cursor_y}}' 2>/dev/null)
    end
end

function _tcap_postexec --on-event fish_postexec
    # $status is the just-finished command's; read it before anything else.
    set -l exit_code $status

    if not set -q _TCAP_CMD
        return
    end

    set -l args --command "$_TCAP_CMD" --exit "$exit_code"
    if set -q _TCAP_CWD
        set -a args --cwd "$_TCAP_CWD"
    end
    if set -q _TCAP_START
        set -a args --start-time "$_TCAP_START"
    end
    if set -q _TCAP_ANCHOR
        set -a args --start-anchor "$_TCAP_ANCHOR"
    end

    command tcap __record $args 2>/dev/null

    set -e _TCAP_CMD
end
