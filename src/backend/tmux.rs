//! tmux backend — the only one that can reach past the most recent command.
//!
//! tmux has no OSC 133 support (verified against 3.7c), so command boundaries
//! come from the shell hook.
//!
//! Coordinates: `capture-pane -S/-E` are pane-relative and shift under a line of
//! text as the pane scrolls, so the hook stores `history_size + cursor_y`
//! instead — stable, because the two move oppositely by the same amount.
//! Convert back with `relative = absolute - history_size_now`.

use anyhow::{anyhow, Result};

use super::{run, Backend, Diagnostic, Fetched};
use crate::state::Record;

pub struct Tmux {
    pane: Option<String>,
}

impl Tmux {
    pub fn detect() -> bool {
        super::has_env("TMUX")
    }

    pub fn new() -> Self {
        Self {
            pane: std::env::var("TMUX_PANE").ok().filter(|p| !p.is_empty()),
        }
    }

    fn query(&self, format: &str) -> Result<String> {
        let mut args: Vec<&str> = vec!["display-message", "-p"];
        if let Some(pane) = &self.pane {
            args.push("-t");
            args.push(pane);
        }
        args.push(format);
        Ok(run("tmux", &args)?.trim().to_string())
    }

    fn query_i64(&self, format: &str) -> Result<i64> {
        let raw = self.query(format)?;
        raw.parse::<i64>()
            .map_err(|_| anyhow!("tmux returned `{raw}` for {format}, expected a number"))
    }

    /// Lines currently held in scrollback above the visible pane.
    pub fn history_size(&self) -> Result<i64> {
        self.query_i64("#{history_size}")
    }

    /// Absolute coordinate of the cursor's row, plus its column.
    ///
    /// `#{e|+:a,b}` is tmux format arithmetic; both fields come from one format
    /// string so this costs a single subprocess, which matters on every prompt.
    pub fn anchor_and_col(&self) -> Result<(i64, i64)> {
        let raw = self.query("#{e|+:#{history_size},#{cursor_y}} #{cursor_x}")?;
        let mut parts = raw.split_whitespace();
        let anchor = parts
            .next()
            .and_then(|v| v.parse::<i64>().ok())
            .ok_or_else(|| anyhow!("tmux returned `{raw}`, expected `<anchor> <col>`"))?;
        let col = parts
            .next()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        Ok((anchor, col))
    }

    /// Last row occupied by output that has just finished printing. A non-zero
    /// column means the output had no trailing newline, so the cursor is still
    /// on its final row rather than past it.
    pub fn output_end_line(&self) -> Result<i64> {
        let (anchor, col) = self.anchor_and_col()?;
        Ok(if col > 0 { anchor } else { anchor - 1 })
    }
}

impl Backend for Tmux {
    fn name(&self) -> &'static str {
        "tmux"
    }

    fn supports_history(&self) -> bool {
        true
    }

    fn fetch(&self, _index: usize, rec: Option<&Record>, raw: bool) -> Result<Fetched> {
        let rec = rec.ok_or_else(|| {
            anyhow!(
                "no command history recorded for this pane.\n\n\
                 The tmux backend needs tcap's shell integration to know where\n\
                 each command's output starts and ends. Add to your ~/.zshrc:\n\n  \
                 eval \"$(tcap init zsh)\"\n\n\
                 then start a new shell and run a command."
            )
        })?;

        let (Some(start), Some(end)) = (rec.start_line, rec.end_line) else {
            return Err(anyhow!(
                "this command was recorded outside tmux, so it has no pane\n\
                 coordinates. Only commands run inside tmux can be captured here."
            ));
        };

        // A command that printed nothing leaves end one row above start.
        if end < start {
            return Ok(String::new().into());
        }

        let history = self.history_size()?;
        // Clamp: if the output has scrolled out of the retained history, take
        // what is left rather than asking tmux for rows it no longer holds.
        let rel_start = (start - history).max(-history);
        let rel_end = end - history;

        let (s, e) = (rel_start.to_string(), rel_end.to_string());
        let mut args: Vec<&str> = vec!["capture-pane", "-p", "-J"];
        if raw {
            // Include SGR attributes as escape sequences.
            args.push("-e");
        }
        if let Some(pane) = &self.pane {
            args.push("-t");
            args.push(pane);
        }
        args.extend_from_slice(&["-S", &s, "-E", &e]);

        Ok(run("tmux", &args)?.into())
    }

    fn diagnose(&self) -> Vec<Diagnostic> {
        let mut d = Vec::new();

        d.push(if super::on_path("tmux") {
            Diagnostic::ok("tmux binary", "found on PATH")
        } else {
            Diagnostic::bad("tmux binary", "not on PATH")
        });

        d.push(match &self.pane {
            Some(p) => Diagnostic::ok("pane", format!("$TMUX_PANE = {p}")),
            None => Diagnostic::bad(
                "pane",
                "$TMUX_PANE is unset; falling back to tmux's active pane",
            ),
        });

        match self.history_size() {
            Ok(n) => d.push(Diagnostic::ok(
                "scrollback",
                format!("{n} lines of history in this pane"),
            )),
            Err(e) => d.push(Diagnostic::bad("scrollback", e.to_string())),
        }

        let records = crate::state::load();
        d.push(if records.is_empty() {
            Diagnostic::bad(
                "shell integration",
                "no commands recorded — add `eval \"$(tcap init zsh)\"` to ~/.zshrc",
            )
        } else {
            let with_coords = records.iter().filter(|r| r.start_line.is_some()).count();
            Diagnostic::ok(
                "shell integration",
                format!(
                    "{} commands recorded, {with_coords} with pane coordinates",
                    records.len()
                ),
            )
        });

        d
    }
}
