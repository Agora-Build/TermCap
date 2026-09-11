//! tcap — capture the last command and its output from your terminal.

mod ansi;
mod backend;
mod capture;
mod cli;
mod config;
mod doctor;
mod render;
mod state;

use std::io::IsTerminal;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use clap::Parser;

use capture::Capture;
use cli::{Cli, RecordArgs, ShellKind, Sub};
use state::Record;

fn main() {
    if let Err(e) = run() {
        eprintln!("tcap: {e:#}");
        mark_if_stderr_is_terminal();
        std::process::exit(1);
    }
}

/// Record that tcap put text on the screen.
///
/// Called at the point of each write, gated on *that stream* being a terminal —
/// not inferred from isatty at exit. Inferring was wrong three ways: under
/// `eval "$(tcap init zsh)"` stdout is a pipe but stderr is still a tty, so
/// every shell startup marked itself and poisoned its first real command;
/// `tcap --output > f` marked despite printing nothing visible; and clap's
/// `--help` exits before any of it runs.
fn mark_if_stdout_is_terminal() {
    if std::io::stdout().is_terminal() {
        state::mark_capture();
    }
}

fn mark_if_stderr_is_terminal() {
    if std::io::stderr().is_terminal() {
        state::mark_capture();
    }
}

fn run() -> Result<()> {
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            // --help and --version print to stdout, usage errors to stderr.
            let _ = e.print();
            if e.use_stderr() {
                mark_if_stderr_is_terminal();
            } else {
                mark_if_stdout_is_terminal();
            }
            std::process::exit(e.exit_code());
        }
    };

    match &cli.command {
        // doctor must survive a broken config — reporting the breakage is
        // exactly its job, so the error is handed over rather than propagated.
        // doctor and init put their text on screen too when not redirected —
        // `eval "$(tcap init zsh)"` pipes stdout, so that correctly does not mark.
        Some(Sub::Doctor) => {
            print!("{}", doctor::run(config::load()));
            mark_if_stdout_is_terminal();
            Ok(())
        }
        Some(Sub::Init { shell }) => {
            print!("{}", init_script(*shell));
            mark_if_stdout_is_terminal();
            Ok(())
        }
        // The record hook runs before every prompt and must not depend on, or
        // be broken by, a malformed config file.
        Some(Sub::Record(args)) => record(args),
        None => {
            let cfg = config::load()?;
            capture_and_print(&cli, &cli.resolve(&cfg))
        }
    }
}

fn init_script(shell: ShellKind) -> &'static str {
    match shell {
        ShellKind::Zsh => include_str!("../shell/tcap.zsh"),
        ShellKind::Bash => include_str!("../shell/tcap.bash"),
        ShellKind::Fish => include_str!("../shell/tcap.fish"),
    }
}

/// Handle `tcap __record`, called from the shell's precmd hook.
///
/// Deliberately silent on failure: this runs before every prompt, and a broken
/// state directory must never print noise into the user's shell.
fn record(args: &RecordArgs) -> Result<()> {
    let duration_ms = args.start_time.and_then(|start| {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_secs_f64();
        let elapsed = now - start;
        // A negative delta means a clock change; report nothing rather than nonsense.
        (elapsed >= 0.0).then_some((elapsed * 1000.0) as u64)
    });

    // The end anchor is sampled here rather than in the shell because nothing
    // has printed between the command finishing and this process running, so
    // the cursor is still sitting immediately after the command's output.
    let (start_line, end_line) = match args.start_anchor {
        Some(anchor) => {
            let end = backend::tmux::Tmux::new().output_end_line().ok();
            (Some(anchor), end)
        }
        None => (None, None),
    };

    state::append(&Record {
        command: args.command.clone(),
        exit_code: args.exit,
        cwd: args.cwd.clone(),
        duration_ms,
        start_line,
        end_line,
        was_capture: state::take_capture_marker(),
    })
}

fn capture_and_print(cli: &Cli, settings: &cli::Settings) -> Result<()> {
    let backend = backend::detect(settings.backend.as_deref())?;
    let records = state::load();
    let mode = settings.mode;
    let raw = mode == cli::Mode::Raw;

    let (count, newest) = cli.select.block_range();

    // Higher index means older, so walking the range in reverse yields blocks
    // oldest-first — the order they were actually run.
    let mut caps: Vec<Capture> = Vec::new();
    for index in (newest..newest + count).rev() {
        let rec = state::nth_from_end(&records, index);

        match backend.fetch(index, rec, raw) {
            Ok(fetched) => caps.push(build_capture(
                fetched,
                rec,
                backend.name(),
                !backend.has_command_boundaries(),
            )),
            // The block the user explicitly asked for must report its error.
            // Extra blocks requested via -C are best-effort: running out of
            // history is normal and should not fail the whole capture.
            Err(e) if index == newest => return Err(e),
            Err(_) => continue,
        }
    }

    if caps.is_empty() {
        return Err(anyhow!(
            "nothing captured. Run `tcap doctor` to see which backend is active \
             and whether shell integration is recording commands."
        ));
    }

    // Only the marker, deliberately — not `is_capture_record`. The two answer
    // different questions: indexing asks "was this a tcap run at all", which the
    // text heuristic approximates well enough; this asks "is tcap's own text the
    // last thing on screen", true only when the capture actually displayed. A
    // redirected `tcap --output > notes.txt` printed nothing to the terminal, so
    // the screen's last output is still a real command's and the next capture is
    // perfectly valid.
    let after_tcap = records.last().map(|r| r.was_capture).unwrap_or(false);

    // Refuse rather than emit, in the one case where the output is known to be
    // wrong: a boundary-less backend returns the last non-empty output, and
    // after a previous tcap that is the previous capture. Printing it first and
    // warning afterwards is no use in `tcap --output | some-service`, where the
    // text has already left by the time stderr is read. Where the mismatch is
    // only possible rather than certain, the capture proceeds and is labelled.
    if after_tcap && caps.iter().any(|c| c.approximate) && mode != cli::Mode::Command {
        return Err(anyhow!(
            "refusing to capture: the previous command was tcap itself, and {} can only\n\
             report the last non-empty output — which is that capture, not a command.\n\n\
             Re-run the command you want to capture, then tcap. Inside tmux this is not\n\
             a limitation, because the shell hook records exact boundaries.",
            backend.name()
        ));
    }

    render::prepare(&mut caps, mode, &cli.select, settings.max_bytes);
    let text = render::render(&caps, mode);

    println!("{text}");
    mark_if_stdout_is_terminal();

    if !settings.quiet && emit_hints(&caps, backend.as_ref(), mode) {
        // The hint itself is now the screen's last output.
        mark_if_stderr_is_terminal();
    }

    if settings.copy {
        copy_to_clipboard(&text)?;
    }

    Ok(())
}

/// Warn about degraded captures, naming the cause and the fix. Goes to stderr
/// so `tcap | sgpt` still pipes clean text.
/// Returns whether anything was printed.
fn emit_hints(caps: &[Capture], backend: &dyn backend::Backend, mode: cli::Mode) -> bool {
    let mut printed = false;
    // Every mode but Command: --output and --raw are the ones piped into another
    // tool, so they are where handing over an earlier command's text does the
    // most damage. Command mode renders the shell's own record, which is exact,
    // and never prints the terminal text this warning is about.
    //
    // The *certain* mismatch — a capture straight after another one — does not
    // reach here at all; capture_and_print refuses it outright.
    if mode != cli::Mode::Command && caps.iter().any(|c| c.approximate) {
        eprintln!(
            "tcap: {} returns the last non-empty output, so if the command named above\n\
             \x20 printed nothing this is an earlier command's output. tmux records\n\
             \x20 exact boundaries; --quiet silences this.",
            backend.name()
        );
        printed = true;
    }

    // Only the annotated view promises metadata, so only it can disappoint.
    if mode != cli::Mode::Annotated {
        return printed;
    }

    if caps.iter().any(Capture::has_metadata) {
        return printed;
    }

    let shell = detect_shell();
    eprintln!(
        "tcap: captured output only — no command, exit code or duration.\n\
         \x20 Cause: tcap's shell integration is not recording in this session,\n\
         \x20        so there is nothing to annotate the output with.\n\
         \x20 Fix:   add to your {rc} then open a new shell:\n\
         \x20          eval \"$(tcap init {shell})\"\n\
         \x20 Silence this with --quiet, or run `tcap doctor` for the full picture.",
        rc = match shell.as_str() {
            "bash" => "~/.bashrc",
            "fish" => "~/.config/fish/config.fish",
            _ => "~/.zshrc",
        },
        shell = shell,
    );

    // Mentioned only when it is already biting, to avoid nagging kitty users
    // who never ask for anything but the last command.
    if !backend.supports_history() {
        eprintln!(
            "tcap: {} can only report its most recent command; -c 2 and beyond \
             need tmux.",
            backend.name()
        );
    }

    true
}

fn detect_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .and_then(|s| s.rsplit('/').next().map(str::to_string))
        .filter(|s| matches!(s.as_str(), "zsh" | "bash" | "fish"))
        .unwrap_or_else(|| "zsh".to_string())
}

/// The shell record wins where both have a value — it saw the command as typed
/// and is the only source of duration. Backend metadata fills the gaps, which is
/// what makes iTerm2 usable with no shell integration at all.
fn build_capture(
    fetched: backend::Fetched,
    rec: Option<&Record>,
    source: &str,
    approximate: bool,
) -> Capture {
    let mut cap = Capture::new(fetched.output, source);
    cap.approximate = approximate;

    cap.command = fetched.command;
    cap.exit_code = fetched.exit_code;
    cap.cwd = fetched.cwd;

    if let Some(r) = rec {
        cap.command = Some(r.command.clone());
        cap.exit_code = r.exit_code.or(cap.exit_code);
        cap.cwd = r.cwd.clone().or(cap.cwd);
        cap.duration_ms = r.duration_ms;
    }

    cap
}

fn copy_to_clipboard(text: &str) -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let candidates: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else {
        &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ]
    };

    for (program, args) in candidates {
        if !backend::on_path(program) {
            continue;
        }
        let mut child = Command::new(program)
            .args(*args)
            .stdin(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("could not open a pipe to {program}"))?
            .write_all(text.as_bytes())?;
        child.wait()?;
        return Ok(());
    }

    Err(anyhow!(
        "no clipboard tool found (looked for {})",
        candidates
            .iter()
            .map(|(p, _)| *p)
            .collect::<Vec<_>>()
            .join(", ")
    ))
}
