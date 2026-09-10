//! Escape-sequence stripping and screen-text cleanup. Everything except
//! `--raw` goes through [`clean`].

/// Remove ANSI/VT escape sequences, leaving the printable text.
///
/// Scans bytes rather than chars: escape sequences are all ASCII, and UTF-8
/// continuation bytes are >= 0x80, so they can never be mistaken for one.
pub fn strip(input: &str) -> String {
    let b = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;

    while i < b.len() {
        if b[i] != 0x1b {
            out.push(b[i]);
            i += 1;
            continue;
        }

        // Lone ESC at end of input; drop it.
        if i + 1 >= b.len() {
            break;
        }
        i += 1;

        match b[i] {
            // CSI: ESC [ <params> <final byte in 0x40..=0x7e>
            b'[' => {
                i += 1;
                while i < b.len() && !(0x40..=0x7e).contains(&b[i]) {
                    i += 1;
                }
                i += 1; // consume the final byte
            }
            // String-terminated sequences: OSC, DCS, APC, PM, SOS.
            // Run until BEL or ST (ESC \).
            b']' | b'P' | b'_' | b'^' | b'X' => {
                i += 1;
                while i < b.len() {
                    if b[i] == 0x07 {
                        i += 1;
                        break;
                    }
                    if b[i] == 0x1b && i + 1 < b.len() && b[i + 1] == b'\\' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            }
            // Two-byte escape (charset selection, ESC =, ESC >, ...).
            _ => i += 1,
        }
    }

    String::from_utf8_lossy(&out).into_owned()
}

/// A progress bar writes `10%\r50%\r100%` but the terminal shows only `100%`.
/// The trailing CR of a CRLF ending is dropped first, so it can't blank a line.
fn resolve_cr(line: &str) -> &str {
    let line = line.strip_suffix('\r').unwrap_or(line);
    match line.rfind('\r') {
        Some(i) => &line[i + 1..],
        None => line,
    }
}

/// Strip escapes, resolve carriage returns, trim trailing padding, and drop
/// blank edges. `capture-pane` right-pads every row to the pane width, hence
/// the trailing-space trim.
pub fn clean(input: &str) -> String {
    let stripped = strip(input);

    let mut lines: Vec<&str> = stripped
        .split('\n')
        .map(|l| resolve_cr(l).trim_end())
        .collect();

    while lines.first().is_some_and(|l| l.is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_sgr_colour() {
        assert_eq!(strip("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(strip("\x1b[1;38;5;204mbold\x1b[m!"), "bold!");
    }

    #[test]
    fn strips_osc_with_both_terminators() {
        // BEL-terminated
        assert_eq!(strip("\x1b]0;title\x07text"), "text");
        // ST-terminated
        assert_eq!(strip("\x1b]133;D;1\x1b\\text"), "text");
        // OSC 8 hyperlink wrapping a label
        assert_eq!(strip("\x1b]8;;http://x\x1b\\label\x1b]8;;\x1b\\"), "label");
    }

    #[test]
    fn preserves_multibyte_utf8() {
        assert_eq!(strip("\x1b[32m✓ 日本語\x1b[0m"), "✓ 日本語");
    }

    #[test]
    fn tolerates_truncated_escape() {
        assert_eq!(strip("ok\x1b"), "ok");
        assert_eq!(strip("ok\x1b["), "ok");
    }

    #[test]
    fn resolves_carriage_returns() {
        assert_eq!(clean("10%\r50%\r100% done"), "100% done");
        assert_eq!(clean("plain\r\nsecond"), "plain\nsecond");
    }

    #[test]
    fn trims_pane_padding_and_blank_edges() {
        assert_eq!(
            clean("\n\n  hello     \n\nworld   \n\n\n"),
            "  hello\n\nworld"
        );
    }
}
