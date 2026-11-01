use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-env-changed=CI");
    if Path::new(".git").exists() {
        println!("cargo:rerun-if-changed=.git/HEAD");
        println!("cargo:rerun-if-changed=.git/refs");
        if Path::new(".git/packed-refs").exists() {
            println!("cargo:rerun-if-changed=.git/packed-refs");
        }
    }

    let version = std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION is set by Cargo");
    let suffix = if std::env::var_os("CI").is_some() {
        String::new()
    } else {
        git_short_hash()
            .map(|hash| format!("+{hash}"))
            .unwrap_or_default()
    };
    println!("cargo:rustc-env=DFRELAY_VERSION_DISPLAY={version}{suffix}");
}

fn git_short_hash() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if hash.is_empty() || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(hash)
}
