//! iTerm2 backend.
//!
//! iTerm2 has the richest source of all — `async_get_last_prompt` hands back the
//! command text, its output range and the working directory in one call — but
//! its API is protobuf over a websocket, so we drive it through a small Python
//! shim using iTerm2's own client library rather than reimplementing the
//! protocol.
//!
//! Setup is correspondingly heavy: Shell Integration, the `iterm2` pip package,
//! and an auth cookie that iTerm2 vends over AppleScript (a one-time macOS
//! permission prompt). `diagnose` checks each of those separately so `doctor`
//! can say which one is missing.
//!
//! **Unverified:** iTerm2 is not installed on the machine this was written on,
//! so this adapter is written to the documented API but has not been run.

use anyhow::{anyhow, Result};

use super::{Backend, Diagnostic, Fetched};
use crate::state::Record;

const SHIM: &str = include_str!("../../python/iterm2_capture.py");

const E_NO_MODULE: i32 = 3;
const E_NO_PROMPT: i32 = 4;
const E_NO_CONNECT: i32 = 5;

#[derive(serde::Deserialize)]
struct ShimResult {
    output: String,
    command: Option<String>,
    exit_code: Option<i32>,
    cwd: Option<String>,
}

pub struct ITerm2;

impl ITerm2 {
    pub fn detect() -> bool {
        std::env::var("TERM_PROGRAM")
            .map(|t| t == "iTerm.app")
            .unwrap_or(false)
            || std::env::var("LC_TERMINAL")
                .map(|t| t == "iTerm2")
                .unwrap_or(false)
    }

    pub fn new() -> Self {
        Self
    }

    /// Materialise the shim next to our state so we are not re-writing it into
    /// a world-writable temp path on every call.
    fn shim_path() -> Result<std::path::PathBuf> {
        let dir = crate::state::state_dir();
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("iterm2_capture.py");
        let stale = std::fs::read_to_string(&path)
            .map(|existing| existing != SHIM)
            .unwrap_or(true);
        if stale {
            std::fs::write(&path, SHIM)?;
        }
        Ok(path)
    }

    fn call_shim(raw: bool) -> Result<ShimResult> {
        let script = Self::shim_path()?;

        let out = std::process::Command::new("python3")
            .arg(&script)
            .env("TCAP_ITERM2_ANSI", if raw { "1" } else { "0" })
            .output()
            .map_err(|e| anyhow!("could not run python3: {e}"))?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(match out.status.code() {
                Some(E_NO_MODULE) => anyhow!(
                    "the iTerm2 Python library is missing. Install it with:\n  \
                     python3 -m pip install --user iterm2"
                ),
                Some(E_NO_PROMPT) => anyhow!(
                    "iTerm2 has no prompt information for this session.\n\n\
                     Install Shell Integration from the iTerm2 menu:\n  \
                     iTerm2 > Install Shell Integration\n\n\
                     ({stderr})"
                ),
                Some(E_NO_CONNECT) => anyhow!(
                    "could not reach the iTerm2 API.\n\n\
                     Enable it under Preferences > General > Magic > \
                     \"Enable Python API\",\n\
                     and accept the permission prompt the first time tcap runs.\n\n\
                     ({stderr})"
                ),
                _ => anyhow!("iTerm2 capture failed: {stderr}"),
            });
        }

        let stdout = String::from_utf8_lossy(&out.stdout);
        serde_json::from_str(stdout.trim())
            .map_err(|e| anyhow!("could not parse the iTerm2 shim's output: {e}"))
    }
}

impl Backend for ITerm2 {
    fn name(&self) -> &'static str {
        "iterm2"
    }

    fn fetch(&self, index: usize, _rec: Option<&Record>, raw: bool) -> Result<Fetched> {
        if index > 1 {
            return Err(anyhow!(
                "iTerm2 exposes only its most recent prompt, so -c {index} is not \
                 available here.\n\n\
                 To reach further back, run inside tmux."
            ));
        }

        // iTerm2's Prompt object carries the command, cwd and exit status, so
        // this backend can annotate output even with no shell integration
        // installed on tcap's side.
        let r = Self::call_shim(raw)?;
        Ok(Fetched {
            output: r.output,
            command: r.command,
            exit_code: r.exit_code,
            cwd: r.cwd,
        })
    }

    fn diagnose(&self) -> Vec<Diagnostic> {
        let mut d = vec![Diagnostic::ok("detected", "iTerm2 session")];

        d.push(if super::on_path("python3") {
            Diagnostic::ok("python3", "found on PATH")
        } else {
            Diagnostic::bad("python3", "not on PATH — required for the iTerm2 backend")
        });

        let module = std::process::Command::new("python3")
            .args(["-c", "import iterm2"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        d.push(if module {
            Diagnostic::ok("iterm2 package", "importable")
        } else {
            Diagnostic::bad(
                "iterm2 package",
                "missing — install with `python3 -m pip install --user iterm2`",
            )
        });

        d.push(if super::has_env("ITERM_SESSION_ID") {
            Diagnostic::ok("session id", "$ITERM_SESSION_ID is set")
        } else {
            Diagnostic::bad(
                "session id",
                "$ITERM_SESSION_ID unset — tcap will fall back to the focused session",
            )
        });

        // The end-to-end check: this exercises auth and shell integration.
        d.push(match Self::call_shim(false) {
            Ok(_) => Diagnostic::ok("api + shell integration", "capture succeeded"),
            Err(e) => Diagnostic::bad("api + shell integration", e.to_string()),
        });

        d.push(Diagnostic::bad(
            "history depth",
            "iTerm2 serves only the latest command; -c 2 and beyond need tmux",
        ));

        d
    }
}
