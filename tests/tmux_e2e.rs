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
            Shell::BashBare => "bash --norc",
        }
    }

    fn binary(self) -> Option<String> {
        which(self.name())
    }

    /// The command tmux should run for the pane.
    fn command(self) -> Option<Vec<String>> {
        let bin = self.binary()?;
        Some(match self {
            Shell::BashBare => vec![bin, "--norc".into(), "--noprofile".into()],
            _ => vec![bin],
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
        // The shell is explicit rather than inherited from $SHELL, which is zsh
        // on a dev machine and bash on CI. The integrations are not
        // interchangeable, so inheriting silently installed nothing.
        let tmpdir = format!("TMPDIR={}", dir.display());
        let mut args: Vec<String> = vec![
            "new-session".into(),
            "-d".into(),
            "-s".into(),
            name.into(),
            "-x".into(),
            "160".into(),
            "-y".into(),
            "50".into(),
            "-e".into(),
            tmpdir,
        ];
        args.extend(shell.command().expect("shell binary vanished"));
        tmux(&args.iter().map(String::as_str).collect::<Vec<_>>());

        let s = Self {
            name: name.to_string(),
            dir,
        };
        s.wait_for_shell();

        // Record what setup actually did. Previously this ran `clear`, which
        // erased the evidence and made every setup failure look identical to a
        // capture bug.
        let bin_dir = PathBuf::from(BIN).parent().unwrap().to_path_buf();
        let log = s.dir.join("setup.log");
        s.send(&format!(
            "export PATH=\"{bin}:$PATH\"; \
             {{ echo \"tcap=$(command -v tcap)\"; echo \"TMPDIR=$TMPDIR\"; \
             eval \"$(tcap init {sh})\"; echo \"init_rc=$?\"; }} > {log} 2>&1",
            bin = bin_dir.display(),
            sh = shell.name(),
            log = log.display(),
        ));

        // The hook is installed *during* that line, so its own preexec never
        // ran and it records nothing. The next command is the first to appear
        // in the log — wait for it, so a slow shell startup cannot masquerade
        // as a capture bug.
        s.send("true");
        s.wait_for_records(1);
        s
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
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return 0;
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
            .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
            .sum()
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
        let before = self.record_count();
        self.send(cmd);
        self.wait_for_records(before + 1);
    }

    /// Run tcap with `args` and return its stdout.
    ///
    /// The redirect is part of the command line, which is harmless: the whole
    /// line still begins with `tcap`, so indexing skips it as an invocation.
    fn tcap(&self, args: &str) -> String {
        let out = self.dir.join(format!("out{}.txt", rand_suffix()));
        let before = self.record_count();
        self.send(&format!("tcap {args} > {} 2>/dev/null", out.display()));
        self.wait_for_records(before + 1);
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
