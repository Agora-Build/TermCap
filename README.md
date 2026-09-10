# tcap

[![CI](https://github.com/Agora-Build/TermCap/actions/workflows/ci.yml/badge.svg)](https://github.com/Agora-Build/TermCap/actions/workflows/ci.yml)
[![npm](https://img.shields.io/npm/v/@agora-build/tcap)](https://www.npmjs.com/package/@agora-build/tcap)
[![license](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

Capture the last command and its output from your terminal, ready to pipe into an LLM.

```console
$ npm run build
> app@1.0.0 build
> tsc
Error: Cannot find module 'foo'

$ tcap | sgpt "tell me how to fix it"
```

`tcap` produces the command, its exit code, where it ran, and its output:

```console
$ npm run build
# exit: 1  cwd: ~/Dev/app  took: 3.2s

> app@1.0.0 build
> tsc
Error: Cannot find module 'foo'
```

The exit code matters more than it looks — it tells the model whether the command
actually failed, rather than making it guess from the text.

## Install

```sh
curl -fsSL https://dl.agora.build/tcap/install.sh | bash
```

or

```sh
npm i -g @agora-build/tcap
```

Then enable shell integration and open a new terminal:

```sh
echo 'eval "$(tcap init zsh)"' >> ~/.zshrc      # bash: tcap init bash >> ~/.bashrc
                                                # fish: tcap init fish | source
```

Check it worked:

```sh
tcap doctor
```

## Usage

Two axes, each with an index and a count form, following one rule:
**lowercase selects the Nth from the end, uppercase takes the last N.**

| | index (lowercase) | count (uppercase) |
|---|---|---|
| **commands** | `-c 3` — 3rd-from-last block | `-C 3` — last 3 blocks |
| **lines** | `-l 3` — 3rd-from-last line | `-L 3` — last 3 lines |

The axes combine, which is what makes long build logs tractable:

```sh
tcap                  # last command + its output
tcap -c 2             # the command before the last one
tcap -C 3             # the last 3 commands, oldest first
tcap -c 2 -L 50       # 2nd-to-last command, tail 50 lines of its output
```

Line flags apply to each block individually, so `-C 3 -L 20` gives the last 20
lines of each of three commands.

### Output formats

```sh
tcap --output         # output only, no header
tcap --command        # the command text only
tcap --raw            # verbatim, ANSI colour intact, no header
tcap --json           # {command, exit_code, cwd, duration_ms, output, source}
tcap --copy           # also copy to the clipboard
```

`--command` prints the command; `-c` selects *which* command. They are unrelated
despite the similar spelling.

### Other flags

```sh
tcap --backend tmux       # force an adapter instead of auto-detecting
tcap --max-bytes 8000     # elide the middle to fit an LLM budget (0 = unlimited)
tcap --quiet              # suppress the capability hints on stderr
```

## Configuration

Optional. `~/.config/tcap/config.toml`, or wherever `$TCAP_CONFIG` points:

```toml
backend   = "tmux"      # skip auto-detection
max_bytes = 8000        # smaller budget for a smaller context window
format    = "json"      # default output format
quiet     = false       # suppress the stderr hints
copy      = false       # always copy to the clipboard too
```

Precedence is `flags > environment > config file > built-in defaults`. Every key
is optional, and `config.example.toml` in this repo documents each one.

Config booleans stay escapable from the command line — `--no-quiet` and
`--no-copy` undo `quiet = true` / `copy = true` for a single run, so neither is a
one-way door.

An unknown key is an **error**, not a silent no-op:

```console
$ tcap
tcap: in ~/.config/tcap/config.toml: TOML parse error at line 1, column 1
  |
1 | max_byte = 100
  | ^^^^^^^^
unknown field `max_byte`, expected one of `backend`, `max_bytes`, `format`, `quiet`, `copy`
```

A setting you believe is applied but isn't is worse than a loud failure. `tcap
doctor` shows the resolved config, and still runs when the file is broken —
reporting the breakage is its job.

TOML rather than YAML because these are flat scalars, which is TOML's sweet spot,
while YAML's hand-editing hazards all apply: significant whitespace, and the
Norway problem, where `quiet: no` silently parses as `false`.

## Terminal support

| Terminal | Last command | Older commands (`-c 2`) | Notes |
|---|---|---|---|
| **tmux** | yes | **yes** | The only backend that can reach back |
| **kitty** | yes | no | Needs remote control (`yes`, or `socket-only` + `listen_on`) |
| **iTerm2** | yes | no | Needs Shell Integration + `pip install iterm2` |
| **WezTerm** | not yet | no | Detected; run inside tmux |
| Ghostty, Terminal.app, Alacritty | no | no | No remote-control API — run inside tmux |

Two things worth knowing:

**tmux wins whenever it is running.** Inside kitty running tmux, asking kitty for
its scrollback returns tmux's *rendered viewport*, not the shell's real history —
and tmux swallows the prompt marks kitty's output extents depend on. So tmux
is always treated as authoritative when it is in the stack.

**Only tmux can reach past the most recent command.** kitty exposes only the
latest output and nothing older; iTerm2 exposes only its latest prompt. `tcap -c
2` therefore fails with a pointer to tmux rather than silently returning the
wrong block.

**On kitty, output is the last *non-empty* output.** kitty counts the running
`tcap` as the current command, so it has to be asked for the last output that
wasn't empty. The consequence: if your last command printed nothing, kitty
returns an *older* command's output while the header names the last one — and two
`tcap` runs in a row return the first run's own output, since that was the last
thing to print. tcap warns on stderr when it can tell. tmux has no such
ambiguity, because the shell hook records real boundaries. kitty also
needs remote control reachable — either `allow_remote_control yes`, or
`socket-only` together with `listen_on unix:/tmp/kitty-{kitty_pid}`.

When something is missing, `tcap` says what and why:

```console
$ tcap
tcap: captured output only — no command, exit code or duration.
  Cause: tcap's shell integration is not recording in this session,
         so there is nothing to annotate the output with.
  Fix:   add to your ~/.zshrc then open a new shell:
           eval "$(tcap init zsh)"
```

Hints go to stderr, so `tcap | sgpt` still pipes clean text.

## How it works

Terminals do not report exit codes, durations, or where one command's output ends
and the next begins. `tcap` fills those gaps from two sides:

- **Shell hooks** (`tcap init`) record the command text, exit code, cwd and
  duration on every prompt.
- **Terminal adapters** retrieve the actual text. kitty and iTerm2 know their own
  command boundaries; tmux does not (it has no OSC 133 support), so the hook also
  records where output starts and ends.

Those tmux coordinates are stored as `history_size + cursor_y`, which is stable
under scrolling: as lines scroll off, `history_size` grows by exactly as much as
`cursor_y` shrinks, so the sum stays pinned to the same row of text. Storing raw
`capture-pane` offsets would drift the moment the pane scrolled.

## Development

```sh
cargo test                       # 54 unit + 11 end-to-end tests
git config core.hooksPath .githooks   # run fmt, clippy and tests before every push
```

The end-to-end tests drive a real tmux session — they install the shell
integration, run commands, and assert on what `tcap` returns. That is the only
way to catch an off-by-one in the scrollback coordinates, which is invisible to
unit tests and silently returns the wrong text. They skip themselves when tmux is
unavailable.

## Naming

Not to be confused with `termcap`, the terminfo predecessor. The binary is `tcap`
precisely to avoid that collision.

## License

MIT
