//! End-to-end tests against a real tmux session.
//!
//! The unit tests cover rendering and selection logic in isolation; these cover
//! the part that can only break for real — whether the scrollback coordinates
//! recorded by the shell hook actually line up with the text tmux hands back.
//! A off-by-one there is invisible to unit tests and obvious here.
//!
//! Each test drives an actual shell: install the integration, run commands,
//! then run `tcap` and assert on what it produced.
//!
//! Skipped (not failed) when tmux is unavailable, so the suite still runs in
//! environments without it.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_tcap");

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run tmux, failing loudly on a non-zero exit.
///
/// The unchecked version of this silently returned an empty string, which turned
/// every tmux problem into an indistinguishable "saw 0 recorded commands"
/// timeout with a blank pane dump.
fn tmux(args: &[&str]) -> String {
    let out = Command::new("tmux")
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("running tmux {args:?}: {e}"));
    if !out.status.success() {
        panic!(
            "tmux {args:?} failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// For commands whose failure is expected and uninteresting, e.g. killing a
/// session that was never created.
fn tmux_quiet(args: &[&str]) {
    let _ = Command::new("tmux").args(args).output();
}

/// Like [`tmux`], but retries first.
///
/// Creating a session can lose a race against a server that is still shutting
/// down — `tmux kill-server` in a neighbouring shell is enough to do it — and
/// that is a transient worth surviving rather than a failure worth reporting.
fn tmux_retrying(args: &[&str]) -> String {
    for attempt in 0..4 {
        let out = Command::new("tmux")
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("running tmux {args:?}: {e}"));
        if out.status.success() {
            return String::from_utf8_lossy(&out.stdout).into_owned();
        }
        if attempt == 3 {
            panic!(
                "tmux {args:?} failed after 4 attempts ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        std::thread::sleep(Duration::from_millis(200 * (attempt + 1)));
    }
    unreachable!()
}

fn which(program: &str) -> Option<String> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}

/// Shells with a tcap integration worth exercising.
///
/// The integrations are not interchangeable — zsh uses `add-zsh-hook`, bash a
/// DEBUG trap — so assuming zsh broke every test at once on CI, where the
/// default shell is bash and the zsh init failed with `add-zsh-hook: command
/// not found`.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Shell {
    Zsh,
    /// bash as the user actually has it, rc files and all.
    Bash,
    /// bash with no rc files, which is what CI runners give you.
    ///
    /// Worth testing separately because it decides which branch of the bash
    /// integration runs: with rc files this machine loads bash-preexec and we
    /// register into its hook arrays; without them we install our own DEBUG
    /// trap. Both occur in the wild, and only one of them runs on CI.
    BashBare,
}

impl Shell {
    /// The name passed to `tcap init`.
    fn name(self) -> &'static str {
        match self {
            Shell::Zsh => "zsh",
            Shell::Bash | Shell::BashBare => "bash",
        }
    }

    /// Label for assertion messages, distinguishing the two bash variants.
    fn label(self) -> &'static str {
        match self {
            Shell::Zsh => "zsh",
            Shell::Bash => "bash",
            Shell::BashBare => "bash (no rc)",
        }
    }

    fn binary(self) -> Option<String> {
        which(self.name())
    }

    /// The command tmux runs for the pane, wired to read `rc` at startup.
    ///
    /// The rc file is how setup reaches the shell. Typing it at the prompt is
    /// unreliable: a shell that puts up a startup prompt — zsh's newuser wizard,
    /// or compinit's "insecure directories" question — swallows the first
    /// characters sent and silently mangles the line.
    fn command(self, rc: &std::path::Path) -> Option<Vec<String>> {
        let bin = self.binary()?;
        let rc = rc.to_string_lossy().into_owned();
        Some(match self {
            // zsh finds its rc through ZDOTDIR, set by the caller.
            Shell::Zsh => vec![bin],
            Shell::Bash => vec![bin, "--rcfile".into(), rc],
            // --norc would override --rcfile, so only --noprofile here.
            Shell::BashBare => vec![bin, "--noprofile".into(), "--rcfile".into(), rc],
        })
    }
}

fn available_shells() -> Vec<Shell> {
    [Shell::Zsh, Shell::Bash, Shell::BashBare]
        .into_iter()
        .filter(|s| s.binary().is_some())
        .collect()
}

/// Preferred shell for the bulk of the suite; zsh when present.
fn primary_shell() -> Shell {
    *available_shells()
        .first()
        .expect("neither zsh nor bash found on PATH")
}

/// A disposable tmux session with tcap's shell integration loaded.
struct Session {
    name: String,
    dir: PathBuf,
}

impl Session {
    fn new(name: &str) -> Self {
        Self::with_shell(name, primary_shell())
    }

    fn with_shell(name: &str, shell: Shell) -> Self {
        // Scope the session name to this process so a concurrent run, or a
        // leftover session from an aborted one, cannot collide with it.
        let name = &format!("{name}-{}", std::process::id());
        let dir = std::env::temp_dir().join(format!("tcap-e2e-{name}"));
        std::fs::create_dir_all(&dir).unwrap();

        tmux_quiet(&["kill-session", "-t", name]);

        let bin_dir = PathBuf::from(BIN).parent().unwrap().to_path_buf();
        let log = dir.join("setup.log");
        let rc = dir.join("rc");

        // Installing tcap the way a user would — a line in an rc file — rather
        // than typing it at the prompt, which races shell startup.
        let setup = format!(
            // The eval comes last: in bash any line after it fires the
            // freshly-installed DEBUG trap and is recorded as if the user had
            // typed it.
            "export PATH=\"{bin}:$PATH\"\n\
             {{ echo \"tcap=$(command -v tcap)\"; echo \"TMPDIR=$TMPDIR\"; }} > {log} 2>&1\n\
             eval \"$(tcap init {sh})\" >> {log} 2>&1\n",
            bin = bin_dir.display(),
            sh = shell.name(),
            log = log.display(),
        );

        let mut env = vec![format!("TMPDIR={}", dir.display())];
        match shell {
            Shell::Zsh => {
                // zsh takes its rc from ZDOTDIR. Having a .zshrc there also
                // suppresses zsh-newuser-install, and skip_global_compinit
                // stops Ubuntu's /etc/zsh/zshrc running compinit — both put up
                // startup prompts that swallow input.
                std::fs::write(dir.join(".zshrc"), &setup).unwrap();
                std::fs::write(dir.join(".zshenv"), "skip_global_compinit=1\n").unwrap();
                env.push(format!("ZDOTDIR={}", dir.display()));
            }
            // Keep the developer's own rc so bash-preexec, when installed, is
            // exercised; --rcfile otherwise replaces it.
            Shell::Bash => {
                std::fs::write(&rc, format!("[ -f ~/.bashrc ] && . ~/.bashrc\n{setup}")).unwrap()
            }
            Shell::BashBare => std::fs::write(&rc, &setup).unwrap(),
        }

        // The shell is explicit rather than inherited from $SHELL, which is zsh
        // on a dev machine and bash on CI. The integrations are not
        // interchangeable, so inheriting silently installed nothing.
        let mut args: Vec<String> = vec![
            "new-session".into(),
            "-d".into(),
            "-s".into(),
            name.into(),
            "-x".into(),
            "160".into(),
            "-y".into(),
            "50".into(),
        ];
        for var in env {
            args.push("-e".into());
            args.push(var);
        }
        args.extend(shell.command(&rc).expect("shell binary vanished"));
        tmux_retrying(&args.iter().map(String::as_str).collect::<Vec<_>>());

        let s = Self {
            name: name.to_string(),
            dir,
        };
        s.wait_for_shell();
        s.verify_setup(&log, shell);

        // The hook was installed by the rc file, before the first prompt, so
        // the next command is the first that can appear in the log. Waiting for
        // it stops a slow shell startup masquerading as a capture bug.
        s.send_and_wait("true", "true");
        s
    }

    /// Fail immediately, and specifically, if setup did not take.
    ///
    /// Without this a mangled setup line surfaces 45 seconds later as a generic
    /// "saw 0 recorded commands", which is what made the CI failure so hard to
    /// place.
    fn verify_setup(&self, log: &std::path::Path, shell: Shell) {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut contents = String::new();
        while Instant::now() < deadline {
            contents = std::fs::read_to_string(log).unwrap_or_default();
            if contents.contains("tcap=") {
                break;
            }
            std::thread::sleep(Duration::from_millis(40));
        }

        let ctx = shell.label();
        assert!(
            contents.contains("tcap="),
            "{ctx}: shell never completed setup\n--- setup.log ---\n{contents}\n--- pane ---\n{}",
            tmux(&["capture-pane", "-p", "-t", &self.target()])
        );
        assert!(
            contents.lines().any(|l| l.starts_with("tcap=/")),
            "{ctx}: tcap did not reach PATH — the setup line was probably \
             mangled by a shell startup prompt\n--- setup.log ---\n{contents}\n--- pane ---\n{}",
            tmux(&["capture-pane", "-p", "-t", &self.target()])
        );
        assert!(
            !contents.contains("not found"),
            "{ctx}: `tcap init` reported an error\n--- setup.log ---\n{contents}"
        );
    }

    fn target(&self) -> String {
        format!("{}:0.0", self.name)
    }

    fn send(&self, keys: &str) {
        tmux(&["send-keys", "-t", &self.target(), keys, "Enter"]);
    }

    fn wait_for_shell(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if !tmux(&["list-panes", "-t", &self.name]).is_empty() {
                std::thread::sleep(Duration::from_millis(400));
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("tmux session {} never started", self.name);
    }

    /// How many commands the shell hook has recorded so far.
    ///
    /// This is the synchronisation primitive for the whole suite. Appending a
    /// marker command to the line under test would corrupt the very thing being
    /// measured — it becomes part of the recorded command text and its exit
    /// status replaces the real one — so instead we watch tcap's own log grow.
    fn record_count(&self) -> usize {
        self.recorded_commands().len()
    }

    /// Every command the hook has recorded, oldest first.
    ///
    /// Waiting on a *count* is not safe: in bash the rc file's own trailing
    /// lines fire the DEBUG trap and get recorded, so the count runs one ahead
    /// and every wait returns before its command has actually finished. Waiting
    /// for the exact command text is immune to that.
    fn recorded_commands(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_dir()
                    && p.file_name()
                        .map(|n| n.to_string_lossy().starts_with("tcap-"))
                        .unwrap_or(false)
            })
            .flat_map(|state_dir| std::fs::read_dir(state_dir).into_iter().flatten().flatten())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
            .filter_map(|p| std::fs::read_to_string(p).ok())
            .flat_map(|s| {
                s.lines()
                    .filter(|l| !l.trim().is_empty())
                    .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                    .filter_map(|v| v.get("command")?.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn count_matching(&self, token: &str) -> usize {
        self.recorded_commands()
            .iter()
            .filter(|c| c.contains(token))
            .count()
    }

    /// Send `line` and block until the hook records it.
    ///
    /// Matching is by substring rather than equality, because shells differ in
    /// how faithfully they can report a command line. It counts occurrences
    /// rather than testing presence: the same text can legitimately be run more
    /// than once — setup runs `true`, and so does one of the tests — and
    /// presence alone would match the earlier one and return before this
    /// command had even started.
    fn send_and_wait(&self, line: &str, token: &str) {
        let before = self.count_matching(token);
        self.send(line);

        let deadline = Instant::now() + Duration::from_secs(45);
        while Instant::now() < deadline {
            if self.count_matching(token) > before {
                return;
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        panic!(
            "timed out waiting for a new recorded command containing `{token}`\n\
             recorded so far: {:#?}\n\
             --- setup.log ---\n{}\n--- pane ---\n{}",
            self.recorded_commands(),
            std::fs::read_to_string(self.dir.join("setup.log"))
                .unwrap_or_else(|e| format!("(unreadable: {e})")),
            tmux(&["capture-pane", "-p", "-t", &self.target()])
        );
    }

    /// Recursive listing of the session's temp dir, for failure messages.
    fn describe_state_dir(&self) -> String {
        fn walk(dir: &std::path::Path, depth: usize, out: &mut String) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                out.push_str(&format!("{}(unreadable)\n", "  ".repeat(depth)));
                return;
            };
            for e in entries.flatten() {
                let p = e.path();
                let name = p
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                out.push_str(&format!("{}{name}\n", "  ".repeat(depth)));
                if p.is_dir() && depth < 3 {
                    walk(&p, depth + 1, out);
                }
            }
        }
        let mut out = String::new();
        walk(&self.dir, 0, &mut out);
        if out.is_empty() {
            out.push_str("(empty)\n");
        }
        out
    }

    fn wait_for_records(&self, at_least: usize) {
        let deadline = Instant::now() + Duration::from_secs(45);
        while Instant::now() < deadline {
            if self.record_count() >= at_least {
                return;
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        panic!(
            "timed out waiting for {at_least} recorded commands (saw {})\n\
             --- setup.log ---\n{}\n\
             --- state dir {} ---\n{}\n\
             --- pane ---\n{}",
            self.record_count(),
            std::fs::read_to_string(self.dir.join("setup.log"))
                .unwrap_or_else(|e| format!("(unreadable: {e})")),
            self.dir.display(),
            self.describe_state_dir(),
            tmux(&["capture-pane", "-p", "-t", &self.target()])
        );
    }

    /// Run a shell command verbatim and wait for the hook to record it.
    fn run(&self, cmd: &str) {
        self.send_and_wait(cmd, cmd);
    }

    /// Run tcap with `args` and return its stdout.
    ///
    /// The redirect is part of the command line, which is harmless: the whole
    /// line still begins with `tcap`, so indexing skips it as an invocation.
    fn tcap(&self, args: &str) -> String {
        let stem = rand_suffix();
        let out = self.dir.join(format!("out{stem}.txt"));
        let err = self.dir.join(format!("err{stem}.txt"));
        let rc = self.dir.join(format!("rc{stem}.txt"));

        // Keep stderr and the exit code rather than discarding them: routing
        // stderr to /dev/null turned a failing tcap into an empty string and an
        // assertion that could not say why.
        let sent = format!(
            "tcap {args} > {} 2> {}; echo $? > {}",
            out.display(),
            err.display(),
            rc.display()
        );
        // The stem appears in all three redirect paths, so it identifies this
        // invocation however much of the line the shell managed to record.
        self.send_and_wait(&sent, &stem);

        let status = std::fs::read_to_string(&rc).unwrap_or_default();
        let stderr = std::fs::read_to_string(&err).unwrap_or_default();
        assert_eq!(
            status.trim(),
            "0",
            "`tcap {args}` exited {}\n--- stderr ---\n{stderr}\n--- pane ---\n{}",
            status.trim(),
            tmux(&["capture-pane", "-p", "-t", &self.target()])
        );

        std::fs::read_to_string(&out).unwrap_or_default()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        tmux_quiet(&["kill-session", "-t", &self.name]);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn rand_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    format!("{n:x}")
}

macro_rules! skip_without_tmux {
    () => {
        if !tmux_available() {
            eprintln!("skipping: tmux not available");
            return;
        }
    };
}

/// The headline case: a failing build, captured with its exit code.
#[test]
fn captures_the_last_command_with_its_metadata_and_exact_output() {
    skip_without_tmux!();
    let s = Session::new("tcap-e2e-basic");

    s.run(r#"sh -c 'echo "> tsc"; echo "Error: Cannot find module foo"; exit 1'"#);
    let out = s.tcap("");

    assert!(out.contains("exit: 1"), "exit code missing:\n{out}");
    assert!(out.contains("cwd: "), "cwd missing:\n{out}");
    assert!(out.contains("> tsc"), "output missing:\n{out}");
    assert!(
        out.contains("Error: Cannot find module foo"),
        "error line missing:\n{out}"
    );

    // Boundary correctness: the prompt line must not bleed into the capture,
    // and the capture must not start with blank padding.
    assert!(
        !out.contains("$ tcap"),
        "tcap's own prompt leaked into the capture:\n{out}"
    );
    let body = out.split("\n\n").nth(1).unwrap_or("");
    assert!(
        body.starts_with("> tsc"),
        "output should begin at the first line the command printed:\n{body:?}"
    );
}

/// Every shell integration must actually record, not just zsh's.
///
/// zsh uses `add-zsh-hook`; bash uses a DEBUG trap with a latch and its own
/// `$?` handling. Only zsh was covered until CI — which runs bash — failed every
/// end-to-end test at once.
#[test]
fn every_available_shell_integration_records() {
    skip_without_tmux!();

    let shells = available_shells();
    assert!(!shells.is_empty(), "no shell available to test");

    for shell in shells {
        let s = Session::with_shell(&format!("tcap-e2e-shell-{shell:?}"), shell);

        s.run(r#"sh -c 'echo shell_probe; exit 2'"#);
        let out = s.tcap("");

        let ctx = shell.label();
        assert!(out.contains("exit: 2"), "{ctx}: exit code missing:\n{out}");
        assert!(out.contains("shell_probe"), "{ctx}: output missing:\n{out}");
        assert!(
            out.contains("shell_probe; exit 2"),
            "{ctx}: command text missing:\n{out}"
        );

        // Indexing depends on the hook recording every command, not just the last.
        s.run("echo second");
        let prev = s.tcap("-c 2");
        assert!(
            prev.contains("shell_probe"),
            "{ctx}: -c 2 did not reach the earlier command:\n{prev}"
        );

        // The whole line, not just its first simple command. bash's DEBUG trap
        // sees only `echo one` of `echo one; echo two`, so this needs the
        // history lookup rather than $BASH_COMMAND.
        s.run("echo one; echo two");
        let cmd = s.tcap("--command");
        assert_eq!(
            cmd.trim(),
            "echo one; echo two",
            "{ctx}: compound command was truncated"
        );

        // tcap's own integration script must not appear as a user command; the
        // bash DEBUG trap used to be armed before its own setup lines ran.
        let recorded = s.recorded_commands();
        let noise: Vec<&String> = recorded
            .iter()
            .filter(|c| c.contains("PROMPT_COMMAND") || c.contains("_tcap_"))
            .collect();
        assert!(
            noise.is_empty(),
            "{ctx}: tcap's own setup was recorded as user commands: {noise:#?}"
        );
    }
}

/// A successful command still reports `exit: 0` — the model needs to know the
/// command worked and the problem lies elsewhere.
#[test]
fn reports_exit_zero_for_successful_commands() {
    skip_without_tmux!();
    let s = Session::new("tcap-e2e-exit0");

    s.run("echo hello");
    let out = s.tcap("");

    assert!(out.contains("exit: 0"), "expected exit: 0 in:\n{out}");
    assert!(out.contains("hello"), "expected output in:\n{out}");
}

/// `-c 2` must count real commands, stepping over tcap's own invocations.
#[test]
fn command_index_skips_tcap_invocations() {
    skip_without_tmux!();
    let s = Session::new("tcap-e2e-index");

    s.run("echo FIRST_COMMAND");
    s.run("echo SECOND_COMMAND");
    // A capture in between must not shift the indices.
    let _ = s.tcap("");

    let last = s.tcap("");
    assert!(
        last.contains("SECOND_COMMAND"),
        "-c 1 should be the most recent real command:\n{last}"
    );

    let previous = s.tcap("-c 2");
    assert!(
        previous.contains("FIRST_COMMAND"),
        "-c 2 should reach past the tcap runs to the older command:\n{previous}"
    );
    assert!(
        !previous.contains("SECOND_COMMAND"),
        "-c 2 returned the wrong block:\n{previous}"
    );
}

/// The build-log case: uppercase `-L` keeps the tail.
#[test]
fn uppercase_l_keeps_the_last_n_lines() {
    skip_without_tmux!();
    let s = Session::new("tcap-e2e-tail");

    s.run("seq 1 200 | sed 's/^/line /'");
    let out = s.tcap("-L 3");

    assert!(out.contains("line 198"), "tail missing:\n{out}");
    assert!(out.contains("line 200"), "last line missing:\n{out}");
    assert!(
        !out.contains("line 100"),
        "-L should have dropped the middle:\n{out}"
    );
}

/// Lowercase `-l` is an index, not a count — the distinction the whole flag
/// scheme rests on.
#[test]
fn lowercase_l_selects_a_single_line_by_index() {
    skip_without_tmux!();
    let s = Session::new("tcap-e2e-lineidx");

    s.run("seq 1 200 | sed 's/^/line /'");
    let out = s.tcap("-l 2");

    assert!(
        out.contains("line 199"),
        "-l 2 should be the 2nd-from-last line:\n{out}"
    );
    assert!(
        !out.contains("line 200"),
        "-l 2 must not include the last line:\n{out}"
    );
    assert!(
        !out.contains("line 198"),
        "-l 2 must return exactly one line:\n{out}"
    );
}

/// JSON must carry the metadata a script would key off.
#[test]
fn json_output_is_structured_and_complete() {
    skip_without_tmux!();
    let s = Session::new("tcap-e2e-json");

    s.run("sh -c 'echo payload; exit 3'");
    let out = s.tcap("--json -L 1");

    let v: serde_json::Value =
        serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("invalid JSON: {e}\n{out}"));

    assert_eq!(v["exit_code"], 3, "exit code wrong in {v}");
    assert_eq!(v["output"], "payload", "output wrong in {v}");
    assert_eq!(v["source"], "tmux", "source wrong in {v}");
    assert!(v["command"].as_str().unwrap().contains("payload"));
}

/// A command that prints nothing must not invent output or capture the prompt.
#[test]
fn empty_output_is_captured_as_empty() {
    skip_without_tmux!();
    let s = Session::new("tcap-e2e-empty");

    s.run("true");
    let out = s.tcap("--output");

    assert!(
        out.trim().is_empty(),
        "expected no output for `true`, got:\n{out:?}"
    );
}

/// A config file must actually reach a live capture — the unit tests prove the
/// precedence arithmetic, this proves the value survives the whole path from
/// disk to rendered output.
#[test]
fn config_file_sets_the_default_format_and_flags_override_it() {
    skip_without_tmux!();
    let s = Session::new("tcap-e2e-config");

    let cfg = s.dir.join("config.toml");
    std::fs::write(&cfg, "format = \"json\"\n").unwrap();
    s.run(&format!("export TCAP_CONFIG={}", cfg.display()));

    s.run("echo configured");

    // Config alone: JSON, despite no --json flag.
    let out = s.tcap("");
    let v: serde_json::Value = serde_json::from_str(out.trim())
        .unwrap_or_else(|e| panic!("config format=json was not applied: {e}\n{out}"));
    assert_eq!(v["output"], "configured");

    // A flag must still beat the file.
    let overridden = s.tcap("--output");
    assert_eq!(
        overridden.trim(),
        "configured",
        "--output should have overridden format=json:\n{overridden}"
    );
}

/// A malformed config must fail loudly rather than being silently ignored.
#[test]
fn a_broken_config_is_reported_not_ignored() {
    skip_without_tmux!();
    let s = Session::new("tcap-e2e-badconfig");

    let cfg = s.dir.join("bad.toml");
    std::fs::write(&cfg, "max_byte = 1\n").unwrap();
    s.run(&format!("export TCAP_CONFIG={}", cfg.display()));
    s.run("echo hi");

    // stderr is where the complaint goes, so capture it deliberately.
    let marker = format!("cfgerr{}", rand_suffix());
    let path = s.dir.join(format!("{marker}.txt"));
    let before = s.record_count();
    s.send(&format!("tcap > /dev/null 2> {}", path.display()));
    s.wait_for_records(before + 1);

    let err = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        err.contains("max_byte"),
        "the bad key should be named:\n{err}"
    );
    assert!(
        err.contains("max_bytes"),
        "valid keys should be listed:\n{err}"
    );
}

/// `--command` returns the command as typed, including its quoting.
#[test]
fn command_mode_returns_the_command_text() {
    skip_without_tmux!();
    let s = Session::new("tcap-e2e-cmdonly");

    s.run("echo 'quoted arg'");
    let out = s.tcap("--command");

    assert_eq!(out.trim(), "echo 'quoted arg'", "got: {out:?}");
}
