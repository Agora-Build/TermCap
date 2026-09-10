//! kitty backend, via `kitty @ get-text` and kitty's own shell-integration
//! prompt marks. Verified against kitty 0.48.2.
//!
//! Two things this backend cannot do:
//!
//! - Reach past the latest output. There is no extent for "the command before
//!   that", so `-c 2` fails with a pointer to tmux rather than returning the
//!   wrong text.
//! - Tie output to a specific command. It asks for the last *non-empty* output
//!   (see `fetch` for why), so when the last command printed nothing, kitty
//!   returns an older command's output while the header names the last one.
//!   tmux has no such ambiguity, having real boundaries.

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

    /// Turn kitty's refusal into the fix for the mode actually in force.
    ///
    /// `allow_remote_control` has several values and they need different
    /// answers: telling a `socket-only` user to set `yes` is wrong advice, and
    /// its real cause — no socket to talk to — is not obvious from kitty's
    /// message alone.
    fn remote_control_hint(err: &anyhow::Error, listen_on: &Option<String>) -> String {
        let msg = err.to_string().to_lowercase();

        if msg.contains("socket only") || msg.contains("socket-only") {
            return match listen_on {
                None => "kitty is set to `allow_remote_control socket-only`, but \
                     $KITTY_LISTEN_ON is unset, so there is no socket to use.\n\
                     Add to ~/.config/kitty/kitty.conf and restart kitty:\n  \
                     listen_on unix:/tmp/kitty-{kitty_pid}\n\
                     If it is set in your shell but not here, something in between is \
                     clearing the environment."
                    .to_string(),
                Some(sock) => format!(
                    "the socket at {sock} was refused. Restart kitty so the running \
                     instance and $KITTY_LISTEN_ON agree."
                ),
            };
        }

        if msg.contains("password") {
            return "kitty is requiring a remote-control password, which tcap does not \
                 send. Allow this one command without a password in kitty.conf:\n  \
                 remote_control_password \"\" get-text"
                .to_string();
        }

        "kitty remote control appears to be disabled. In ~/.config/kitty/kitty.conf \
         either:\n  allow_remote_control yes\nor keep it restricted and give tcap a \
         socket:\n  allow_remote_control socket-only\n  listen_on unix:/tmp/kitty-{kitty_pid}\n\
         Then restart kitty."
            .to_string()
    }

    /// `--match id:N` for the window tcap is running in.
    ///
    /// Without it kitty reads whichever window is *focused*, which is not
    /// necessarily this one — a capture triggered from a script, or with focus
    /// on another split, silently returns another window's text.
    fn window_match() -> Option<String> {
        std::env::var("KITTY_WINDOW_ID")
            .ok()
            .filter(|s| !s.is_empty())
            .map(|id| format!("id:{id}"))
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
                 kitty exposes only the latest output and nothing for older\n\
                 commands. To reach further back, run inside tmux."
            ));
        }

        let listen_on = Self::listen_on();
        let window = Self::window_match();
        let mut args = self.rc_args(&listen_on);
        args.push("get-text");
        // `--match` is a get-text option, not a global one: before the
        // subcommand kitty rejects it with "Unknown option: --match".
        if let Some(w) = &window {
            args.extend_from_slice(&["--match", w]);
        }
        // `last_non_empty_output`, not `last_cmd_output`.
        //
        // kitty counts the command currently executing — tcap itself — as the
        // last command, so `last_cmd_output` returns tcap's own output, which is
        // empty. That extent is meant for a keybinding pressed at the prompt,
        // where nothing is running. Skipping empty output steps back over
        // tcap's own invocation to the command the user actually cares about.
        args.extend_from_slice(&["--extent", "last_non_empty_output"]);
        if raw {
            args.push("--ansi");
        }

        run(&self.exe, &args)
            .map(Fetched::from)
            .map_err(|e| anyhow!("{e}\n\n{}", Self::remote_control_hint(&e, &listen_on)))
    }

    fn diagnose(&self) -> Vec<Diagnostic> {
        let mut d = vec![Diagnostic::ok("kitty binary", self.exe.clone())];

        let listen_on = Self::listen_on();
        let mut args = self.rc_args(&listen_on);
        args.push("ls");

        let channel = match &listen_on {
            Some(sock) => format!("socket {sock}"),
            None => "escape-code channel (no $KITTY_LISTEN_ON)".to_string(),
        };
        d.push(match run(&self.exe, &args) {
            Ok(_) => Diagnostic::ok("remote control", format!("reachable via {channel}")),
            Err(e) => Diagnostic::bad(
                "remote control",
                format!("{}\n{}", channel, Self::remote_control_hint(&e, &listen_on)),
            ),
        });

        // The real test of shell integration is whether the extent resolves.
        let window = Self::window_match();
        let mut probe = self.rc_args(&listen_on);
        probe.push("get-text");
        if let Some(w) = &window {
            probe.extend_from_slice(&["--match", w]);
        }
        probe.extend_from_slice(&["--extent", "last_non_empty_output"]);
        d.push(match run(&self.exe, &probe) {
            Ok(t) if t.trim().is_empty() => Diagnostic::bad(
                "shell integration",
                "the extent resolved but returned nothing — kitty's own shell \
                 integration is probably off (`shell_integration enabled` in kitty.conf)",
            ),
            Ok(_) => Diagnostic::ok("shell integration", "last_non_empty_output resolves"),
            Err(e) => Diagnostic::bad(
                "shell integration",
                format!("{e} — kitty needs shell_integration enabled for this extent"),
            ),
        });

        d.push(match Self::window_match() {
            Some(w) => Diagnostic::ok("window", w),
            None => Diagnostic::bad(
                "window",
                "$KITTY_WINDOW_ID unset — kitty will read the focused window",
            ),
        });

        d.push(Diagnostic::bad(
            "history depth",
            "kitty serves only the latest command; -c 2 and beyond need tmux",
        ));

        d
    }
}
