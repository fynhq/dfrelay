use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn ensure_private_dir(path: &Path) -> io::Result<()> {
    if path.exists() {
        fs::create_dir_all(path)?;
        return Ok(());
    }
    fs::create_dir_all(path)?;
    set_private_dir(path)
}

pub fn set_private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub fn open_private_file(path: &Path, append: bool) -> io::Result<File> {
    let file = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut options = OpenOptions::new();
            options.create(true).write(true).mode(0o600);
            if append {
                options.append(true);
            } else {
                options.truncate(true);
            }
            options.open(path)?
        }
        #[cfg(not(unix))]
        {
            let mut options = OpenOptions::new();
            options.create(true).write(true);
            if append {
                options.append(true);
            } else {
                options.truncate(true);
            }
            options.open(path)?
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

/// Tighten an existing file to 0600. Existing directories are intentionally
/// left unchanged; the product scope promises not to alter user directory
/// permissions.
pub fn enforce_private_file(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub fn set_executable(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub fn write_private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = open_private_file(path, false)?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()
}

pub fn write_private_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("data");
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(".{name}.{}.{counter}.tmp", std::process::id()));
    let result = (|| {
        write_private_file(&temp, bytes)?;
        fs::rename(&temp, path)?;
        // Persist the rename itself; otherwise a power loss can leave the
        // directory entry pointing at neither the old nor the new file.
        #[cfg(unix)]
        {
            if let Ok(dir) = File::open(parent) {
                let _ = dir.sync_all();
            }
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}
