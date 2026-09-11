//! Turning captures into the text that gets piped to an LLM.

use crate::ansi;
use crate::capture::Capture;
use crate::cli::{Mode, Select};

/// Apply the line-axis selection to a block of text.
///
/// `-L n` keeps the last `n` lines; `-l n` keeps only the `n`th line from the
/// end. Both count from the end, so they are stable as output grows.
pub fn apply_line_selection(text: &str, select: &Select) -> String {
    let lines: Vec<&str> = text.split('\n').collect();

    if let Some(n) = select.line_index {
        let n = n.max(1);
        return match lines.len().checked_sub(n) {
            Some(i) => lines[i].to_string(),
            None => String::new(),
        };
    }

    if let Some(n) = select.last_lines {
        let n = n.max(1);
        let start = lines.len().saturating_sub(n);
        return lines[start..].join("\n");
    }

    text.to_string()
}

/// Elide the middle of `text` so it fits roughly `max_bytes`.
///
/// Both ends carry signal — the command's opening lines and the error it died
/// on — so we keep both and cut the middle. A silent truncation would be worse
/// than a visible one, hence the explicit marker. `max_bytes == 0` disables.
pub fn truncate_middle(text: &str, max_bytes: usize) -> String {
    if max_bytes == 0 || text.len() <= max_bytes {
        return text.to_string();
    }

    let lines: Vec<&str> = text.split('\n').collect();
    // Reserve room for the marker itself.
    let budget = max_bytes.saturating_sub(64);
    let half = budget / 2;

    let mut head = Vec::new();
    let mut used = 0usize;
    for l in &lines {
        if used + l.len() + 1 > half {
            break;
        }
        used += l.len() + 1;
        head.push(*l);
    }

    let mut tail = Vec::new();
    let mut used_tail = 0usize;
    for l in lines.iter().rev() {
        if head.len() + tail.len() >= lines.len() {
            break;
        }
        if used_tail + l.len() + 1 > budget.saturating_sub(used) {
            break;
        }
        used_tail += l.len() + 1;
        tail.push(*l);
    }
    tail.reverse();

    let omitted = lines.len().saturating_sub(head.len() + tail.len());
    if omitted == 0 {
        return text.to_string();
    }

    format!(
        "{}\n\n[... {} lines omitted by tcap --max-bytes ...]\n\n{}",
        head.join("\n"),
        omitted,
        tail.join("\n")
    )
}

fn shorten_home(path: &str) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy().to_string();
        if !home.is_empty() && path.starts_with(&home) {
            return format!("~{}", &path[home.len()..]);
        }
    }
    path.to_string()
}

fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m {}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

/// The annotation block above the output.
///
/// The exit code is the highest-signal item for an LLM diagnosing a failure, so
/// it is shown whenever known — including `0`, which tells the model the command
/// actually succeeded and the problem is elsewhere.
fn header(cap: &Capture) -> String {
    let mut s = String::new();

    if let Some(cmd) = &cap.command {
        s.push_str(&format!("$ {cmd}\n"));
    }

    let mut meta = Vec::new();
    if let Some(code) = cap.exit_code {
        meta.push(format!("exit: {code}"));
    }
    if let Some(cwd) = &cap.cwd {
        meta.push(format!("cwd: {}", shorten_home(cwd)));
    }
    if let Some(ms) = cap.duration_ms {
        meta.push(format!("took: {}", format_duration(ms)));
    }
    if !meta.is_empty() {
        s.push_str(&format!("# {}\n", meta.join("  ")));
    }

    // Without real boundaries the metadata above is the shell's record of the
    // last command, while the body is whatever the terminal last had on screen.
    // Usually the same thing; when the last command printed nothing, not.
    if cap.approximate {
        s.push_str(&format!(
            "# note: {} returns the last non-empty output, which may not be this \
             command's\n",
            cap.source
        ));
    }

    s
}

/// Render one already line-selected, already-truncated capture.
fn render_one(cap: &Capture, mode: Mode) -> String {
    match mode {
        Mode::Command => cap.command.clone().unwrap_or_default(),
        Mode::Raw | Mode::Output => cap.output.clone(),
        Mode::Annotated => {
            // `approximate` counts as something worth saying, even with no
            // command or exit code to show: kitty without shell integration has
            // no metadata at all, and that is precisely where an unlabelled body
            // is most likely to be the wrong one.
            if !cap.has_metadata() && !cap.approximate {
                return cap.output.clone();
            }
            let h = header(cap);
            if cap.output.is_empty() {
                h.trim_end().to_string()
            } else {
                format!("{h}\n{}", cap.output)
            }
        }
        Mode::Json => unreachable!("JSON is rendered for the whole set at once"),
    }
}

/// Prepare captures for output: clean or preserve ANSI, apply the line axis,
/// then truncate.
pub fn prepare(caps: &mut [Capture], mode: Mode, select: &Select, max_bytes: usize) {
    for cap in caps.iter_mut() {
        // `--raw` means verbatim, so it skips cleaning entirely.
        if mode != Mode::Raw {
            cap.output = ansi::clean(&cap.output);
        }
        cap.output = apply_line_selection(&cap.output, select);
        cap.output = truncate_middle(&cap.output, max_bytes);
    }
}

/// Render the full set. Multiple blocks are separated by a blank line and read
/// oldest-first, matching the order they were run.
pub fn render(caps: &[Capture], mode: Mode) -> String {
    if mode == Mode::Json {
        let value = if caps.len() == 1 {
            serde_json::to_value(&caps[0])
        } else {
            serde_json::to_value(caps)
        };
        return value
            .and_then(|v| serde_json::to_string_pretty(&v))
            .unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"));
    }

    caps.iter()
        .map(|c| render_one(c, mode))
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap_with_output(output: &str) -> Capture {
        Capture::new(output.to_string(), "test")
    }

    fn full_capture() -> Capture {
        Capture {
            command: Some("npm run build".into()),
            exit_code: Some(1),
            cwd: Some("/tmp/app".into()),
            duration_ms: Some(3200),
            output: "Error: Cannot find module 'foo'".into(),
            source: "tmux".into(),
            approximate: false,
        }
    }

    #[test]
    fn last_lines_keeps_the_tail() {
        let s = Select {
            last_lines: Some(2),
            ..Default::default()
        };
        assert_eq!(apply_line_selection("a\nb\nc\nd", &s), "c\nd");
    }

    #[test]
    fn last_lines_tolerates_asking_for_too_many() {
        let s = Select {
            last_lines: Some(99),
            ..Default::default()
        };
        assert_eq!(apply_line_selection("a\nb", &s), "a\nb");
    }

    #[test]
    fn line_index_counts_from_the_end() {
        let one = Select {
            line_index: Some(1),
            ..Default::default()
        };
        let two = Select {
            line_index: Some(2),
            ..Default::default()
        };
        assert_eq!(apply_line_selection("a\nb\nc", &one), "c");
        assert_eq!(apply_line_selection("a\nb\nc", &two), "b");
    }

    #[test]
    fn line_index_past_the_start_is_empty() {
        let s = Select {
            line_index: Some(9),
            ..Default::default()
        };
        assert_eq!(apply_line_selection("a\nb", &s), "");
    }

    #[test]
    fn no_line_flags_passes_text_through() {
        assert_eq!(apply_line_selection("a\nb", &Select::default()), "a\nb");
    }

    #[test]
    fn truncation_keeps_both_ends_and_marks_the_cut() {
        let text: String = (0..500)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = truncate_middle(&text, 400);
        assert!(out.contains("line 0"), "head should survive");
        assert!(out.contains("line 499"), "tail should survive");
        assert!(out.contains("lines omitted"), "cut must be visible");
        assert!(out.len() < text.len());
    }

    #[test]
    fn truncation_is_a_noop_when_it_fits_or_is_disabled() {
        assert_eq!(truncate_middle("short", 400), "short");
        let long = "x".repeat(10_000);
        assert_eq!(truncate_middle(&long, 0), long);
    }

    #[test]
    fn annotated_output_has_header_then_blank_line_then_body() {
        let out = render(&[full_capture()], Mode::Annotated);
        assert_eq!(
            out,
            "$ npm run build\n# exit: 1  cwd: /tmp/app  took: 3.2s\n\nError: Cannot find module 'foo'"
        );
    }

    /// Without shell integration there is no metadata, but an approximate body
    /// still has to say so.
    #[test]
    fn approximate_is_labelled_even_with_no_metadata() {
        let mut c = cap_with_output("mystery output");
        c.source = "kitty".into();
        c.approximate = true;
        let out = render(&[c], Mode::Annotated);
        assert!(out.contains("# note:"), "{out}");
        assert!(out.contains("mystery output"), "{out}");
    }

    #[test]
    fn annotated_falls_back_to_bare_output_without_metadata() {
        let out = render(&[cap_with_output("just text")], Mode::Annotated);
        assert_eq!(out, "just text");
    }

    /// Metadata over a body the backend cannot vouch for must say so, or the
    /// header reads as authoritative for text it may not describe.
    #[test]
    fn approximate_captures_are_labelled() {
        let mut c = full_capture();
        c.source = "kitty".into();
        c.approximate = true;
        let out = render(&[c], Mode::Annotated);
        assert!(
            out.contains("# note: kitty returns the last non-empty output"),
            "{out}"
        );

        // Exact backends must stay unqualified.
        let exact = render(&[full_capture()], Mode::Annotated);
        assert!(!exact.contains("# note:"), "{exact}");
    }

    #[test]
    fn approximate_is_only_serialised_when_true() {
        let mut c = full_capture();
        c.approximate = true;
        assert!(render(&[c], Mode::Json).contains("\"approximate\": true"));
        assert!(!render(&[full_capture()], Mode::Json).contains("approximate"));
    }

    #[test]
    fn exit_zero_is_still_reported() {
        let mut c = full_capture();
        c.exit_code = Some(0);
        assert!(render(&[c], Mode::Annotated).contains("exit: 0"));
    }

    #[test]
    fn output_and_command_modes_are_narrow() {
        let c = full_capture();
        assert_eq!(
            render(std::slice::from_ref(&c), Mode::Output),
            "Error: Cannot find module 'foo'"
        );
        assert_eq!(render(&[c], Mode::Command), "npm run build");
    }

    #[test]
    fn raw_mode_preserves_ansi_while_others_strip_it() {
        let mut raw = [cap_with_output("\x1b[31mred\x1b[0m")];
        prepare(&mut raw, Mode::Raw, &Select::default(), 0);
        assert!(raw[0].output.contains('\x1b'));

        let mut clean = [cap_with_output("\x1b[31mred\x1b[0m")];
        prepare(&mut clean, Mode::Output, &Select::default(), 0);
        assert_eq!(clean[0].output, "red");
    }

    #[test]
    fn json_is_an_object_for_one_block_and_an_array_for_many() {
        let one = render(&[full_capture()], Mode::Json);
        assert!(one.trim_start().starts_with('{'));
        assert!(one.contains("\"exit_code\": 1"));

        let many = render(&[full_capture(), full_capture()], Mode::Json);
        assert!(many.trim_start().starts_with('['));
    }

    #[test]
    fn json_omits_absent_metadata() {
        let out = render(&[cap_with_output("hi")], Mode::Json);
        assert!(!out.contains("exit_code"));
        assert!(out.contains("\"output\""));
    }

    #[test]
    fn multiple_blocks_are_separated_by_a_blank_line() {
        let out = render(&[full_capture(), full_capture()], Mode::Annotated);
        assert_eq!(out.matches("$ npm run build").count(), 2);
        assert!(out.contains("'foo'\n\n$ npm run build"));
    }

    #[test]
    fn durations_scale_by_magnitude() {
        assert_eq!(format_duration(250), "250ms");
        assert_eq!(format_duration(3200), "3.2s");
        assert_eq!(format_duration(65_000), "1m 5s");
    }
}
