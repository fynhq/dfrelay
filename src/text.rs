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

/// Sanitize an already-owned string without copying it when it is clean.
/// Most log lines and node names need no rewriting, so this keeps the hot path
/// allocation-free instead of copying every value.
pub fn sanitize_owned(input: String) -> String {
    if input
        .chars()
        .any(|ch| matches!(ch, '\n' | '\r' | '\t') || ch.is_control() || is_unsafe_format_char(ch))
    {
        sanitize_display(&input)
    } else {
        input
    }
}

/// Zero-width and bidirectional formatting characters (`Unicode` category Cf)
/// that have no legitimate use in a node or host name and can visually spoof
/// or reorder surrounding text.
fn is_unsafe_format_char(ch: char) -> bool {
    matches!(
        ch,
        '\u{00AD}'              // soft hyphen
            | '\u{0600}'..='\u{0605}' // Arabic number signs
            | '\u{061C}'         // Arabic letter mark
            | '\u{06DD}'         // Arabic end of ayah
            | '\u{070F}'         // Syriac abbreviation mark
            | '\u{0890}'..='\u{0891}' // Arabic pound/piastre marks
            | '\u{08E2}'         // Arabic disputed end of ayah
            | '\u{17B4}'..='\u{17B5}' // Khmer inherent vowels
            | '\u{180E}'         // Mongolian vowel separator
            | '\u{200B}'..='\u{200F}' // zero-width space/joiners, LRM/RLM
            | '\u{202A}'..='\u{202E}' // bidi embedding/override
            | '\u{2028}' | '\u{2029}' // line/paragraph separators
            | '\u{2060}'..='\u{2064}' // word joiner, invisible operators
            | '\u{2066}'..='\u{206F}' // bidi isolates and deprecated format chars
            | '\u{110BD}' | '\u{110CD}' // Kaithi number signs
            | '\u{13430}'..='\u{1343F}' // Egyptian hieroglyph format controls
            | '\u{1BCA0}'..='\u{1BCA3}' // shorthand format controls
            | '\u{1D173}'..='\u{1D17A}' // musical symbol format controls
            | '\u{E0001}'         // language tag
            | '\u{E0020}'..='\u{E007F}' // tag characters
            | '\u{FEFF}'          // zero-width no-break space (BOM)
            | '\u{FFF9}'..='\u{FFFB}' // interlinear annotation controls
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
        let input = "ab\u{202E}cd\u{200B}ef\u{2066}gh\u{FEFF}\u{00AD}\u{E0001}\u{E0041}\u{FFF9}";
        assert_eq!(sanitize_display(input), "abcdefgh");
    }

    #[test]
    fn sanitize_owned_reuses_clean_strings_and_cleans_dirty_ones() {
        let clean = String::from("香港 01");
        let pointer = clean.as_ptr();
        let sanitized = sanitize_owned(clean);
        assert_eq!(sanitized, "香港 01");
        assert_eq!(sanitized.as_ptr(), pointer);

        assert_eq!(sanitize_owned("a\nb".to_string()), "a b");
    }
}
