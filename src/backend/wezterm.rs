//! WezTerm backend — detection only.
//!
//! `wezterm cli get-text` exists, so this is implementable, but WezTerm exposes
//! no `history_size` equivalent for converting our absolute coordinates back to
//! pane-relative ones, and the `--start-line`/`--end-line` semantics could not
//! be verified (WezTerm is not installed here). A guess would return
//! plausible-looking but wrong text, so detection is wired up only far enough
//! for `doctor` to say so and point at tmux.

use anyhow::{anyhow, Result};

use super::{Backend, Diagnostic, Fetched};
use crate::state::Record;

pub struct WezTerm;

impl WezTerm {
    pub fn detect() -> bool {
        super::has_env("WEZTERM_PANE")
            || std::env::var("TERM_PROGRAM")
                .map(|t| t == "WezTerm")
                .unwrap_or(false)
    }

    pub fn new() -> Self {
        Self
    }
}

impl Backend for WezTerm {
    fn name(&self) -> &'static str {
        "wezterm"
    }

    fn fetch(&self, _index: usize, _rec: Option<&Record>, _raw: bool) -> Result<Fetched> {
        Err(anyhow!(
            "WezTerm support is not implemented yet.\n\n\
             `wezterm cli get-text` can dump a pane, but tcap has no verified way\n\
             to map a recorded command boundary onto WezTerm's line numbering, so\n\
             it would return the wrong region rather than fail.\n\n\
             Workaround: run inside tmux, which tcap supports fully."
        ))
    }

    fn diagnose(&self) -> Vec<Diagnostic> {
        vec![
            Diagnostic::ok("detected", "WezTerm session"),
            Diagnostic::bad(
                "support",
                "not implemented in this version — run inside tmux instead",
            ),
            if super::on_path("wezterm") {
                Diagnostic::ok("wezterm binary", "found on PATH")
            } else {
                Diagnostic::bad("wezterm binary", "not on PATH")
            },
        ]
    }
}
