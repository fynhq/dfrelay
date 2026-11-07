//! Pure display-width and unit-formatting helpers for the TUI.
//!
//! Kept free of `View`/`App` types so they can be unit-tested without a
//! `TestBackend`.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(super) fn wrap_help_text(text: &str, width: usize) -> Vec<String> {
    if text.trim().is_empty() {
        return vec![String::new()];
    }
    wrap_text(text, width, true)
}

/// Wrap display text (value columns, errors, hints) without carrying the
/// source indentation into continuation lines.
pub(super) fn wrap_display_text(text: &str, width: usize) -> Vec<String> {
    wrap_text(text, width, false)
}

/// Shared display-width wrapper. `keep_indent` makes continuation lines repeat
/// the leading spaces of the source line (Help content alignment).
fn wrap_text(text: &str, width: usize, keep_indent: bool) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for raw in text.split('\n') {
        if raw.is_empty() {
            lines.push(String::new());
            continue;
        }
        let indent: String = if keep_indent {
            raw.chars().take_while(|ch| *ch == ' ').collect()
        } else {
            String::new()
        };
        let indent_width = UnicodeWidthStr::width(indent.as_str());
        let mut current = String::new();
        let mut current_width = 0;
        for ch in raw.chars() {
            let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if !current.is_empty() && current_width + char_width > width {
                lines.push(current.trim_end().to_string());
                current = indent.clone();
                current_width = indent_width;
            }
            current.push(ch);
            current_width += char_width;
        }
        if !current.trim().is_empty() {
            lines.push(current.trim_end().to_string());
        }
    }
    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

pub(super) fn format_rate(bytes_per_second: f64) -> String {
    format_units(bytes_per_second, "/s")
}

pub(super) fn format_bytes(bytes: u64) -> String {
    format_units(bytes as f64, "")
}

/// Shared 1024-based unit ladder: whole bytes below 1 KiB, one decimal above.
fn format_units(value: f64, suffix: &str) -> String {
    if value < 1024.0 {
        return format!("{value:.0} B{suffix}");
    }
    const UNITS: [&str; 4] = ["K", "M", "G", "T"];
    let mut scaled = value / 1024.0;
    let mut index = 0;
    while scaled >= 1024.0 && index < UNITS.len() - 1 {
        scaled /= 1024.0;
        index += 1;
    }
    format!("{scaled:.1} {}{suffix}", UNITS[index])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_use_compact_binary_suffixes() {
        assert_eq!(format_rate(0.0), "0 B/s");
        assert_eq!(format_rate(1023.0), "1023 B/s");
        assert_eq!(format_rate(1024.0), "1.0 K/s");
        assert_eq!(format_rate(1024.0 * 1024.0), "1.0 M/s");
        assert_eq!(format_rate(1024.0f64.powi(3)), "1.0 G/s");
        assert_eq!(format_rate(1024.0f64.powi(4)), "1.0 T/s");
    }

    #[test]
    fn byte_counts_use_compact_binary_suffixes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.0 K");
        assert_eq!(format_bytes(1024u64.pow(2)), "1.0 M");
        assert_eq!(format_bytes(1024u64.pow(3)), "1.0 G");
        assert_eq!(format_bytes(1024u64.pow(4)), "1.0 T");
    }

    #[test]
    fn display_wrap_keeps_continuation_unindented() {
        let lines = wrap_display_text("abcdef", 3);
        assert_eq!(lines, vec!["abc", "def"]);
        let lines = wrap_display_text("a\nb", 3);
        assert_eq!(lines, vec!["a", "b"]);
    }

    #[test]
    fn help_wrap_repeats_the_leading_indent() {
        let lines = wrap_help_text("    abcdef", 6);
        assert_eq!(lines, vec!["    ab", "    cd", "    ef"]);
        assert_eq!(wrap_help_text("   ", 10), vec![String::new()]);
    }
}
