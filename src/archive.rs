//! Shared archive-entry reading for the sing-box and dfr archives.
//!
//! Both the tar.gz and zip paths reject oversized entries and bound the bytes
//! actually read, so a malicious archive cannot inflate memory beyond the
//! declared entry size.

use std::io::{self, Cursor, Read};

use flate2::read::GzDecoder;

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

/// Extract exactly one named file from a gzip-compressed tar archive, rejecting
/// oversized entries, unsafe paths and duplicate matches.
pub(crate) fn extract_tar_gz_entry(
    bytes: &[u8],
    target_name: &str,
    max: u64,
    too_large_message: &str,
) -> Result<Vec<u8>, String> {
    let decoder = GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    let mut found: Option<Vec<u8>> = None;
    for entry in archive
        .entries()
        .map_err(|err| format!("archive error: {err}"))?
    {
        let entry = entry.map_err(|err| format!("archive error: {err}"))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .map_err(|err| format!("archive error: {err}"))?;
        if path.file_name().and_then(|name| name.to_str()) != Some(target_name) {
            continue;
        }
        if path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        }) {
            return Err("unsafe archive path".to_string());
        }
        let size = entry.size();
        if size > max {
            return Err(too_large_message.to_string());
        }
        if found.is_some() {
            return Err(format!("archive contains multiple {target_name} files"));
        }
        found = Some(read_limited(entry, size, max, too_large_message)?);
    }
    found.ok_or_else(|| format!("{target_name} not found in archive"))
}
