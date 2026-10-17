//! Configuration for krunner-rustwarden.
//!
//! The config file lives at `~/.config/krunner-rustwarden/config.ini`.
//! Generator presets are expressed in rbw's own terms, so every key maps
//! to a real `rbw generate` flag and nothing is silently ignored.
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
    /// Password presets: number of characters.
    pub length: u32,
    /// Passphrase presets: number of diceware words.
    pub words: u32,
    /// Password presets: include symbols. False passes --no-symbols.
    pub symbols: bool,
    /// Password presets: digits only. Passes --only-numbers.
    pub numbers_only: bool,
    /// Password presets: exclude look-alike characters. Passes
    /// --nonconfusables.
    pub nonconfusables: bool,
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
    /// Offer an "Open in Web Vault" action on entry matches. The vault
    /// address is not configured here: it comes from rbw's own ui_url /
    /// base_url settings at action time.
    pub webvault_action: bool,
    /// Include secure note entries in search results. Running one opens
    /// the entry in the web vault; note text is never copied.
    pub show_secure_notes: bool,
    pub presets: Vec<Preset>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            rbw_exe: "rbw".to_string(),
            rbw_profile: String::new(),
            clipboard_clear_seconds: 30,
            webvault_action: false,
            show_secure_notes: false,
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
    base.join("krunner-rustwarden")
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

# Add an Open in Web Vault action to entry matches. It opens the item's
# page in the web vault, which is where an entry's notes can be read. The
# vault address comes from rbw's own ui_url / base_url settings
# (see `rbw config show`); there is no separate address to configure here.
webvault_action = false

# Include secure note entries in search results. Running a note entry
# opens it in the web vault; note text is never copied.
show_secure_notes = false

[generator.default]
type = password
length = 16
symbols = false

[generator.strong]
type = password
length = 24
symbols = true

[generator.passphrase]
type = passphrase
words = 3

[generator.pin]
type = password
length = 6
numbers_only = true
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

    let rbw_exe = bw
        .get("rbw_executable_path")
        .cloned()
        .unwrap_or_default();
    if !rbw_exe.is_empty() {
        cfg.rbw_exe = rbw_exe;
    }
    cfg.rbw_profile = bw.get("rbw_profile").cloned().unwrap_or_default();
    cfg.clipboard_clear_seconds = get_u64(bw, "clipboard_clear_seconds", 30);
    cfg.webvault_action = get_bool(bw, "webvault_action", false);
    cfg.show_secure_notes = get_bool(bw, "show_secure_notes", false);

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
            symbols: get_bool(p, "symbols", true),
            numbers_only: get_bool(p, "numbers_only", false),
            nonconfusables: get_bool(p, "nonconfusables", false),
        });
    }
    cfg
}
