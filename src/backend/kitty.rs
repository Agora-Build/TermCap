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

    /// The socket recommendation, given once so the three places that offer it
    /// cannot drift apart.
    ///
    /// Two things this text has to get right, because it exists to be pasted
    /// into kitty.conf:
    ///
    /// - **No trailing comments.** kitty takes the rest of the line as the
    ///   value; `#` only opens a comment at the start of a line. Verified: a
    ///   `listen_on unix:/tmp/sock   # note` produced a socket literally named
    ///   `/tmp/sock   # note-69755`.
    /// - **One `listen_on` line.** Last one wins, so offering both platforms'
    ///   lines would hand a macOS user the Linux one — where
    ///   `$XDG_RUNTIME_DIR` is a systemd convention that is normally unset and
    ///   expands to nothing, putting the socket at the filesystem root.
    fn socket_advice() -> String {
        // macOS $TMPDIR is already per-user and 0700; $XDG_RUNTIME_DIR is the
        // equivalent on Linux.
        let dir = if cfg!(target_os = "macos") {
            // Already per-user and 0700.
            Some("${TMPDIR}".to_string())
        } else {
            // Only when it is actually set: it is a systemd convention, absent
            // on non-systemd distros, in containers and under some su/ssh paths,
            // where it would expand to nothing and put the socket at /.
            std::env::var("XDG_RUNTIME_DIR")
                .ok()
                .filter(|v| !v.is_empty())
                .map(|_| "${XDG_RUNTIME_DIR}".to_string())
        };

        let line = match &dir {
            Some(d) => format!("  listen_on unix:{d}/kitty-{{kitty_pid}}"),
            None => "  listen_on unix:/run/user/$(id -u)/kitty-{kitty_pid}   <- or any \
                 directory only you can open; $XDG_RUNTIME_DIR is unset here"
                .to_string(),
        };

        format!(
            "  allow_remote_control socket-only\n{line}\n\
             Keep the socket in a directory only you can open. One in /tmp is reachable by \
             anyone with write permission on it, which under a group-writable umask is more \
             than just you. Note kitty.conf has no trailing comments: everything after the \
             value is part of it."
        )
    }

    /// Turn kitty's refusal into the fix for the mode actually in force.
    ///
    /// `allow_remote_control` has several values needing different answers:
    /// telling a `socket-only` user to set `yes` is wrong advice, and the real
    /// cause — no socket to talk to — is not obvious from kitty's message.
    fn remote_control_hint(err: &anyhow::Error, listen_on: &Option<String>) -> String {
        let msg = err.to_string().to_lowercase();

        if msg.contains("password") {
            // Deliberately not `remote_control_password "" get-text`: that would
            // remove authentication from the one call that dumps terminal
            // contents, for every process that can reach the socket, to work
            // around tcap's own limitation. Better to be unsupported.
            return format!(
                "kitty is requiring a remote-control password, which tcap cannot send, so \
                 this setup is unsupported.\n\
                 Rather than weaken authentication for `get-text` — which returns whatever \
                 is on screen, credentials included — run inside tmux, which needs no kitty \
                 remote control at all. If you would rather use a socket:\n{}",
                Self::socket_advice()
            );
        }

        if msg.contains("no matching") || msg.contains("no window") {
            return "kitty could not find the window tcap asked for.\n\
                 $KITTY_LISTEN_ON probably points at a different kitty instance — a \
                 `listen_on` without {kitty_pid} is shared between instances, so window ids \
                 from one do not exist in another. Give each instance its own socket:\n"
                .to_string()
                + &Self::socket_advice();
        }

        // Checked after `password`, so a password refusal that happens to name
        // the socket it arrived on is not answered with "restart kitty".
        //
        // Matched on the bare word rather than a phrase: kitty 0.48.2 says
        // "Remote control is allowed over a socket only" (captured live), but
        // word order is not worth depending on across versions, and by here we
        // already know it is a remote-control failure.
        if msg.contains("socket") {
            return match listen_on {
                // "socket" is one word out of kitty's message, so this is an
                // inference, not a reading of the config — hedged accordingly.
                None => format!(
                    "This looks like `allow_remote_control socket-only` with \
                     $KITTY_LISTEN_ON unset, leaving no socket to use.\n\
                     Add to ~/.config/kitty/kitty.conf and restart kitty:\n{}\n\
                     If it is set in your shell but not here, something in between is \
                     clearing the environment.",
                    Self::socket_advice()
                ),
                Some(sock) => format!(
                    "the socket at {sock} was refused. Restart kitty so the running \
                     instance and $KITTY_LISTEN_ON agree."
                ),
            };
        }

        format!(
            "If this is a remote-control problem, kitty needs it enabled. In \
             ~/.config/kitty/kitty.conf either:\n  allow_remote_control yes\nor keep it \
             restricted and give tcap a socket:\n{}\nThen restart kitty. If the message \
             above says something else, that is the real cause.",
            Self::socket_advice()
        )
    }

    /// `--match id:N` for the window tcap is running in.
    ///
    /// Without it kitty reads whichever window is *focused*, which is not
    /// necessarily this one — a capture from a script, or with focus on another
    /// split, returns that window's text instead. Callers must treat `None` as
    /// fatal rather than omitting the flag: the fallback would put another
    /// pane's scrollback, secrets included, under this command's header.
    ///
    /// The guarantee is bounded: `id:N` is instance-local, so it identifies this
    /// window within whichever instance `--to` reaches. A stale or shared
    /// $KITTY_LISTEN_ON could still point at a different instance.
    ///
    /// `--match state:self` was measured as an alternative and rejected: it
    /// resolves the invoking window correctly even when unfocused, but still
    /// fails without $KITTY_WINDOW_ID ("No matching windows for expression:
    /// state:self"), so it hides the same dependency behind a worse message.
    ///
    /// The id is parsed as a number so a poisoned variable cannot widen the
    /// match expression, and so a malformed one is reported as such instead of
    /// surfacing later as an unexplained remote-control failure.
    fn window_match() -> Option<String> {
        Self::parse_window_id(&std::env::var("KITTY_WINDOW_ID").ok()?)
    }

    /// Split out so the numeric guarantee is testable without touching the
    /// environment, which is racy across threads.
    fn parse_window_id(raw: &str) -> Option<String> {
        raw.trim().parse::<u64>().ok().map(|id| format!("id:{id}"))
    }

    fn require_window() -> Result<String> {
        Self::window_match().ok_or_else(|| {
            anyhow!(
                "$KITTY_WINDOW_ID is missing or not a number, so tcap cannot tell kitty\n\
                 which window to read.\n\n\
                 Without it kitty returns whichever window is focused, which may be a\n\
                 different pane — tcap refuses rather than risk showing you another\n\
                 window's output, which could contain secrets.\n\n\
                 kitty sets this automatically; if it is missing, the environment was\n\
                 cleared somewhere between kitty and this shell."
            )
        })
    }

    /// The full `get-text` argv, shared by `fetch` and `diagnose`.
    ///
    /// Built once so the two cannot drift: if they disagreed, `doctor` would
    /// report the capture path healthy while `fetch` failed — the worst outcome
    /// for a tool whose job is telling you what is wrong.
    fn get_text_args<'a>(
        &self,
        listen_on: &'a Option<String>,
        window: &'a str,
        ansi: bool,
    ) -> Vec<&'a str> {
        let mut args = self.rc_args(listen_on);
        args.push("get-text");
        // `--match` is a get-text option, not a global one: placed before the
        // subcommand kitty rejects it with "Unknown option: --match".
        args.extend_from_slice(&["--match", window]);
        // `last_non_empty_output`, not `last_cmd_output`: kitty counts the
        // running tcap as the current command, so the latter returns tcap's own
        // (empty) output. That extent is for a keybinding pressed at the prompt.
        args.extend_from_slice(&["--extent", "last_non_empty_output"]);
        if ansi {
            args.push("--ansi");
        }
        args
    }
}

impl Backend for Kitty {
    fn name(&self) -> &'static str {
        "kitty"
    }

    /// kitty returns the last non-empty output, which need not belong to the
    /// command tcap's shell record names.
    fn has_command_boundaries(&self) -> bool {
        false
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
        let window = Self::require_window()?;
        let args = self.get_text_args(&listen_on, &window, raw);

        let text = run(&self.exe, &args)
            .map_err(|e| anyhow!("{e}\n\n{}", Self::remote_control_hint(&e, &listen_on)))?;

        // `run` succeeding with an empty body would render as a bare header with
        // nothing under it — the exact symptom this backend was fixed for — so
        // it fails with the same explanation doctor gives.
        if text.trim().is_empty() {
            return Err(anyhow!(
                "kitty returned no output for this window.\n\n\
                 Nothing has printed here yet, or kitty's own shell integration is off.\n\
                 Run a command that prints something and try again; if it stays empty,\n\
                 check `shell_integration` in ~/.config/kitty/kitty.conf."
            ));
        }

        Ok(Fetched::from(text))
    }

    fn diagnose(&self) -> Vec<Diagnostic> {
        let mut d = vec![Diagnostic::ok("kitty binary", self.exe.clone())];

        let listen_on = Self::listen_on();
        let channel = match &listen_on {
            Some(sock) => format!("socket {sock}"),
            None => "escape-code channel (no $KITTY_LISTEN_ON)".to_string(),
        };

        let mut args = self.rc_args(&listen_on);
        args.push("ls");
        // Probed once: running it twice costs a second round trip and lets the
        // two results disagree, so the get-text probe could be skipped while
        // remote control was reported healthy, or vice versa.
        let ls = run(&self.exe, &args);
        let reachable = ls.is_ok();
        d.push(match ls {
            Ok(_) => Diagnostic::ok("remote control", format!("reachable via {channel}")),
            // The underlying error is kept: the hint is a guess from keywords,
            // and for anything it does not recognise the real message is the
            // only useful thing doctor can say.
            Err(e) => Diagnostic::bad(
                "remote control",
                format!(
                    "{e}\n         via {channel}\n         {}",
                    Self::remote_control_hint(&e, &listen_on)
                ),
            ),
        });

        match Self::window_match() {
            Some(w) => {
                d.push(Diagnostic::ok("window", w.clone()));

                // Probe exactly what fetch runs, so doctor cannot report a path
                // healthy that fetch would fail on. Only the probe is skipped
                // when remote control is already down — an early return here
                // would also drop the diagnostics below it.
                if reachable {
                    let probe = self.get_text_args(&listen_on, &w, false);
                    d.push(match run(&self.exe, &probe) {
                        // Empty is also what a brand-new window returns, so this
                        // must not call a healthy setup broken — doctor is only
                        // useful if its verdicts can be trusted.
                        Ok(t) if t.trim().is_empty() => Diagnostic::bad(
                            "shell integration",
                            "no output to read yet. Run a command that prints something and \
                             re-run doctor; if it is still empty, check `shell_integration` \
                             in kitty.conf",
                        ),
                        Ok(_) => {
                            Diagnostic::ok("shell integration", "last_non_empty_output resolves")
                        }
                        Err(e) => Diagnostic::bad("shell integration", e.to_string()),
                    });
                }
            }
            None => d.push(Diagnostic::bad(
                "window",
                "$KITTY_WINDOW_ID missing or not a number — tcap will refuse to capture \
                 rather than read whichever window is focused",
            )),
        }

        d.push(Diagnostic::bad(
            "history depth",
            "kitty serves only the latest output; -c 2 and beyond need tmux",
        ));

        d
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kitty() -> Kitty {
        Kitty {
            exe: "kitten".into(),
        }
    }

    /// Locks in the two argv facts that were wrong in the field: the extent, and
    /// `--match` following the subcommand rather than preceding it.
    #[test]
    fn get_text_argv_order_and_extent() {
        let k = kitty();
        let args = k.get_text_args(&None, "id:7", false);
        assert_eq!(
            args,
            vec![
                "@",
                "get-text",
                "--match",
                "id:7",
                "--extent",
                "last_non_empty_output"
            ]
        );

        let sub = args.iter().position(|a| *a == "get-text").unwrap();
        let m = args.iter().position(|a| *a == "--match").unwrap();
        assert!(m > sub, "--match is a get-text option, not a global one");
        assert!(
            !args.contains(&"last_cmd_output"),
            "last_cmd_output returns tcap's own empty output"
        );
    }

    #[test]
    fn socket_is_targeted_before_the_subcommand() {
        let k = kitty();
        let sock = Some("unix:/tmp/kitty-1".to_string());
        let args = k.get_text_args(&sock, "id:7", false);
        assert_eq!(&args[..3], &["@", "--to", "unix:/tmp/kitty-1"]);
        assert_eq!(
            args[3], "get-text",
            "--to is global, so it precedes get-text"
        );
    }

    #[test]
    fn ansi_only_when_raw() {
        let k = kitty();
        assert!(!k.get_text_args(&None, "id:1", false).contains(&"--ansi"));
        assert!(k.get_text_args(&None, "id:1", true).contains(&"--ansi"));
    }

    /// A non-numeric id must be rejected rather than widening the match.
    #[test]
    fn window_match_requires_a_number() {
        let cases = [
            ("7", Some("id:7")),
            (" 7 ", Some("id:7")),
            ("", None),
            ("all", None),
            ("1 or 2", None),
        ];
        for (raw, want) in cases {
            // Must call the production parser: reimplementing it here would
            // assert only that this line does what this line does, while being
            // the sole test guarding the cross-window guarantee.
            assert_eq!(
                Kitty::parse_window_id(raw).as_deref(),
                want,
                "input {raw:?}"
            );
        }
    }

    #[test]
    fn hint_matches_the_mode_in_force() {
        let socket_err = anyhow!("Error: Remote control is allowed over a socket only");

        // socket-only with no socket: the fix is listen_on, never `yes`.
        let no_sock = Kitty::remote_control_hint(&socket_err, &None);
        assert!(no_sock.contains("listen_on"), "{no_sock}");
        assert!(
            !no_sock.contains("allow_remote_control yes"),
            "must not tell a socket-only user to loosen the setting: {no_sock}"
        );

        // socket-only with a socket that was refused: restart, not reconfigure.
        let with_sock = Kitty::remote_control_hint(&socket_err, &Some("unix:/tmp/k".into()));
        assert!(with_sock.contains("unix:/tmp/k"), "{with_sock}");

        let pw = Kitty::remote_control_hint(&anyhow!("Error: password required"), &None);
        assert!(pw.contains("unsupported"), "{pw}");

        // Anything unrecognised must hedge rather than assert a cause.
        let other = Kitty::remote_control_hint(&anyhow!("Error: Unknown option: --match"), &None);
        assert!(
            other.contains("If this is a remote-control problem"),
            "{other}"
        );
    }
}
