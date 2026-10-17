//! Configuration for krunner-rbw.
//!
//! The config file is shared with the Python (bw-serve/rbw) version of this
//! runner: `~/.config/krunner-bitwarden/config.ini`. Installing either
//! implementation picks up the same rbw settings and generator presets.
//!
//! Format is INI. Parsing is deliberately minimal (sections, `key = value`,
//! `#` / `;` comments), which covers everything the config file uses and
//! keeps the dependency list at zero for this layer.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub enum PresetKind {
    Password,
    Passphrase,
}

#[derive(Debug, Clone)]
pub struct Preset {
    pub label: String,
    pub kind: PresetKind,
    pub length: u32,
    pub words: u32,
    pub uppercase: bool,
    pub lowercase: bool,
    pub numbers: bool,
    pub special: bool,
    // Passphrase-only fields. Parsed for config compatibility; rbw's
    // diceware generator has no equivalent for them.
    pub separator: String,
    pub capitalize: bool,
    pub include_number: bool,
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Path to the rbw executable, or "rbw" to resolve it from PATH.
    pub rbw_exe: String,
    /// rbw profile, passed to every rbw call as RBW_PROFILE. Empty = default.
    pub rbw_profile: String,
    /// Seconds after which a copied secret is cleared from the clipboard
    /// (only if the clipboard still holds that secret). 0 disables clearing.
    pub clipboard_clear_seconds: u64,
    pub presets: Vec<Preset>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            rbw_exe: "rbw".to_string(),
            rbw_profile: String::new(),
            clipboard_clear_seconds: 30,
            presets: Vec::new(),
        }
    }
}

pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    base.join("krunner-bitwarden")
}

pub fn config_file() -> PathBuf {
    config_dir().join("config.ini")
}

const DEFAULT_CONFIG: &str = "\
[bitwarden]
# Full path to the rbw executable. Leave empty to use rbw from PATH.
rbw_executable_path =

# rbw profile to use. Leave empty for rbw's default profile.
rbw_profile =

# Seconds after which a copied password/TOTP is cleared from the clipboard,
# if the clipboard still holds that exact text. 0 disables auto-clearing.
clipboard_clear_seconds = 30

[generator.default]
type = password
length = 16
uppercase = true
lowercase = true
numbers = true
special = false

[generator.strong]
type = password
length = 24
uppercase = true
lowercase = true
numbers = true
special = true

[generator.passphrase]
type = passphrase
words = 3
separator = -
capitalize = false
include_number = false

[generator.pin]
type = password
length = 6
uppercase = false
lowercase = false
numbers = true
special = false
";

/// A tiny INI reader: section name -> (key -> value), keys lowercased,
/// section order preserved separately for generator presets.
struct Ini {
    sections: HashMap<String, HashMap<String, String>>,
    order: Vec<String>,
}

fn parse_ini(text: &str) -> Ini {
    let mut ini = Ini {
        sections: HashMap::new(),
        order: Vec::new(),
    };
    let mut current = String::new();
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            current = line[1..line.len() - 1].trim().to_lowercase();
            if !ini.sections.contains_key(&current) {
                ini.order.push(current.clone());
                ini.sections.insert(current.clone(), HashMap::new());
            }
            continue;
        }
        if let Some(eq) = line.find('=') {
            let key = line[..eq].trim().to_lowercase();
            let value = line[eq + 1..].trim().to_string();
            ini.sections
                .entry(current.clone())
                .or_default()
                .insert(key, value);
        }
    }
    ini
}

fn get_bool(map: &HashMap<String, String>, key: &str, fallback: bool) -> bool {
    match map.get(key).map(|v| v.to_lowercase()) {
        Some(v) => matches!(v.as_str(), "true" | "yes" | "on" | "1"),
        None => fallback,
    }
}

fn get_u32(map: &HashMap<String, String>, key: &str, fallback: u32) -> u32 {
    map.get(key)
        .and_then(|v| v.parse().ok())
        .unwrap_or(fallback)
}

fn get_u64(map: &HashMap<String, String>, key: &str, fallback: u64) -> u64 {
    map.get(key)
        .and_then(|v| v.parse().ok())
        .unwrap_or(fallback)
}

pub fn load() -> Config {
    let path = config_file();
    if !path.exists() {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(&path, DEFAULT_CONFIG);
    }
    let text = fs::read_to_string(&path).unwrap_or_default();
    let ini = parse_ini(&text);

    let mut cfg = Config::default();
    let empty = HashMap::new();
    let bw = ini.sections.get("bitwarden").unwrap_or(&empty);

    // Legacy alias from the bw-serve version of the config: the literal
    // default "bw" in old files means "unset", not the rbw path.
    let mut legacy_exe = bw.get("bw_executable_path").cloned().unwrap_or_default();
    if legacy_exe == "bw" {
        legacy_exe.clear();
    }
    let rbw_exe = bw
        .get("rbw_executable_path")
        .cloned()
        .unwrap_or_default();
    if !rbw_exe.is_empty() {
        cfg.rbw_exe = rbw_exe;
    } else if !legacy_exe.is_empty() {
        cfg.rbw_exe = legacy_exe;
    }
    cfg.rbw_profile = bw.get("rbw_profile").cloned().unwrap_or_default();
    cfg.clipboard_clear_seconds = get_u64(bw, "clipboard_clear_seconds", 30);

    for section in &ini.order {
        let Some(label) = section.strip_prefix("generator.") else {
            continue;
        };
        let p = ini.sections.get(section).unwrap_or(&empty);
        let is_passphrase = p
            .get("type")
            .map(|t| t.to_lowercase() == "passphrase")
            .unwrap_or(false);
        cfg.presets.push(Preset {
            label: label.to_string(),
            kind: if is_passphrase {
                PresetKind::Passphrase
            } else {
                PresetKind::Password
            },
            length: get_u32(p, "length", 16),
            words: get_u32(p, "words", 3),
            uppercase: get_bool(p, "uppercase", true),
            lowercase: get_bool(p, "lowercase", true),
            numbers: get_bool(p, "numbers", true),
            special: get_bool(p, "special", false),
            separator: p.get("separator").cloned().unwrap_or_else(|| "-".into()),
            capitalize: get_bool(p, "capitalize", false),
            include_number: get_bool(p, "include_number", false),
        });
    }
    cfg
}
