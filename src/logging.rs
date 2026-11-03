use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{Duration, Local, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::fsutil;

const MAX_BUFFER_LINES: usize = 2000;
const RETENTION_DAYS: i64 = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warn => "WARN",
            Self::Info => "INFO",
            Self::Debug => "DEBUG",
            Self::Trace => "TRACE",
        }
    }

    pub fn next_filter(self) -> Self {
        match self {
            Self::Error => Self::Warn,
            Self::Warn => Self::Info,
            Self::Info => Self::Debug,
            Self::Debug => Self::Trace,
            Self::Trace => Self::Error,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Component {
    App,
    Singbox,
}

impl Component {
    pub fn label(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Singbox => "sing-box",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogLine {
    pub timestamp: String,
    pub level: LogLevel,
    pub component: Component,
    pub message: String,
}

impl LogLine {
    pub fn display(&self, show_date: bool) -> String {
        // The date is optional; the time of day must always stay visible.
        let timestamp = if show_date {
            self.timestamp.as_str()
        } else {
            self.timestamp
                .split_once(' ')
                .map(|(_, time)| time)
                .unwrap_or(self.timestamp.as_str())
        };
        format!(
            "{} [{}] [{}] {}",
            timestamp,
            self.level.label(),
            self.component.label(),
            self.message
        )
    }
}

pub fn parse_singbox_line(line: &str) -> (Option<LogLevel>, String) {
    let line = strip_ansi(line);
    let rest = strip_timestamp(&line).unwrap_or(line.as_str());
    let (level, rest) = strip_level(rest);
    (level, rest.trim_start().to_string())
}

fn strip_ansi(line: &str) -> String {
    if !line.contains('\x1b') {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for next in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(next) = chars.next() {
                    if next == '\x07' {
                        break;
                    }
                    if next == '\x1b' && chars.clone().next() == Some('\\') {
                        chars.next();
                        break;
                    }
                }
            }
            Some('(' | ')' | '#' | '%') => {
                chars.next();
            }
            Some(_) | None => {}
        }
    }
    out
}

fn strip_timestamp(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let mut tokens = trimmed.split_whitespace();
    let first = tokens.next()?;
    let second = tokens.next();
    let third = tokens.next();

    if is_offset(first) {
        let date = second?;
        let time = third?;
        if is_date(date) && is_time(time) {
            return Some(skip_tokens(trimmed, 3));
        }
    }
    if is_date(first) {
        let time = second?;
        if is_time(time) {
            return Some(skip_tokens(trimmed, 2));
        }
    }
    if chrono::DateTime::parse_from_rfc3339(first).is_ok() {
        return Some(skip_tokens(trimmed, 1));
    }
    None
}

fn skip_tokens(line: &str, count: usize) -> &str {
    let mut rest = line;
    for _ in 0..count {
        rest = rest.trim_start();
        let Some(index) = rest.find(char::is_whitespace) else {
            return "";
        };
        rest = &rest[index..];
    }
    rest
}

fn is_offset(value: &str) -> bool {
    value.len() == 5
        && matches!(value.as_bytes()[0], b'+' | b'-')
        && value[1..].bytes().all(|byte| byte.is_ascii_digit())
}

fn is_date(value: &str) -> bool {
    value.len() == 10
        && value.as_bytes()[4] == b'-'
        && value.as_bytes()[7] == b'-'
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
}

fn is_time(value: &str) -> bool {
    value.len() >= 8
        && value.as_bytes()[2] == b':'
        && value.as_bytes()[5] == b':'
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| index == 2 || index == 5 || byte == b'.' || byte.is_ascii_digit())
}

fn strip_level(line: &str) -> (Option<LogLevel>, &str) {
    let trimmed = line.trim_start();
    let Some(index) = trimmed.find(char::is_whitespace) else {
        return match level_token(trimmed) {
            Some(level) => (Some(level), ""),
            None => (None, trimmed),
        };
    };
    let (token, rest) = trimmed.split_at(index);
    match level_token(token) {
        Some(level) => (Some(level), rest),
        None => (None, trimmed),
    }
}

fn level_token(token: &str) -> Option<LogLevel> {
    let token = token.trim_start_matches('[');
    let token = token.split(']').next().unwrap_or(token);
    let token = token.split('[').next().unwrap_or(token);
    let token = token.trim_end_matches(':');
    match token.to_ascii_uppercase().as_str() {
        "TRACE" => Some(LogLevel::Trace),
        "DEBUG" => Some(LogLevel::Debug),
        "INFO" => Some(LogLevel::Info),
        "WARN" | "WARNING" => Some(LogLevel::Warn),
        "ERROR" | "FATAL" | "PANIC" => Some(LogLevel::Error),
        _ => None,
    }
}

struct Inner {
    dir: PathBuf,
    date: String,
    file: Option<File>,
    lines: VecDeque<LogLine>,
    app_level: LogLevel,
    singbox_level: LogLevel,
}

#[derive(Clone)]
pub struct Logger {
    inner: Arc<Mutex<Inner>>,
}

impl Logger {
    pub fn new(dir: &Path, app_level: LogLevel, singbox_level: LogLevel) -> std::io::Result<Self> {
        fsutil::ensure_private_dir(dir)?;
        let inner = Inner {
            dir: dir.to_path_buf(),
            date: today(),
            file: None,
            lines: VecDeque::with_capacity(MAX_BUFFER_LINES),
            app_level,
            singbox_level,
        };
        let logger = Self {
            inner: Arc::new(Mutex::new(inner)),
        };
        logger.cleanup_old();
        Ok(logger)
    }

    pub fn set_levels(&self, app_level: LogLevel, singbox_level: LogLevel) {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.app_level = app_level;
        inner.singbox_level = singbox_level;
    }

    pub fn log(&self, component: Component, level: LogLevel, message: impl Into<String>) {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let min_level = match component {
            Component::App => inner.app_level,
            Component::Singbox => inner.singbox_level,
        };
        if level > min_level {
            return;
        }

        let now = Local::now();
        let date = now.format("%Y-%m-%d").to_string();
        if date != inner.date || inner.file.is_none() {
            inner.date = date;
            inner.file = open_log_file(&inner.dir, &inner.date).ok();
            drop(inner);
            self.cleanup_old();
            inner = match self.inner.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
        }

        let message: String = message.into();
        let line = LogLine {
            timestamp: now.format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
            level,
            component,
            message: crate::text::sanitize_display(&message),
        };

        if let Some(file) = inner.file.as_mut() {
            let _ = writeln!(
                file,
                "{} [{}] [{}] {}",
                line.timestamp,
                line.level.label(),
                line.component.label(),
                line.message
            );
            let _ = file.flush();
        }
        inner.lines.push_back(line);
        while inner.lines.len() > MAX_BUFFER_LINES {
            inner.lines.pop_front();
        }
    }

    pub fn app(&self, level: LogLevel, message: impl Into<String>) {
        self.log(Component::App, level, message);
    }

    pub fn singbox(&self, level: LogLevel, message: impl Into<String>) {
        self.log(Component::Singbox, level, message);
    }

    pub fn recent(&self, limit: usize) -> Vec<LogLine> {
        let inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let skip = inner.lines.len().saturating_sub(limit);
        inner.lines.iter().skip(skip).cloned().collect()
    }

    pub fn clear_view(&self) {
        let mut inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.lines.clear();
    }

    pub fn current_file(&self) -> PathBuf {
        let inner = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        inner.dir.join(format!("dfrelay-{}.log", inner.date))
    }

    pub fn cleanup_old(&self) {
        let dir = {
            let inner = match self.inner.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            inner.dir.clone()
        };
        let cutoff = Local::now().date_naive() - Duration::days(RETENTION_DAYS);
        let Ok(entries) = fs::read_dir(&dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(date_part) = name
                .strip_prefix("dfrelay-")
                .and_then(|s| s.strip_suffix(".log"))
            else {
                continue;
            };
            let Ok(date) = NaiveDate::parse_from_str(date_part, "%Y-%m-%d") else {
                continue;
            };
            if date < cutoff {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

fn today() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

fn open_log_file(dir: &Path, date: &str) -> std::io::Result<File> {
    let path = dir.join(format!("dfrelay-{date}.log"));
    fsutil::open_private_file(&path, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn display_always_shows_time_and_toggles_only_the_date() {
        let line = LogLine {
            timestamp: "2026-10-31 12:34:56.789".to_string(),
            level: LogLevel::Info,
            component: Component::App,
            message: "message".to_string(),
        };
        let without_date = line.display(false);
        assert_eq!(without_date, "12:34:56.789 [INFO] [app] message");
        let with_date = line.display(true);
        assert_eq!(with_date, "2026-10-31 12:34:56.789 [INFO] [app] message");
    }

    #[test]
    fn writes_and_limits_buffer() {
        let dir = TempDir::new().unwrap();
        let logger = Logger::new(dir.path(), LogLevel::Info, LogLevel::Info).unwrap();
        for i in 0..(MAX_BUFFER_LINES + 10) {
            logger.app(LogLevel::Info, format!("line {i}"));
        }
        let recent = logger.recent(MAX_BUFFER_LINES + 100);
        assert_eq!(recent.len(), MAX_BUFFER_LINES);
        assert!(recent.last().unwrap().message.ends_with("2009"));
        assert!(logger.current_file().exists());
    }

    #[test]
    fn messages_are_sanitized_before_buffering_and_writing() {
        let dir = TempDir::new().unwrap();
        let logger = Logger::new(dir.path(), LogLevel::Info, LogLevel::Info).unwrap();
        logger.app(LogLevel::Info, "node \x1b]0;pwned\x07\nnext");
        let recent = logger.recent(1);
        assert_eq!(recent[0].message, "node ]0;pwned next");
        let file = fs::read_to_string(logger.current_file()).unwrap();
        assert!(!file.contains('\x1b'));
        assert!(!file.contains('\x07'));
        assert_eq!(file.lines().count(), 1);
    }

    #[test]
    fn filters_by_component_level() {
        let dir = TempDir::new().unwrap();
        let logger = Logger::new(dir.path(), LogLevel::Info, LogLevel::Error).unwrap();
        logger.app(LogLevel::Debug, "hidden");
        logger.app(LogLevel::Info, "shown");
        logger.singbox(LogLevel::Info, "hidden");
        logger.singbox(LogLevel::Error, "shown error");
        let lines = logger.recent(10);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].message, "shown");
        assert_eq!(lines[1].message, "shown error");
    }

    #[test]
    fn cleanup_removes_old_logs() {
        let dir = TempDir::new().unwrap();
        let old = Local::now().date_naive() - Duration::days(8);
        let old_path = dir.path().join(format!("dfrelay-{old}.log"));
        fs::write(&old_path, "old").unwrap();
        let logger = Logger::new(dir.path(), LogLevel::Info, LogLevel::Info).unwrap();
        logger.cleanup_old();
        assert!(!old_path.exists());
    }

    #[test]
    fn strips_timestamp_and_level_from_singbox_lines() {
        let cases = [
            (
                "+0800 2026-10-30 11:20:00 INFO inbound/mixed[0]: started",
                Some(LogLevel::Info),
                "inbound/mixed[0]: started",
            ),
            (
                "+0800 2026-10-30 11:20:00.123 WARN router: fallback",
                Some(LogLevel::Warn),
                "router: fallback",
            ),
            (
                "2026-10-30T11:20:00+08:00 ERROR outbound: dial failed",
                Some(LogLevel::Error),
                "outbound: dial failed",
            ),
            (
                "2026-10-30 11:20:00 DEBUG service: tick",
                Some(LogLevel::Debug),
                "service: tick",
            ),
            (
                "\u{1b}[36m+0800 2026-10-30 11:20:00 INFO inbound/mixed[0]: started\u{1b}[0m",
                Some(LogLevel::Info),
                "inbound/mixed[0]: started",
            ),
            (
                "INFO[0000] outbound/vless[n-1]: connected",
                Some(LogLevel::Info),
                "outbound/vless[n-1]: connected",
            ),
            (
                "[WARN] router: fallback",
                Some(LogLevel::Warn),
                "router: fallback",
            ),
            ("INFO: started", Some(LogLevel::Info), "started"),
            ("plain message", None, "plain message"),
        ];
        for (line, level, message) in cases {
            let parsed = parse_singbox_line(line);
            assert_eq!(parsed.0, level, "{line}");
            assert_eq!(parsed.1, message, "{line}");
        }
    }
}
