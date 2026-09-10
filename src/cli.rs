//! Command-line surface.
//!
//! Two axes, each with an index and a count form, following one rule:
//! **lowercase selects the Nth from the end, uppercase takes the last N.**
//!
//! ```text
//!   commands:  -c N (Nth block from end)   -C N (last N blocks)
//!   lines:     -l N (Nth line from end)    -L N (last N lines)
//! ```
//!
//! The axes compose — `tcap -c 2 -L 50` is "the second-to-last command, showing
//! only the last 50 lines of its output". Within an axis the index and count
//! forms are mutually exclusive, which clap enforces via `conflicts_with`.

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::config::Config;

const AFTER_HELP: &str = "\
FLAG RULE:
  lowercase = the Nth from the end      uppercase = the last N
    -c 3  the 3rd-from-last command       -C 3  the last 3 commands
    -l 3  the 3rd-from-last line          -L 3  the last 3 lines

EXAMPLES:
  tcap | sgpt \"tell me how to fix it\"   pipe the last failure to an LLM
  tcap -c 2                            the command before the last one
  tcap -C 3                            the last 3 commands, in order
  tcap -c 2 -L 50                      2nd-to-last command, tail 50 lines
  tcap --json                          structured output for scripting

NOTES:
  The command and line axes combine. Line flags apply to each block's output
  individually, so `-C 3 -L 20` gives the last 20 lines of each of 3 commands.

  -c/-C beyond the most recent command needs the tmux backend; kitty and
  iTerm2 can only report their latest command. Run `tcap doctor` to check.";

#[derive(Parser, Debug)]
#[command(
    name = "tcap",
    version,
    about = "Capture the last command and its output from your terminal",
    after_help = AFTER_HELP
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Sub>,

    #[command(flatten)]
    pub select: Select,

    #[command(flatten)]
    pub format: Format,

    /// Also copy the result to the system clipboard
    #[arg(long, global = true, overrides_with = "no_copy")]
    pub copy: bool,

    /// Do not copy, overriding `copy = true` in the config file
    #[arg(long = "no-copy", global = true, overrides_with = "copy")]
    pub no_copy: bool,

    /// Suppress the capability hints printed to stderr
    #[arg(short, long, global = true, overrides_with = "no_quiet")]
    pub quiet: bool,

    /// Show hints, overriding `quiet = true` in the config file
    #[arg(long = "no-quiet", global = true, overrides_with = "quiet")]
    pub no_quiet: bool,

    /// Force a backend instead of auto-detecting [tmux, kitty, wezterm, iterm2]
    #[arg(long, value_name = "NAME", global = true, env = "TCAP_BACKEND")]
    pub backend: Option<String>,

    /// Truncate to roughly this many bytes, eliding the middle. 0 disables
    #[arg(long, value_name = "N", global = true)]
    pub max_bytes: Option<usize>,
}

/// Built-in default for `--max-bytes`, used when neither flag nor config says.
///
/// Not a clap `default_value_t`: that would make a defaulted value
/// indistinguishable from an explicitly passed one, and the config file could
/// then never win.
pub const DEFAULT_MAX_BYTES: usize = 64_000;

/// Settings after flags, environment, config and defaults are folded together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub backend: Option<String>,
    pub max_bytes: usize,
    pub mode: Mode,
    pub quiet: bool,
    pub copy: bool,
}

impl Cli {
    /// Resolve precedence: flags > environment > config file > defaults.
    ///
    /// `--backend` already folds the environment in via clap's `env`, so by the
    /// time it is `Some` it represents either a flag or `TCAP_BACKEND`.
    pub fn resolve(&self, cfg: &Config) -> Settings {
        // For the boolean pairs, `overrides_with` means the last flag on the
        // command line wins; only if neither appeared does the config apply.
        let tri = |on: bool, off: bool, configured: Option<bool>| {
            if on {
                true
            } else if off {
                false
            } else {
                configured.unwrap_or(false)
            }
        };

        Settings {
            backend: self.backend.clone().or_else(|| cfg.backend.clone()),
            max_bytes: self
                .max_bytes
                .or(cfg.max_bytes)
                .unwrap_or(DEFAULT_MAX_BYTES),
            mode: self
                .format
                .mode()
                .or_else(|| cfg.format.map(Mode::from))
                .unwrap_or(Mode::Annotated),
            quiet: tri(self.quiet, self.no_quiet, cfg.quiet),
            copy: tri(self.copy, self.no_copy, cfg.copy),
        }
    }
}

#[derive(Args, Debug, Default)]
pub struct Select {
    /// Capture the Nth command block from the end (1 = the last command)
    #[arg(short = 'c', value_name = "N", conflicts_with = "commands")]
    pub cmd_index: Option<usize>,

    /// Capture the last N command blocks
    #[arg(short = 'C', value_name = "N")]
    pub commands: Option<usize>,

    /// Keep only the Nth line from the end of each block's output
    #[arg(short = 'l', value_name = "N", conflicts_with = "last_lines")]
    pub line_index: Option<usize>,

    /// Keep only the last N lines of each block's output
    #[arg(short = 'L', value_name = "N")]
    pub last_lines: Option<usize>,
}

impl Select {
    /// How many blocks to fetch, and the index of the newest one to include.
    ///
    /// Defaults to a single block at index 1 — the command just run.
    pub fn block_range(&self) -> (usize, usize) {
        match (self.cmd_index, self.commands) {
            (_, Some(c)) => (c.max(1), 1),
            (Some(n), None) => (1, n.max(1)),
            (None, None) => (1, 1),
        }
    }
}

#[derive(Args, Debug, Default)]
#[group(multiple = false)]
pub struct Format {
    /// Print only the output, without the annotation header
    #[arg(long)]
    pub output: bool,

    /// Print only the command text (note: this is not `-c`, which selects which
    /// command to capture)
    #[arg(long = "command")]
    pub command_only: bool,

    /// Print verbatim as it appeared, ANSI colour intact, no header
    #[arg(long)]
    pub raw: bool,

    /// Print structured JSON
    #[arg(long)]
    pub json: bool,
}

/// Which rendering to use. `Annotated` is the default: command, exit code and
/// cwd above the output, which is what makes the result useful to an LLM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Annotated,
    Output,
    Command,
    Raw,
    Json,
}

impl Format {
    /// The mode a format flag asked for, or `None` when none was passed.
    ///
    /// `None` rather than defaulting to `Annotated` here, so the caller can
    /// tell "no flag given" from "explicitly annotated" and let the config file
    /// have its say.
    pub fn mode(&self) -> Option<Mode> {
        Some(if self.json {
            Mode::Json
        } else if self.raw {
            Mode::Raw
        } else if self.command_only {
            Mode::Command
        } else if self.output {
            Mode::Output
        } else {
            return None;
        })
    }
}

#[derive(Subcommand, Debug)]
pub enum Sub {
    /// Report the active backend and any setup that is missing
    Doctor,

    /// Print shell integration to eval, e.g. eval "$(tcap init zsh)"
    Init {
        #[arg(value_enum)]
        shell: ShellKind,
    },

    /// Internal: called by the shell hooks after each command
    #[command(name = "__record", hide = true)]
    Record(RecordArgs),
}

#[derive(ValueEnum, Debug, Clone, Copy)]
pub enum ShellKind {
    Zsh,
    Bash,
    Fish,
}

#[derive(Args, Debug)]
pub struct RecordArgs {
    /// The command line that was run, passed as a single argv element so no
    /// quoting or escaping is needed for arbitrary command text
    #[arg(long)]
    pub command: String,

    #[arg(long)]
    pub exit: Option<i32>,

    #[arg(long)]
    pub cwd: Option<String>,

    /// Epoch seconds (may be fractional) captured in preexec
    #[arg(long)]
    pub start_time: Option<f64>,

    /// `history_size + cursor_y` sampled in preexec, before the command ran
    #[arg(long)]
    pub start_anchor: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn defaults_to_one_block_at_index_one() {
        assert_eq!(Select::default().block_range(), (1, 1));
    }

    #[test]
    fn lowercase_c_is_an_index() {
        let s = Select {
            cmd_index: Some(3),
            ..Default::default()
        };
        assert_eq!(s.block_range(), (1, 3));
    }

    #[test]
    fn uppercase_c_is_a_count() {
        let s = Select {
            commands: Some(3),
            ..Default::default()
        };
        assert_eq!(s.block_range(), (3, 1));
    }

    #[test]
    fn case_distinguishes_index_from_count() {
        let idx = Cli::try_parse_from(["tcap", "-c", "3"]).unwrap();
        assert_eq!(idx.select.cmd_index, Some(3));
        assert_eq!(idx.select.commands, None);

        let cnt = Cli::try_parse_from(["tcap", "-C", "3"]).unwrap();
        assert_eq!(cnt.select.commands, Some(3));
        assert_eq!(cnt.select.cmd_index, None);

        let li = Cli::try_parse_from(["tcap", "-l", "3"]).unwrap();
        assert_eq!(li.select.line_index, Some(3));

        let lc = Cli::try_parse_from(["tcap", "-L", "3"]).unwrap();
        assert_eq!(lc.select.last_lines, Some(3));
    }

    #[test]
    fn axes_compose() {
        let cli = Cli::try_parse_from(["tcap", "-c", "2", "-L", "50"]).unwrap();
        assert_eq!(cli.select.cmd_index, Some(2));
        assert_eq!(cli.select.last_lines, Some(50));
    }

    #[test]
    fn index_and_count_conflict_within_an_axis() {
        assert!(Cli::try_parse_from(["tcap", "-c", "2", "-C", "3"]).is_err());
        assert!(Cli::try_parse_from(["tcap", "-l", "2", "-L", "3"]).is_err());
        assert!(Cli::try_parse_from(["tcap", "--raw", "--json"]).is_err());
    }

    #[test]
    fn short_c_and_long_command_are_distinct() {
        // `-c` selects which command; `--command` selects what to print.
        let sel = Cli::try_parse_from(["tcap", "-c", "2"]).unwrap();
        assert_eq!(sel.select.cmd_index, Some(2));
        assert!(!sel.format.command_only);

        let fmt = Cli::try_parse_from(["tcap", "--command"]).unwrap();
        assert!(fmt.format.command_only);
        assert_eq!(fmt.select.cmd_index, None);
    }

    #[test]
    fn format_flags_map_to_modes() {
        let m = |args: &[&str]| Cli::try_parse_from(args).unwrap().format.mode();
        assert_eq!(m(&["tcap"]), None, "no flag must not claim a mode");
        assert_eq!(m(&["tcap", "--output"]), Some(Mode::Output));
        assert_eq!(m(&["tcap", "--command"]), Some(Mode::Command));
        assert_eq!(m(&["tcap", "--raw"]), Some(Mode::Raw));
        assert_eq!(m(&["tcap", "--json"]), Some(Mode::Json));
    }

    // --- precedence: flags > env > config > defaults ---

    fn resolved(args: &[&str], cfg: Config) -> Settings {
        Cli::try_parse_from(args).unwrap().resolve(&cfg)
    }

    fn cfg(toml: &str) -> Config {
        crate::config::parse(toml).unwrap()
    }

    #[test]
    fn bare_defaults_apply_with_no_flags_and_no_config() {
        let s = resolved(&["tcap"], Config::default());
        assert_eq!(s.max_bytes, DEFAULT_MAX_BYTES);
        assert_eq!(s.mode, Mode::Annotated);
        assert!(!s.quiet);
        assert!(!s.copy);
        assert_eq!(s.backend, None);
    }

    #[test]
    fn config_fills_in_where_no_flag_was_given() {
        let s = resolved(
            &["tcap"],
            cfg(r#"
                backend   = "tmux"
                max_bytes = 8000
                format    = "json"
                quiet     = true
                copy      = true
            "#),
        );
        assert_eq!(s.backend.as_deref(), Some("tmux"));
        assert_eq!(s.max_bytes, 8000);
        assert_eq!(s.mode, Mode::Json);
        assert!(s.quiet);
        assert!(s.copy);
    }

    #[test]
    fn flags_beat_the_config_file() {
        let c = cfg(r#"
            backend   = "kitty"
            max_bytes = 8000
            format    = "json"
        "#);
        let s = resolved(
            &["tcap", "--backend", "tmux", "--max-bytes", "100", "--raw"],
            c,
        );
        assert_eq!(s.backend.as_deref(), Some("tmux"));
        assert_eq!(s.max_bytes, 100);
        assert_eq!(s.mode, Mode::Raw);
    }

    /// A config boolean must be escapable from the command line, otherwise
    /// `quiet = true` would be a one-way door.
    #[test]
    fn negative_flags_override_config_booleans() {
        let c = || cfg("quiet = true\ncopy = true\n");
        assert!(resolved(&["tcap"], c()).quiet);
        assert!(!resolved(&["tcap", "--no-quiet"], c()).quiet);
        assert!(!resolved(&["tcap", "--no-copy"], c()).copy);
    }

    #[test]
    fn last_boolean_flag_on_the_line_wins() {
        let c = Config::default();
        assert!(!resolved(&["tcap", "--quiet", "--no-quiet"], c.clone()).quiet);
        assert!(resolved(&["tcap", "--no-quiet", "--quiet"], c).quiet);
    }

    /// `max_bytes = 0` means "no truncation" and must survive as 0 rather than
    /// being mistaken for absent and replaced by the default.
    #[test]
    fn zero_max_bytes_from_config_is_not_treated_as_unset() {
        assert_eq!(resolved(&["tcap"], cfg("max_bytes = 0")).max_bytes, 0);
    }

    #[test]
    fn explicit_flag_value_matching_the_default_still_counts_as_set() {
        let s = resolved(&["tcap", "--max-bytes", "64000"], cfg("max_bytes = 10"));
        assert_eq!(s.max_bytes, DEFAULT_MAX_BYTES);
    }
}
