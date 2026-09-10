# @agora-build/tcap

Capture the last command and its output from your terminal, ready to pipe into an LLM.

```console
$ npm run build
> app@1.0.0 build
> tsc
Error: Cannot find module 'foo'

$ tcap | sgpt "tell me how to fix it"
```

`tcap` gives the model the command, its exit code, where it ran, and the output:

```console
$ npm run build
# exit: 1  cwd: ~/Dev/app  took: 3.2s

> app@1.0.0 build
> tsc
Error: Cannot find module 'foo'
```

The exit code carries more weight than it looks — it tells the model the command
genuinely failed, instead of leaving it to infer that from the text.

## Install

```sh
npm i -g @agora-build/tcap
```

The postinstall step downloads a prebuilt binary for your platform (macOS and
Linux, x64 and arm64). If it can't reach GitHub it falls back to
`dl.agora.build`, and if both fail it exits cleanly rather than breaking your
install — `tcap` then tells you how to recover.

Then enable shell integration and open a new terminal:

```sh
echo 'eval "$(tcap init zsh)"' >> ~/.zshrc
```

```sh
# bash
echo 'eval "$(tcap init bash)"' >> ~/.bashrc
# fish
echo 'tcap init fish | source' >> ~/.config/fish/config.fish
```

Confirm it's wired up:

```sh
tcap doctor
```

Shell integration is what supplies the exit code, duration and command text.
Without it you still get output, and `tcap` tells you what's missing and why.

## Usage

Two axes, each with an index and a count form, under one rule:
**lowercase selects the Nth from the end, uppercase takes the last N.**

| | index (lowercase) | count (uppercase) |
|---|---|---|
| **commands** | `-c 3` — 3rd-from-last block | `-C 3` — last 3 blocks |
| **lines** | `-l 3` — 3rd-from-last line | `-L 3` — last 3 lines |

They combine, which is what makes long build logs usable:

```sh
tcap                  # last command + its output
tcap -c 2             # the command before the last one
tcap -C 3             # the last 3 commands, oldest first
tcap -c 2 -L 50       # 2nd-to-last command, tail 50 lines of its output
```

### Output formats

```sh
tcap --output         # output only, no header
tcap --command        # the command text only
tcap --raw            # verbatim, ANSI colour intact
tcap --json           # {command, exit_code, cwd, duration_ms, output, source}
tcap --copy           # also copy to the clipboard
tcap --max-bytes 8000 # elide the middle to fit a smaller context window
```

`--command` prints the command; `-c` selects *which* command. Similar spelling,
unrelated jobs.

## Terminal support

| Terminal | Last command | Older (`-c 2`) | Needs |
|---|---|---|---|
| **tmux** | yes | **yes** | nothing |
| **kitty** | yes | no | `allow_remote_control yes` |
| **iTerm2** | yes | no | Shell Integration + `pip install iterm2` |
| **WezTerm** | not yet | no | run inside tmux |
| Ghostty, Terminal.app, Alacritty | no | no | run inside tmux |

Two things worth knowing up front:

**Only tmux can reach past the most recent command.** kitty exposes a
`last_cmd_output` extent and nothing older; iTerm2 exposes only its latest
prompt. So `tcap -c 2` fails there with a pointer to tmux rather than quietly
returning the wrong block.

**tmux wins whenever it's running.** Inside kitty running tmux, asking kitty for
its scrollback returns tmux's rendered viewport rather than the shell's real
history, so tmux is always treated as authoritative when it's in the stack.

Anything missing is reported with its cause and fix, on stderr so `tcap | sgpt`
still pipes clean text:

```console
$ tcap
tcap: captured output only — no command, exit code or duration.
  Cause: tcap's shell integration is not recording in this session,
         so there is nothing to annotate the output with.
  Fix:   add to your ~/.zshrc then open a new shell:
           eval "$(tcap init zsh)"
```

## Configuration

Optional, at `~/.config/tcap/config.toml`:

```toml
max_bytes = 8000        # smaller budget for a smaller context window
format    = "json"      # default output format
quiet     = false       # suppress the stderr hints
```

Precedence is `flags > environment > config > defaults`. An unknown key is an
error rather than a silent no-op, and `--no-quiet` / `--no-copy` escape a config
boolean for a single run.

## Links

- Source, full docs and issues: <https://github.com/Agora-Build/TermCap>
- Standalone install: `curl -fsSL https://dl.agora.build/tcap/install.sh | bash`

MIT
