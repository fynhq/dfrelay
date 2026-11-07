//! Shared archive-entry reading for the sing-box and dfr archives.
//!
//! Both the tar.gz and zip paths reject oversized entries and bound the bytes
//! actually read, so a malicious archive cannot inflate memory beyond the
//! declared entry size.

use std::io::{self, Read};

/// Read at most `max + 1` bytes from `reader` and reject entries whose declared
/// or actual size exceeds `max`.
///
/// The extra byte makes a lying size header observable without reading the
/// whole entry.
pub(crate) fn read_limited<R: Read>(
    reader: R,
    declared_size: u64,
    max: u64,
    too_large_message: &str,
) -> Result<Vec<u8>, String> {
    if declared_size > max {
        return Err(too_large_message.to_string());
    }
    let mut bytes = Vec::new();
    reader
        .take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error_message)?;
    if bytes.len() as u64 > max {
        return Err(too_large_message.to_string());
    }
    Ok(bytes)
}

fn io_error_message(err: io::Error) -> String {
    err.to_string()
}
