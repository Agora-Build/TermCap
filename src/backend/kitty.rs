//! kitty backend.
//!
//! kitty is the one terminal that ships this feature outright:
//! `kitty @ get-text --extent=last_cmd_output` returns exactly the last
//! command's output, using kitty's own shell-integration prompt marks. Verified
//! against kitty 0.48.2, whose help states the extent "requires
//! shell_integration to be enabled".
//!
//! The limitation is that kitty exposes only the *latest* command block. There
//! is no extent for "the one before that", so `-c 2` cannot be served here and
//! fails with a pointer to tmux rather than silently returning the wrong text.

use anyhow::{anyhow, Result};

use super::{run, Backend, Diagnostic, Fetched};
use crate::state::Record;

pub struct Kitty {
    exe: String,
}

impl Kitty {
    pub fn detect() -> bool {
        super::has_env("KITTY_WINDOW_ID")
            || std::env::var("TERM")
                .map(|t| t.contains("kitty"))
                .unwrap_or(false)
    }

    pub fn new() -> Result<Self> {
        Ok(Self {
            exe: Self::find_exe()?,
        })
    }

    /// Locate a kitty binary able to speak remote control.
    ///
    /// `kitten` is preferred (the modern entry point, put on PATH by kitty's
    /// shell integration). On macOS neither may be on PATH even inside kitty,
    /// so we also look inside the app bundle, deriving it from
    /// `KITTY_INSTALLATION_DIR` before falling back to the default location.
    fn find_exe() -> Result<String> {
        for candidate in ["kitten", "kitty"] {
            if super::on_path(candidate) {
                return Ok(candidate.to_string());
            }
        }

        // KITTY_INSTALLATION_DIR points at Contents/Resources/kitty inside the
        // bundle; the executable lives at Contents/MacOS/kitty.
        if let Ok(dir) = std::env::var("KITTY_INSTALLATION_DIR") {
            let p = std::path::Path::new(&dir);
            if let Some(contents) = p.ancestors().find(|a| a.ends_with("Contents")) {
                let exe = contents.join("MacOS").join("kitty");
                if exe.is_file() {
                    return Ok(exe.to_string_lossy().into_owned());
                }
            }
        }

        let fallback = "/Applications/kitty.app/Contents/MacOS/kitty";
        if std::path::Path::new(fallback).is_file() {
            return Ok(fallback.to_string());
        }

        Err(anyhow!(
            "could not find the kitty (or kitten) executable.\n\
             Add kitty's bin directory to PATH, or pass --backend tmux."
        ))
    }

    /// Remote-control arguments, targeting the socket when one is advertised.
    fn rc_args<'a>(&self, listen_on: &'a Option<String>) -> Vec<&'a str> {
        let mut args = vec!["@"];
        if let Some(sock) = listen_on {
            args.push("--to");
            args.push(sock);
        }
        args
    }

    fn listen_on() -> Option<String> {
        std::env::var("KITTY_LISTEN_ON")
            .ok()
            .filter(|s| !s.is_empty())
    }
}

impl Backend for Kitty {
    fn name(&self) -> &'static str {
        "kitty"
    }

    fn fetch(&self, index: usize, _rec: Option<&Record>, raw: bool) -> Result<Fetched> {
        if index > 1 {
            return Err(anyhow!(
                "kitty can only report its most recent command's output, so -c {index} \
                 is not available here.\n\n\
                 kitty exposes a `last_cmd_output` extent but nothing for older\n\
                 commands. To reach further back, run inside tmux."
            ));
        }

        let listen_on = Self::listen_on();
        let mut args = self.rc_args(&listen_on);
        args.extend_from_slice(&["get-text", "--extent", "last_cmd_output"]);
        if raw {
            args.push("--ansi");
        }

        run(&self.exe, &args).map(Fetched::from).map_err(|e| {
            anyhow!(
                "{e}\n\n\
                     kitty remote control may be disabled. Add to ~/.config/kitty/kitty.conf:\n  \
                     allow_remote_control yes\n\
                     and make sure shell integration is enabled (it is by default)."
            )
        })
    }

    fn diagnose(&self) -> Vec<Diagnostic> {
        let mut d = vec![Diagnostic::ok("kitty binary", self.exe.clone())];

        let listen_on = Self::listen_on();
        let mut args = self.rc_args(&listen_on);
        args.push("ls");

        d.push(match run(&self.exe, &args) {
            Ok(_) => Diagnostic::ok("remote control", "enabled and reachable"),
            Err(e) => Diagnostic::bad(
                "remote control",
                format!("{e} — set `allow_remote_control yes` in kitty.conf"),
            ),
        });

        // The real test of shell integration is whether the extent resolves.
        let mut probe = self.rc_args(&listen_on);
        probe.extend_from_slice(&["get-text", "--extent", "last_cmd_output"]);
        d.push(match run(&self.exe, &probe) {
            Ok(_) => Diagnostic::ok("shell integration", "last_cmd_output resolves"),
            Err(e) => Diagnostic::bad(
                "shell integration",
                format!("{e} — kitty needs shell_integration enabled for this extent"),
            ),
        });

        d.push(Diagnostic::bad(
            "history depth",
            "kitty serves only the latest command; -c 2 and beyond need tmux",
        ));

        d
    }
}
