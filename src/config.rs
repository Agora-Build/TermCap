//! Optional TOML config at `~/.config/tcap/config.toml`.
//!
//! Defaults only: `flags > environment > config file > built-in defaults`.
//!
//! TOML over YAML because these are flat scalars, and YAML's hand-editing
//! hazards all apply here — significant whitespace, and `quiet: no` parsing as
//! `false`.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use crate::cli::Mode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FormatName {
    Annotated,
    Output,
    Command,
    Raw,
    Json,
}

impl From<FormatName> for Mode {
    fn from(f: FormatName) -> Self {
        match f {
            FormatName::Annotated => Mode::Annotated,
            FormatName::Output => Mode::Output,
            FormatName::Command => Mode::Command,
            FormatName::Raw => Mode::Raw,
            FormatName::Json => Mode::Json,
        }
    }
}

/// `deny_unknown_fields` is deliberate: a mistyped key that is silently ignored
/// leaves the user believing a setting applies when it does not.
#[derive(Debug, Default, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub backend: Option<String>,
    /// 0 disables truncation.
    pub max_bytes: Option<usize>,
    pub format: Option<FormatName>,
    pub quiet: Option<bool>,
    pub copy: Option<bool>,
}

/// Returns `(path, explicit)`; `explicit` marks a path named via `TCAP_CONFIG`,
/// which changes how a missing file is treated.
pub fn path() -> Option<(PathBuf, bool)> {
    if let Some(p) = std::env::var_os("TCAP_CONFIG") {
        if !p.is_empty() {
            return Some((PathBuf::from(p), true));
        }
    }

    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;

    Some((base.join("tcap").join("config.toml"), false))
}

/// A missing file at the default location yields defaults; one named by
/// `TCAP_CONFIG` is an error, since ignoring it would hide a typo in the
/// variable itself.
pub fn load() -> Result<Config> {
    let Some((path, explicit)) = path() else {
        return Ok(Config::default());
    };

    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !explicit => {
            return Ok(Config::default());
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(anyhow!(
                "no config at {} (named by $TCAP_CONFIG)",
                path.display()
            ));
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };

    parse(&text).with_context(|| format!("in {}", path.display()))
}

/// The error passes through untouched: serde already underlines the offending
/// span and lists every valid key or variant.
pub fn parse(text: &str) -> Result<Config> {
    Ok(toml::from_str(text)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_is_all_defaults() {
        assert_eq!(parse("").unwrap(), Config::default());
    }

    #[test]
    fn parses_every_key() {
        let cfg = parse(
            r#"
            backend   = "tmux"
            max_bytes = 8000
            format    = "json"
            quiet     = true
            copy      = false
            "#,
        )
        .unwrap();

        assert_eq!(cfg.backend.as_deref(), Some("tmux"));
        assert_eq!(cfg.max_bytes, Some(8000));
        assert_eq!(cfg.format, Some(FormatName::Json));
        assert_eq!(cfg.quiet, Some(true));
        assert_eq!(cfg.copy, Some(false));
    }

    #[test]
    fn partial_config_leaves_the_rest_unset() {
        let cfg = parse("max_bytes = 100").unwrap();
        assert_eq!(cfg.max_bytes, Some(100));
        assert_eq!(cfg.backend, None);
        assert_eq!(cfg.format, None);
    }

    #[test]
    fn unknown_key_is_rejected_and_names_the_alternatives() {
        let err = parse("max_byte = 100").unwrap_err().to_string();
        assert!(err.contains("max_byte"), "should quote the bad key: {err}");
        assert!(err.contains("max_bytes"), "should list valid keys: {err}");
    }

    #[test]
    fn unknown_format_is_rejected_and_names_the_alternatives() {
        let err = parse(r#"format = "pretty""#).unwrap_err().to_string();
        assert!(err.contains("pretty"), "should quote the bad value: {err}");
        assert!(
            err.contains("annotated"),
            "should list valid formats: {err}"
        );
    }

    #[test]
    fn zero_max_bytes_is_accepted_as_unlimited() {
        assert_eq!(parse("max_bytes = 0").unwrap().max_bytes, Some(0));
    }

    /// The reason this file is TOML: YAML would read a bare `no` as `false`.
    #[test]
    fn bare_no_is_not_silently_a_boolean() {
        assert!(parse("quiet = no").is_err());
    }
}
