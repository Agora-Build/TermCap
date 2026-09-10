//! `tcap doctor` — say which backend is active and what setup is missing.
//!
//! The failure mode this exists to prevent is a cryptic error from a terminal
//! whose remote control was never enabled. Every check names the fix.

use anyhow::Result;

use crate::backend;
use crate::config::{self, Config};
use crate::state;

pub fn run(cfg: Result<Config>) -> String {
    let mut out = String::new();

    out.push_str("tcap doctor\n\n");

    // Config first: a bad one changes the meaning of everything below it, and
    // doctor is the one command that must still run when it is broken.
    out.push_str("Config\n");
    match config::path() {
        Some((path, explicit)) => {
            let via = if explicit { " (via $TCAP_CONFIG)" } else { "" };
            out.push_str(&format!("  path: {}{via}\n", path.display()));
            match &cfg {
                Ok(c) if *c == Config::default() => {
                    out.push_str("  [ok  ] not present — using built-in defaults\n");
                }
                Ok(c) => {
                    out.push_str("  [ok  ] loaded\n");
                    for (k, v) in [
                        ("backend", c.backend.clone()),
                        ("max_bytes", c.max_bytes.map(|v| v.to_string())),
                        ("format", c.format.map(|f| format!("{f:?}").to_lowercase())),
                        ("quiet", c.quiet.map(|v| v.to_string())),
                        ("copy", c.copy.map(|v| v.to_string())),
                    ] {
                        if let Some(v) = v {
                            out.push_str(&format!("         {k} = {v}\n"));
                        }
                    }
                }
                Err(e) => {
                    out.push_str(&format!("  [FAIL] {e:#}\n"));
                }
            }
        }
        None => out.push_str("  [ok  ] no HOME or XDG_CONFIG_HOME — config disabled\n"),
    }
    out.push('\n');

    // What we're running in.
    out.push_str("Environment\n");
    for (label, var) in [
        ("TERM", "TERM"),
        ("TERM_PROGRAM", "TERM_PROGRAM"),
        ("TMUX", "TMUX"),
        ("KITTY_WINDOW_ID", "KITTY_WINDOW_ID"),
        ("WEZTERM_PANE", "WEZTERM_PANE"),
        ("ITERM_SESSION_ID", "ITERM_SESSION_ID"),
    ] {
        let value = std::env::var(var).unwrap_or_else(|_| "(unset)".into());
        out.push_str(&format!("  {label:<18} {value}\n"));
    }
    out.push('\n');

    // Backend selection.
    match backend::detected_name() {
        Some(name) => {
            out.push_str(&format!("Backend: {name}\n"));
            if name == "tmux" {
                out.push_str(
                    "  tmux takes priority over the outer terminal: it renders the\n\
                     \x20 screen itself, so the terminal underneath cannot see the\n\
                     \x20 shell's real scrollback or prompt marks.\n",
                );
            }
            out.push('\n');

            match backend::detect(None) {
                Ok(b) => {
                    for d in b.diagnose() {
                        let mark = if d.ok { "ok  " } else { "FAIL" };
                        out.push_str(&format!("  [{mark}] {:<24} {}\n", d.label, d.detail));
                    }
                }
                Err(e) => out.push_str(&format!("  could not initialise backend: {e}\n")),
            }
        }
        None => {
            out.push_str("Backend: none\n\n");
            out.push_str(
                "  No supported terminal detected. tcap needs to ask the terminal\n\
                 \x20 for its scrollback, which requires a remote-control API.\n\n\
                 \x20 Supported: tmux, kitty, WezTerm, iTerm2\n\n\
                 \x20 Ghostty, Terminal.app and Alacritty expose no such API. Running\n\
                 \x20 inside tmux works in all of them:\n\n\
                 \x20   tmux\n",
            );
        }
    }
    out.push('\n');

    // Shell integration is orthogonal to the backend: kitty works without it,
    // but then there is no exit code or duration to report.
    out.push_str("Shell integration\n");
    let records = state::load();
    if records.is_empty() {
        out.push_str(&format!(
            "  [FAIL] no commands recorded for this session\n\
             \x20        (session key: {})\n\n\
             \x20        Add to ~/.zshrc:   eval \"$(tcap init zsh)\"\n\
             \x20        Then open a new shell and run a command.\n",
            state::session_key()
        ));
    } else {
        let with_coords = records.iter().filter(|r| r.start_line.is_some()).count();
        out.push_str(&format!(
            "  [ok  ] {} commands recorded ({with_coords} with pane coordinates)\n",
            records.len()
        ));
        if let Some(last) = state::nth_from_end(&records, 1) {
            out.push_str(&format!("         last: {}\n", last.command));
        }
        out.push_str(&format!("         log: {}\n", state::log_path().display()));
    }
    out.push('\n');

    // The one capability difference users actually hit.
    out.push_str("History depth\n");
    let deep = backend::detect(None)
        .map(|b| b.supports_history())
        .unwrap_or(false);
    if deep {
        out.push_str("  [ok  ] -c 2 and beyond available (tmux)\n");
    } else {
        out.push_str(
            "  [FAIL] only the most recent command is reachable\n\
             \x20        kitty and iTerm2 expose their latest command block and\n\
             \x20        nothing older. Run inside tmux for -c 2 and beyond.\n",
        );
    }

    out
}
