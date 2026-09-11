//! Per-session command history recorded by the shell integration.
//!
//! Terminals report neither exit code, cwd, duration, nor (for tmux) where one
//! command's output ends, so the hooks in `shell/` call `tcap __record` after
//! every command.
//!
//! JSON Lines: appending keeps the hook cheap, and compaction past
//! [`COMPACT_AT`] bounds the file in a long-lived shell.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

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

pub fn mark_capture() {
    let _ = fs::create_dir_all(state_dir());
    let _ = fs::write(marker_path(), b"1");
}

/// Whether a capture happened during this command, clearing the marker.
pub fn take_capture_marker() -> bool {
    fs::remove_file(marker_path()).is_ok()
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

/// Avoids a `libc` dependency for a single call.
fn current_uid() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
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
    let dir = state_dir();
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating state directory {}", dir.display()))?;

    let path = log_path();
    let line = serde_json::to_string(rec)?;

    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    writeln!(f, "{line}")?;
    drop(f);

    compact_if_needed(&path)?;
    Ok(())
}

fn compact_if_needed(path: &PathBuf) -> Result<()> {
    let Ok(text) = fs::read_to_string(path) else {
        return Ok(());
    };
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= COMPACT_AT {
        return Ok(());
    }
    let kept = &lines[lines.len() - KEEP..];
    // Write via a temp file so a concurrent reader never sees a partial log.
    let tmp = path.with_extension("jsonl.tmp");
    fs::write(&tmp, format!("{}\n", kept.join("\n")))?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Load this session's records, oldest first. Unparseable lines are skipped
/// rather than failing the whole read.
pub fn load() -> Vec<Record> {
    let Ok(text) = fs::read_to_string(log_path()) else {
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

/// Whether this record is a tcap capture, by any spelling.
///
/// `was_capture` is authoritative: the hook set it during the command that
/// captured, so it holds for aliases and shell functions that no amount of text
/// matching would catch.
pub fn is_capture_record(r: &Record) -> bool {
    r.was_capture || is_tcap_invocation(&r.command)
}

/// The `n`th real command counting back from the most recent (`n == 1` is the
/// last command), with `tcap` invocations skipped.
pub fn nth_from_end(records: &[Record], n: usize) -> Option<&Record> {
    records
        .iter()
        .rev()
        .filter(|r| !is_capture_record(r))
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

    /// The marker catches what text matching cannot: an alias or function whose
    /// name says nothing about tcap.
    #[test]
    fn was_capture_beats_the_text_heuristic() {
        let mut aliased = rec("t");
        assert!(
            !is_capture_record(&aliased),
            "text alone cannot see an alias"
        );
        aliased.was_capture = true;
        assert!(
            is_capture_record(&aliased),
            "the hook's marker is authoritative"
        );

        assert!(is_capture_record(&rec("tcap --json")));
        assert!(!is_capture_record(&rec("npm run build")));
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

    /// An aliased capture has no tcap in its text, so only `was_capture` keeps
    /// it from being indexed as a real command — which would pair its metadata
    /// with unrelated output, on tmux as much as kitty.
    #[test]
    fn nth_skips_captures_made_through_an_alias() {
        let mut aliased = rec("t");
        aliased.was_capture = true;
        let records = vec![rec("npm run build"), rec("ls"), aliased];

        assert_eq!(nth_from_end(&records, 1).unwrap().command, "ls");
        assert_eq!(nth_from_end(&records, 2).unwrap().command, "npm run build");
    }

    #[test]
    fn nth_returns_none_past_the_start_of_history() {
        assert!(nth_from_end(&[], 1).is_none());
        assert!(nth_from_end(&[rec("only")], 2).is_none());
    }
}
