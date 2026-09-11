//! Per-session command history recorded by the shell integration.
//!
//! Terminals report neither exit code, cwd, duration, nor (for tmux) where one
//! command's output ends, so the hooks in `shell/` call `tcap __record` after
//! every command.
//!
//! JSON Lines: appending keeps the hook cheap, and compaction past
//! [`COMPACT_AT`] bounds the file in a long-lived shell.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Records retained when the log is compacted. Must comfortably exceed the
/// largest `-n`/`-c` anyone would plausibly type.
const KEEP: usize = 40;
/// Compact once the log exceeds this many lines.
const COMPACT_AT: usize = 120;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Absolute scrollback coordinate (`history_size + cursor_y`) of the first
    /// line of output. See `backend::tmux` for why the sum is scroll-stable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_line: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<i64>,
    /// Set when tcap performed a capture during this command.
    ///
    /// Recorded by the hook rather than inferred from the command text, so it
    /// holds for `command tcap`, `env tcap`, an alias, a shell function or any
    /// other spelling that text matching cannot see.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub was_capture: bool,
}

/// Dropped by a capture, collected by the next `__record`.
///
/// tcap captures *during* a command; the hook records that same command when it
/// finishes. So a marker present at record time means this command is the one
/// that captured, whatever it was called.
fn marker_path() -> PathBuf {
    state_dir().join(format!("{}.capturing", session_key()))
}

pub fn mark_capture() -> Result<()> {
    secure_open(&marker_path())?.write_all(b"1")?;
    Ok(())
}

/// Whether a capture happened during this command, clearing the marker.
pub fn take_capture_marker() -> bool {
    fs::remove_file(marker_path()).is_ok()
}

/// Why the state directory is unusable, if it is.
///
/// `load` failing closed to an empty history also switches off the guard that
/// depends on it, so the caller needs to be able to say so rather than behave as
/// though this were a fresh session.
pub fn unusable_reason() -> Option<String> {
    ensure_state_dir().err().map(|e| format!("{e:#}"))
}

/// Directory holding one log per terminal session.
pub fn state_dir() -> PathBuf {
    let base = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    // TMPDIR is already per-user on macOS; the uid suffix matters for /tmp,
    // where it keeps one user's captures out of another's directory.
    base.join(format!("tcap-{}", current_uid()))
}

fn current_uid() -> u32 {
    // Safe: getuid cannot fail and touches no memory we own.
    unsafe { libc::getuid() }
}

/// The state directory, created 0700 and checked to be ours.
///
/// The paths under it are predictable, and on Linux the fallback is the shared
/// /tmp. Without this, another local user could pre-create the directory and
/// leave a symlink where a state file goes: an ordinary write would then follow
/// it and truncate whatever it pointed at. macOS is not exposed (TMPDIR is
/// already per-user) but the check is cheap and the fallback is not.
fn ensure_state_dir() -> Result<PathBuf> {
    let dir = state_dir();

    // Created 0700 from the outset rather than chmod'ed afterwards, so there is
    // no window where it exists world-readable.
    match fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
    {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e).with_context(|| format!("creating {}", dir.display())),
    }

    // Inspected before anything is mutated: `set_permissions` follows symlinks,
    // so tightening first would let a planted symlink redirect the chmod onto a
    // file of the attacker's choosing that the victim owns.
    let md = fs::symlink_metadata(&dir).with_context(|| format!("checking {}", dir.display()))?;
    if !md.is_dir() {
        return Err(anyhow!(
            "{} is a symlink or not a directory — refusing to use it",
            dir.display()
        ));
    }
    if md.uid() != current_uid() {
        return Err(anyhow!(
            "{} is owned by uid {}, not you — refusing to use it",
            dir.display(),
            md.uid()
        ));
    }
    // Only now that it is known to be our own real directory.
    if md.mode() & 0o077 != 0 {
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("securing {}", dir.display()))?;
    }

    Ok(dir)
}

/// Read a state file with the same checks the writes get.
///
/// Reads matter as much as writes here: a pre-created directory with a crafted
/// log would let another user inject records, and their command text is printed
/// back — escape sequences included.
fn secure_read(path: &Path) -> Option<String> {
    ensure_state_dir().ok()?;
    let mut f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    let mut buf = String::new();
    f.read_to_string(&mut buf).ok()?;
    Some(buf)
}

/// Write a file under the state directory with the same protections.
pub fn secure_write(path: &Path, bytes: &[u8]) -> Result<()> {
    secure_open(path)?.write_all(bytes)?;
    Ok(())
}

/// Read a file under the state directory with the same protections.
pub fn secure_read_file(path: &Path) -> Option<String> {
    secure_read(path)
}

/// Open a state file for writing without following symlinks.
fn secure_open(path: &Path) -> Result<fs::File> {
    ensure_state_dir()?;
    OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        // The whole point: a symlink here fails instead of being followed.
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))
}

/// Identify the current terminal session.
///
/// Falls back to the parent pid, which is the shell itself, so a session
/// without any of these variables still gets a stable key for its lifetime.
pub fn session_key() -> String {
    for var in [
        "TMUX_PANE",
        "KITTY_WINDOW_ID",
        "WEZTERM_PANE",
        "ITERM_SESSION_ID",
    ] {
        if let Ok(v) = std::env::var(var) {
            if !v.is_empty() {
                return sanitize(&format!("{var}-{v}"));
            }
        }
    }
    format!("ppid-{}", std::os::unix::process::parent_id())
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

pub fn log_path() -> PathBuf {
    state_dir().join(format!("{}.jsonl", session_key()))
}

/// Append one record, compacting the log if it has grown too long.
pub fn append(rec: &Record) -> Result<()> {
    ensure_state_dir()?;

    let path = log_path();
    let line = serde_json::to_string(rec)?;

    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    writeln!(f, "{line}")?;
    drop(f);

    compact_if_needed(&path)?;
    Ok(())
}

fn compact_if_needed(path: &PathBuf) -> Result<()> {
    let Some(text) = secure_read(path) else {
        return Ok(());
    };
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= COMPACT_AT {
        return Ok(());
    }
    let kept = &lines[lines.len() - KEEP..];
    // Write via a temp file so a concurrent reader never sees a partial log.
    let tmp = path.with_extension("jsonl.tmp");
    secure_open(&tmp)?.write_all(format!("{}\n", kept.join("\n")).as_bytes())?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Load this session's records, oldest first. Unparseable lines are skipped
/// rather than failing the whole read.
pub fn load() -> Vec<Record> {
    let Some(text) = secure_read(&log_path()) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Record>(l).ok())
        .collect()
}

/// `-c` counts real commands, so `tcap -c 1` must mean "the command I just ran",
/// never a previous capture.
///
/// Text matching only, so it cannot see an alias or a shell function — which is
/// why the authoritative signal is [`Record::was_capture`], set by the hook.
/// This remains for records written before that existed, and for skipping in
/// [`nth_from_end`]. It looks past leading `VAR=value` assignments and the usual
/// wrappers.
pub fn is_tcap_invocation(cmd: &str) -> bool {
    cmd.split(['|', ';', '&']).any(|seg| {
        let mut words = seg.split_whitespace().peekable();
        loop {
            let Some(w) = words.peek() else { return false };
            let base = w.rsplit('/').next().unwrap_or(w);
            // `FOO=bar tcap`, `command tcap`, `env FOO=1 tcap`, `\tcap`.
            if (w.contains('=') && !w.starts_with('='))
                || matches!(
                    base,
                    "command" | "builtin" | "exec" | "env" | "nohup" | "time"
                )
            {
                words.next();
                continue;
            }
            return base.trim_start_matches('\\') == "tcap";
        }
    })
}

/// The `n`th real command counting back from the most recent (`n == 1` is the
/// last command), with `tcap` invocations skipped.
pub fn nth_from_end(records: &[Record], n: usize) -> Option<&Record> {
    // Deliberately the text check, not `is_capture_record`. `was_capture` means
    // "tcap wrote to this terminal during this command", which is also true of a
    // wrapper script, a Makefile target or a git alias that happens to call
    // tcap. Skipping those would silently return the command before them — on
    // tmux too, where boundaries are exact and none of this is needed.
    records
        .iter()
        .rev()
        .filter(|r| !is_tcap_invocation(&r.command))
        .nth(n.saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(command: &str) -> Record {
        Record {
            command: command.to_string(),
            exit_code: None,
            cwd: None,
            duration_ms: None,
            start_line: None,
            end_line: None,
            was_capture: false,
        }
    }

    #[test]
    fn detects_tcap_invocations() {
        assert!(is_tcap_invocation("tcap"));
        assert!(is_tcap_invocation("tcap -n 2"));
        assert!(is_tcap_invocation("tcap | sgpt \"fix it\""));
        assert!(is_tcap_invocation("/usr/local/bin/tcap --json"));
        assert!(is_tcap_invocation("FOO=bar tcap"));
        assert!(is_tcap_invocation("ls; tcap"));
    }

    /// Wrappers that the original first-word check walked straight past.
    #[test]
    fn detects_wrapped_invocations() {
        for cmd in [
            "command tcap",
            "env tcap --output",
            "env FOO=1 tcap",
            "builtin tcap",
            "exec tcap",
            "nohup tcap",
            "command /usr/local/bin/tcap",
        ] {
            assert!(is_tcap_invocation(cmd), "should detect: {cmd}");
        }
    }

    /// The two signals answer different questions and are used in different
    /// places: `was_capture` gates the refusal (is tcap's text on screen), the
    /// text check gates indexing (was this a tcap run). An alias is visible only
    /// to the first; a piped capture sets only the second. Conflating them has
    /// broken each direction once.
    #[test]
    fn the_two_capture_signals_are_independent() {
        let mut aliased = rec("t");
        aliased.was_capture = true;
        assert!(
            !is_tcap_invocation(&aliased.command),
            "text cannot see an alias"
        );

        let piped = rec("tcap --output | llm");
        assert!(
            is_tcap_invocation(&piped.command),
            "text sees an explicit run"
        );
        assert!(
            !piped.was_capture,
            "but stdout was a pipe, so nothing was displayed"
        );
    }

    #[test]
    fn does_not_match_unrelated_commands() {
        assert!(!is_tcap_invocation("npm run build"));
        assert!(!is_tcap_invocation("echo tcap"));
        assert!(!is_tcap_invocation("git commit -m 'add tcap'"));
        assert!(!is_tcap_invocation("tcapture"));
        assert!(!is_tcap_invocation(""));
    }

    #[test]
    fn nth_skips_tcap_and_is_one_based() {
        // Chronological: build, ls, tcap
        let records = vec![rec("npm run build"), rec("ls"), rec("tcap")];
        assert_eq!(nth_from_end(&records, 1).unwrap().command, "ls");
        assert_eq!(nth_from_end(&records, 2).unwrap().command, "npm run build");
        assert!(nth_from_end(&records, 3).is_none());
    }

    /// A command that merely *called* tcap is still a real command, and must
    /// stay indexable. `was_capture` cannot tell "was tcap" from "called tcap",
    /// so indexing uses the text check and accepts missing the alias case —
    /// naming an alias is visibly odd, whereas silently skipping someone's
    /// deploy script returns the wrong command with nothing to notice.
    #[test]
    fn nth_keeps_commands_that_merely_called_tcap() {
        let mut wrapper = rec("./deploy.sh");
        wrapper.was_capture = true;
        let records = vec![rec("npm run build"), wrapper];

        assert_eq!(nth_from_end(&records, 1).unwrap().command, "./deploy.sh");
        assert_eq!(nth_from_end(&records, 2).unwrap().command, "npm run build");
    }

    #[test]
    fn nth_returns_none_past_the_start_of_history() {
        assert!(nth_from_end(&[], 1).is_none());
        assert!(nth_from_end(&[rec("only")], 2).is_none());
    }
}
