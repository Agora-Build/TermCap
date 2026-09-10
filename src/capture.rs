//! The unit `tcap` produces: one command, its output, and what we know about it.

use serde::Serialize;

/// A single captured command block.
///
/// The metadata is optional because kitty and iTerm2 can return output with no
/// shell hook installed — in which case there is no exit code or duration.
#[derive(Debug, Clone, Serialize)]
pub struct Capture {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    pub output: String,
    pub source: String,
    /// True when the backend cannot tie this output to the named command, so
    /// the metadata above may describe a different command than the body.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub approximate: bool,
}

impl Capture {
    pub fn new(output: String, source: &str) -> Self {
        Self {
            command: None,
            exit_code: None,
            cwd: None,
            duration_ms: None,
            output,
            source: source.to_string(),
            approximate: false,
        }
    }

    pub fn has_metadata(&self) -> bool {
        self.command.is_some()
            || self.exit_code.is_some()
            || self.cwd.is_some()
            || self.duration_ms.is_some()
    }
}
