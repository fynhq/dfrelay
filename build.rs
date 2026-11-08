use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
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

    generate_i18n();
}

const RESERVED_I18N_KEYS: &[&str] = &[
    "as",
    "break",
    "const",
    "continue",
    "crate",
    "else",
    "enum",
    "extern",
    "false",
    "fn",
    "for",
    "if",
    "impl",
    "in",
    "let",
    "loop",
    "match",
    "mod",
    "move",
    "mut",
    "pub",
    "ref",
    "return",
    "self",
    "static",
    "struct",
    "super",
    "trait",
    "true",
    "type",
    "unsafe",
    "use",
    "where",
    "while",
    "async",
    "await",
    "dyn",
    "abstract",
    "become",
    "box",
    "do",
    "final",
    "macro",
    "override",
    "priv",
    "typeof",
    "unsized",
    "virtual",
    "yield",
    "try",
    "help_footer",
    "help_sections",
];

fn generate_i18n() {
    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set"));
    let locales_dir = manifest_dir.join("locales");
    let zh_path = locales_dir.join("zh.conf");
    let en_path = locales_dir.join("en.conf");
    let source_path = manifest_dir.join("src/i18n.rs");
    println!("cargo:rerun-if-changed={}", locales_dir.display());
    println!("cargo:rerun-if-changed={}", zh_path.display());
    println!("cargo:rerun-if-changed={}", en_path.display());
    println!("cargo:rerun-if-changed={}", source_path.display());

    let zh = parse_locale(&zh_path);
    let en = parse_locale(&en_path);
    ensure_same_keys(&zh, &en);
    ensure_text_fields(&source_path, &zh);
    if let Some((key, _)) = en.iter().find(|(_, value)| has_cjk(value)) {
        panic!("i18n: en.conf key `{key}` contains Chinese text");
    }

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set"));
    let generated_path = out_dir.join("i18n_gen.rs");
    let generated = render_i18n(&zh, &en);
    fs::write(&generated_path, generated)
        .unwrap_or_else(|error| panic!("i18n: write {}: {error}", generated_path.display()));
}

fn parse_locale(path: &Path) -> BTreeMap<String, String> {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("i18n: read {}: {error}", path.display()));
    let mut entries = BTreeMap::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let line_number = index + 1;
        let Some((key, value)) = line.split_once('=') else {
            panic!("i18n: {}:{line_number}: expected key=value", path.display());
        };
        let key = key.trim();
        if !valid_key(key) {
            panic!(
                "i18n: {}:{line_number}: invalid key `{key}`",
                path.display()
            );
        }
        if RESERVED_I18N_KEYS.contains(&key) {
            panic!(
                "i18n: {}:{line_number}: reserved key `{key}`",
                path.display()
            );
        }
        if value.is_empty() {
            panic!(
                "i18n: {}:{line_number}: empty value for `{key}`",
                path.display()
            );
        }
        if entries.insert(key.to_owned(), value.to_owned()).is_some() {
            panic!(
                "i18n: {}:{line_number}: duplicate key `{key}`",
                path.display()
            );
        }
    }
    if entries.is_empty() {
        panic!("i18n: {}: no entries", path.display());
    }
    entries
}

fn valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn ensure_same_keys(zh: &BTreeMap<String, String>, en: &BTreeMap<String, String>) {
    let missing_en = zh
        .keys()
        .filter(|key| !en.contains_key(*key))
        .map(String::as_str)
        .collect::<Vec<_>>();
    let missing_zh = en
        .keys()
        .filter(|key| !zh.contains_key(*key))
        .map(String::as_str)
        .collect::<Vec<_>>();
    if !missing_en.is_empty() || !missing_zh.is_empty() {
        panic!(
            "i18n: locale key mismatch; en.conf missing [{}], zh.conf missing [{}]",
            missing_en.join(", "),
            missing_zh.join(", ")
        );
    }
}

fn ensure_text_fields(path: &Path, entries: &BTreeMap<String, String>) {
    let source = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("i18n: read {}: {error}", path.display()));
    let fields = parse_text_fields(&source)
        .unwrap_or_else(|error| panic!("i18n: {}: {error}", path.display()));
    let missing_from_locale = fields
        .iter()
        .filter(|field| !entries.contains_key(field.as_str()))
        .map(String::as_str)
        .collect::<Vec<_>>();
    let unknown_in_locale = entries
        .keys()
        .filter(|key| !fields.contains(key.as_str()))
        .map(String::as_str)
        .collect::<Vec<_>>();
    if !missing_from_locale.is_empty() || !unknown_in_locale.is_empty() {
        panic!(
            "i18n: locale keys and Text fields differ; locale missing [{}], locale unknown [{}]",
            missing_from_locale.join(", "),
            unknown_in_locale.join(", ")
        );
    }
}

fn parse_text_fields(source: &str) -> Result<BTreeSet<String>, String> {
    let start = source
        .find("pub struct Text {")
        .ok_or("Text struct not found")?;
    let rest = &source[start..];
    let end = rest.find("\n}").ok_or("Text struct end not found")?;
    let mut fields = BTreeSet::new();
    for line in rest[..end].lines() {
        let line = line.trim();
        let line = line
            .strip_prefix("pub(crate) ")
            .or_else(|| line.strip_prefix("pub "))
            .unwrap_or(line);
        let Some((name, ty)) = line.split_once(':') else {
            continue;
        };
        if ty.trim() != "&'static str," {
            continue;
        }
        let name = name.trim();
        if !valid_key(name) {
            return Err(format!("invalid Text field `{name}`"));
        }
        fields.insert(name.to_owned());
    }
    if fields.is_empty() {
        return Err("no flat Text fields found".to_owned());
    }
    Ok(fields)
}

fn has_cjk(value: &str) -> bool {
    value
        .chars()
        .any(|c| matches!(c, '\u{4e00}'..='\u{9fff}' | '\u{f900}'..='\u{faff}'))
}

fn render_i18n(zh: &BTreeMap<String, String>, en: &BTreeMap<String, String>) -> String {
    let mut generated = String::new();
    writeln!(
        generated,
        "// @generated by build.rs from locales/zh.conf and locales/en.conf."
    )
    .expect("write generated header");
    for (name, entries, footer, sections) in [
        ("ZH", zh, "ZH_HELP_FOOTER", "ZH_HELP_SECTIONS"),
        ("EN", en, "EN_HELP_FOOTER", "EN_HELP_SECTIONS"),
    ] {
        writeln!(generated, "\nstatic {name}: Text = Text {{").expect("write generated static");
        for (key, value) in entries {
            writeln!(generated, "    {key}: {value:?},").expect("write generated field");
        }
        writeln!(generated, "    help_footer: {footer},").expect("write help footer");
        writeln!(generated, "    help_sections: {sections},").expect("write help sections");
        writeln!(generated, "}};").expect("write generated static end");
    }
    generated
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
