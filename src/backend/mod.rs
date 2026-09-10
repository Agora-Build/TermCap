//! Terminal adapters.
//!
//! Each backend answers one question: "give me the text of command block N".
//! They differ enormously in what they can do — kitty knows its own command
//! boundaries and needs no help, while tmux knows nothing and relies entirely
//! on the coordinates our shell hook recorded.

use anyhow::{anyhow, Context, Result};

use crate::state::Record;

pub mod iterm2;
pub mod kitty;
pub mod tmux;
pub mod wezterm;

/// One line of `tcap doctor` output.
pub struct Diagnostic {
    pub ok: bool,
    pub label: String,
    pub detail: String,
}

impl Diagnostic {
    pub fn ok(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            ok: true,
            label: label.into(),
            detail: detail.into(),
        }
    }
    pub fn bad(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            ok: false,
            label: label.into(),
            detail: detail.into(),
        }
    }
}

/// Most backends return only text, but iTerm2's API hands back the command and
/// cwd too, which covers the gap when shell integration is not installed.
pub struct Fetched {
    pub output: String,
    pub command: Option<String>,
    pub exit_code: Option<i32>,
    pub cwd: Option<String>,
}

impl From<String> for Fetched {
    fn from(output: String) -> Self {
        Self {
            output,
            command: None,
            exit_code: None,
            cwd: None,
        }
    }
}

pub trait Backend {
    fn name(&self) -> &'static str;

    /// The command block at `index`, where 1 is the most recent.
    ///
    /// `rec` is the matching shell-integration record when we have one. Some
    /// backends require it (tmux cannot find boundaries without it); others
    /// only use it for metadata.
    fn fetch(&self, index: usize, rec: Option<&Record>, raw: bool) -> Result<Fetched>;

    /// Whether this backend can reach past the most recent command.
    ///
    /// Only tmux can. kitty and iTerm2 expose their latest command block and
    /// nothing older, so `-c 2` has to fail loudly rather than quietly return
    /// the wrong block.
    fn supports_history(&self) -> bool {
        false
    }

    fn diagnose(&self) -> Vec<Diagnostic>;
}

/// Order matters: **tmux wins whenever it is present.** Inside kitty running
/// tmux, `kitty @ get-text` returns tmux's rendered viewport rather than the
/// shell's scrollback, and tmux swallows the prompt marks `last_cmd_output`
/// relies on — so asking kitty there silently returns the wrong text.
pub fn detect(forced: Option<&str>) -> Result<Box<dyn Backend>> {
    if let Some(name) = forced {
        return match name {
            "tmux" => Ok(Box::new(tmux::Tmux::new())),
            "kitty" => Ok(Box::new(kitty::Kitty::new()?)),
            "wezterm" => Ok(Box::new(wezterm::WezTerm::new())),
            "iterm2" => Ok(Box::new(iterm2::ITerm2::new())),
            other => Err(anyhow!(
                "unknown backend `{other}` (expected tmux, kitty, wezterm or iterm2)"
            )),
        };
    }

    if tmux::Tmux::detect() {
        return Ok(Box::new(tmux::Tmux::new()));
    }
    if kitty::Kitty::detect() {
        return Ok(Box::new(kitty::Kitty::new()?));
    }
    if wezterm::WezTerm::detect() {
        return Ok(Box::new(wezterm::WezTerm::new()));
    }
    if iterm2::ITerm2::detect() {
        return Ok(Box::new(iterm2::ITerm2::new()));
    }

    Err(anyhow!(
        "no supported terminal detected.\n\n\
         tcap needs a terminal that can be asked for its scrollback. Supported:\n  \
         tmux, kitty, WezTerm, iTerm2\n\n\
         Ghostty, Terminal.app and Alacritty have no remote-control API, so they\n\
         cannot be supported this way. Running inside tmux works in any of them:\n  \
         tmux\n\n\
         Run `tcap doctor` for details."
    ))
}

/// Which backend `detect` would choose, for `doctor` to report.
pub fn detected_name() -> Option<&'static str> {
    if tmux::Tmux::detect() {
        Some("tmux")
    } else if kitty::Kitty::detect() {
        Some("kitty")
    } else if wezterm::WezTerm::detect() {
        Some("wezterm")
    } else if iterm2::ITerm2::detect() {
        Some("iterm2")
    } else {
        None
    }
}

/// Run a program and capture stdout, turning a non-zero exit into an error.
pub(crate) fn run(program: &str, args: &[&str]) -> Result<String> {
    let out = std::process::Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("could not run `{program}`"))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stderr = stderr.trim();
        return Err(anyhow!(
            "`{program}` failed{}",
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        ));
    }

    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Whether a program can be found on `PATH`.
pub(crate) fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let p = dir.join(program);
                p.is_file()
            })
        })
        .unwrap_or(false)
}

/// Whether an environment variable is set to something non-empty.
pub(crate) fn has_env(key: &str) -> bool {
    std::env::var(key).map(|v| !v.is_empty()).unwrap_or(false)
}
