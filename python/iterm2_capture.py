#!/usr/bin/env python3
"""Capture the last command's output from iTerm2 via its Python API.

Invoked by tcap's iterm2 backend. Prints a single JSON object on stdout:

    {"output": str, "command": str|null, "exit_code": int|null, "cwd": str|null}

Exit codes are meaningful so `tcap doctor` can distinguish setup problems:

    0  success
    3  the `iterm2` Python package is not installed
    4  connected, but no prompt info (iTerm2 Shell Integration missing)
    5  could not connect / authenticate to the iTerm2 API
    1  anything else

Requires iTerm2 Shell Integration, the `iterm2` pip package, and API access
(iTerm2 vends an auth cookie via AppleScript on first use, which triggers a
one-time macOS permission prompt).
"""

import json
import os
import sys

E_NO_MODULE = 3
E_NO_PROMPT = 4
E_NO_CONNECT = 5

try:
    import iterm2
except ImportError:
    print("the `iterm2` Python package is not installed", file=sys.stderr)
    sys.exit(E_NO_MODULE)


def wanted_session_id():
    """ITERM_SESSION_ID looks like `w0t0p0:UUID`; the API wants the UUID."""
    raw = os.environ.get("ITERM_SESSION_ID", "")
    if not raw:
        return None
    return raw.split(":")[-1] or None


def want_ansi():
    return os.environ.get("TCAP_ITERM2_ANSI") == "1"


async def main(connection):
    app = await iterm2.async_get_app(connection)

    session = None
    sid = wanted_session_id()
    if sid:
        session = app.get_session_by_id(sid)
    if session is None:
        # Fall back to whatever is focused, e.g. when ITERM_SESSION_ID is unset
        # because the shell was re-exec'd.
        window = app.current_terminal_window
        if window and window.current_tab:
            session = window.current_tab.current_session
    if session is None:
        print("could not locate the current iTerm2 session", file=sys.stderr)
        sys.exit(E_NO_PROMPT)

    prompt = await iterm2.async_get_last_prompt(connection, session.session_id)
    if prompt is None:
        print(
            "iTerm2 reported no prompt information; install Shell Integration "
            "(iTerm2 > Install Shell Integration)",
            file=sys.stderr,
        )
        sys.exit(E_NO_PROMPT)

    rng = getattr(prompt, "output_range", None)
    text = ""
    if rng is not None:
        start_y, end_y = rng.start.y, rng.end.y
        count = max(0, end_y - start_y + 1)
        if count:
            lines = await session.async_get_contents(start_y, count)
            # `hard_eol` distinguishes a real newline from a soft wrap, so
            # joining on it avoids inserting breaks mid-line.
            parts = []
            for line in lines:
                parts.append(line.string.rstrip())
                if not getattr(line, "hard_eol", True):
                    # Soft wrap: the next line continues this one.
                    parts[-1] = line.string
            text = "\n".join(parts)

    # exit_status is not documented on Prompt across all versions; absence is
    # not an error, we just report less metadata.
    exit_code = getattr(prompt, "exit_status", None)
    if exit_code is not None:
        try:
            exit_code = int(exit_code)
        except (TypeError, ValueError):
            exit_code = None

    print(
        json.dumps(
            {
                "output": text,
                "command": getattr(prompt, "command", None) or None,
                "exit_code": exit_code,
                "cwd": getattr(prompt, "working_directory", None) or None,
            }
        )
    )


try:
    # retry=False: fail fast rather than blocking a shell pipeline forever.
    iterm2.run_until_complete(main, False)
except SystemExit:
    raise
except Exception as exc:  # noqa: BLE001 - surface any connection failure to tcap
    print(f"could not connect to the iTerm2 API: {exc}", file=sys.stderr)
    sys.exit(E_NO_CONNECT)
