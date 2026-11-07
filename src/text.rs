/// Remove terminal-control characters from text that originates outside the
/// program (subscription node names, remote host names, parser errors) before
/// it is written to a terminal buffer or a log file.
///
/// Newlines and tabs are folded to spaces so a single value cannot create a
/// forged log line. Printable Unicode, including wide CJK characters, is kept
/// unchanged. Bidi overrides and zero-width format characters are dropped so
/// an untrusted name cannot reorder or hide text (trojan-source style).
pub fn sanitize_display(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\n' | '\r' | '\t' => out.push(' '),
            ch if ch.is_control() => {}
            ch if is_unsafe_format_char(ch) => {}
            ch => out.push(ch),
        }
    }
    out
}

/// Zero-width and bidirectional formatting characters (`Unicode` category Cf)
/// that have no legitimate use in a node or host name and can visually spoof
/// or reorder surrounding text.
fn is_unsafe_format_char(ch: char) -> bool {
    matches!(
        ch,
        '\u{061C}'              // Arabic letter mark
            | '\u{200B}'..='\u{200F}' // zero-width space/joiners, LRM/RLM
            | '\u{202A}'..='\u{202E}' // bidi embedding/override
            | '\u{2060}'..='\u{2064}' // word joiner, invisible operators
            | '\u{2066}'..='\u{206F}' // bidi isolates and deprecated format chars
            | '\u{FEFF}'             // zero-width no-break space (BOM)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_escape_and_control_characters() {
        let input = "香港\x1b]52;c;ZGF0YQ==\x07\x1b[31m-red\x1b[0m\nnext";
        let output = sanitize_display(input);
        assert!(!output.contains('\x1b'));
        assert!(!output.contains('\x07'));
        assert!(!output.contains('\n'));
        assert!(output.contains("香港"));
        assert!(output.contains("next"));
    }

    #[test]
    fn keeps_printable_unicode_and_spaces() {
        assert_eq!(sanitize_display("香港 01"), "香港 01");
        assert_eq!(sanitize_display("line\r\nnext"), "line  next");
    }

    #[test]
    fn removes_bidi_and_zero_width_format_characters() {
        let input = "ab\u{202E}cd\u{200B}ef\u{2066}gh\u{FEFF}";
        assert_eq!(sanitize_display(input), "abcdefgh");
    }
}
