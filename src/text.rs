/// Remove terminal-control characters from text that originates outside the
/// program (subscription node names, remote host names, parser errors) before
/// it is written to a terminal buffer or a log file.
///
/// Newlines and tabs are folded to spaces so a single value cannot create a
/// forged log line. Printable Unicode, including wide CJK characters, is kept
/// unchanged.
pub fn sanitize_display(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\n' | '\r' | '\t' => out.push(' '),
            ch if ch.is_control() => {}
            ch => out.push(ch),
        }
    }
    out
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
}
