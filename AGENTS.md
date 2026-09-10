# tcap — agent notes

Captures the last command and its output from the terminal, for piping into an LLM.
Rust binary named `tcap` (not `termcap`, which is the terminfo predecessor).

## Architecture

Two halves that fill each other's gaps:

- **Shell hooks** (`shell/tcap.{zsh,bash,fish}`, emitted by `tcap init <shell>`) record
  command text, exit code, cwd and duration on every prompt by calling `tcap __record`.
- **Terminal adapters** (`src/backend/`) retrieve the actual scrollback text.

`src/state.rs` is the JSONL log the hooks write, one file per session under
`$TMPDIR/tcap-<uid>/`. `src/render.rs` formats; `src/config.rs` loads optional TOML.

## Invariants and gotchas

**tmux must win backend detection whenever `$TMUX` is set.** Inside kitty running tmux,
`kitty @ get-text` returns tmux's *rendered viewport*, not the shell's scrollback, and tmux
swallows the OSC 133 prompt marks that kitty's `last_cmd_output` depends on. Asking kitty
there returns plausible but wrong text. Order lives in `backend::detect`.

**Scrollback coordinates are absolute, never pane-relative.** `capture-pane -S/-E` are
relative to the top visible row and shift as the pane scrolls. The hook stores
`history_size + cursor_y`, which is stable because the two move oppositely by the same
amount. Convert back at capture time with `relative = absolute - history_size_now`. Getting
this wrong silently returns the wrong region — no error, just wrong text. This is what the
tmux end-to-end tests exist to catch.

**`$?` must be read on the first line of precmd**, before anything can clobber it.

**The bash integration has two branches.** When bash-preexec is loaded (it ships with iTerm2
shell integration, Atuin and starship) it already owns the DEBUG trap, so tcap registers into
its `preexec_functions`/`precmd_functions` arrays; installing a competing trap captures
bash-preexec's own internals (`__bp_interactive_mode`) instead of the user's command. Without
it, tcap installs its own trap. Both branches are covered by
`every_available_shell_integration_records`, which runs bash with and without rc files.

**Command text reaches the binary as a single argv element**, so quoting, backslashes and
newlines need no escaping.

**Only tmux can reach past the most recent command.** kitty exposes a `last_cmd_output`
extent and nothing older; iTerm2 exposes only its latest prompt. `-c 2` on those must fail
with a pointer to tmux, never return the wrong block.

## Verification status

- **tmux** — fully working, covered by end-to-end tests.
- **kitty** — implemented against `--extent=last_cmd_output`, verified present in kitty 0.48.2.
- **iTerm2** — written to the documented Python API but **never run**; iTerm2 was not
  installed on the machine it was written on. Treat as unverified.
- **WezTerm** — detection only. Deliberately not implemented: there is no verified way to map
  a recorded boundary onto WezTerm's line numbering, and a guess would return wrong text
  rather than fail.

## Build and test

```sh
cargo test                            # unit + end-to-end
git config core.hooksPath .githooks   # fmt, clippy, tests before every push
```

End-to-end tests (`tests/tmux_e2e.rs`) drive a real tmux session. They synchronise by polling
tcap's own state log — never by appending a marker command to the line under test, which
would corrupt the recorded command text and steal its exit status. They skip themselves when
tmux is missing, so CI installs tmux explicitly rather than letting coverage silently vanish.

## Release

Tag-driven: pushing `vX.Y.Z` builds four targets, publishes a GitHub Release, then npm
(`@agora-build/tcap`) and the R2 mirror behind `dl.agora.build`. A `verify` job gates the
whole thing so a failing tag cannot ship.

Required secrets: `NPM_TOKEN`, `CF_ACCOUNT_ID`, `CF_API_TOKEN`, and for the review workflows
`ANTHROPIC_API_KEY` / `ANTHROPIC_BASE_URL` and `OPENAI_API_KEY` / `OPENAI_BASE_URL`.

R2 layout mirrors the other Agora-Build CLIs: shared `agora-build-releases` bucket, `tcap/`
key prefix, plus `tcap/releases/latest` as a plain-text version marker and `tcap/latest/`
aliases.

`codex-code-review.yml` pins `openai/codex-action` to the v1.11 SHA — the floating `@v1` moved
to v1.12, which hangs the job. See the comment in that file before bumping it.
