use std::io::{self, Cursor, Read};

use flate2::read::GzDecoder;

// SECURITY: cap the decompressed bytes and entry count for every entry, including
// entries that are skipped, so a tar.gz bomb cannot exhaust CPU or disk time.
const TAR_MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
const TAR_MAX_ENTRIES: u64 = 4096;

// SECURITY: reject oversized entries and unsafe archive paths; never read beyond the declared size.
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

struct LimitedReader<R> {
    inner: R,
    remaining: u64,
    limit: u64,
}

impl<R> LimitedReader<R> {
    fn new(inner: R, limit: u64) -> Self {
        Self {
            inner,
            remaining: limit,
            limit,
        }
    }
}

impl<R: Read> Read for LimitedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        // SECURITY: read one byte past the budget so a decompression bomb is detected
        // instead of silently truncating the stream.
        let allowance = self.remaining.saturating_add(1).min(buf.len() as u64) as usize;
        let read = self.inner.read(&mut buf[..allowance])?;
        if read as u64 > self.remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("archive exceeds the {} byte extraction limit", self.limit),
            ));
        }
        self.remaining -= read as u64;
        Ok(read)
    }
}

pub(crate) fn extract_tar_gz_entry(
    bytes: &[u8],
    target_name: &str,
    max: u64,
    too_large_message: &str,
) -> Result<Vec<u8>, String> {
    extract_tar_gz_entry_with_limits(
        bytes,
        target_name,
        max,
        too_large_message,
        TAR_MAX_TOTAL_BYTES,
        TAR_MAX_ENTRIES,
    )
}

fn extract_tar_gz_entry_with_limits(
    bytes: &[u8],
    target_name: &str,
    max: u64,
    too_large_message: &str,
    max_total_bytes: u64,
    max_entries: u64,
) -> Result<Vec<u8>, String> {
    let decoder = GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(LimitedReader::new(decoder, max_total_bytes));
    let mut found: Option<Vec<u8>> = None;
    let mut entry_count = 0u64;
    for entry in archive
        .entries()
        .map_err(|err| format!("archive error: {err}"))?
    {
        let entry = entry.map_err(|err| format!("archive error: {err}"))?;
        entry_count += 1;
        if entry_count > max_entries {
            return Err(format!("archive has more than {max_entries} entries"));
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tar_gz(entries: &[(&str, usize)]) -> Vec<u8> {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            for (name, size) in entries {
                let data = vec![0u8; *size];
                let mut header = tar::Header::new_gnu();
                header.set_size(*size as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(&mut header, *name, data.as_slice())
                    .unwrap();
            }
            builder.finish().unwrap();
        }
        let mut gz = Vec::new();
        {
            let mut encoder = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::fast());
            encoder.write_all(&tar_bytes).unwrap();
            encoder.finish().unwrap();
        }
        gz
    }

    #[test]
    fn extracts_the_target_within_limits() {
        let gz = tar_gz(&[("readme.txt", 512), ("sing-box", 16)]);
        let bytes =
            extract_tar_gz_entry_with_limits(&gz, "sing-box", 4096, "too large", 1 << 20, 16)
                .unwrap();
        assert_eq!(bytes.len(), 16);
    }

    #[test]
    fn rejects_a_large_skipped_entry_before_the_target() {
        let gz = tar_gz(&[("filler.bin", 128 * 1024), ("sing-box", 16)]);
        let err =
            extract_tar_gz_entry_with_limits(&gz, "sing-box", 4096, "too large", 32 * 1024, 16)
                .unwrap_err();
        assert!(err.contains("extraction limit"), "{err}");
    }

    #[test]
    fn rejects_a_large_skipped_entry_after_the_target() {
        let gz = tar_gz(&[("sing-box", 16), ("filler.bin", 128 * 1024)]);
        let err =
            extract_tar_gz_entry_with_limits(&gz, "sing-box", 4096, "too large", 32 * 1024, 16)
                .unwrap_err();
        assert!(err.contains("extraction limit"), "{err}");
    }

    #[test]
    fn rejects_archives_with_too_many_entries() {
        let gz = tar_gz(&[("a", 1), ("b", 1), ("sing-box", 1)]);
        let err = extract_tar_gz_entry_with_limits(&gz, "sing-box", 4096, "too large", 1 << 20, 2)
            .unwrap_err();
        assert!(err.contains("entries"), "{err}");
    }
}
