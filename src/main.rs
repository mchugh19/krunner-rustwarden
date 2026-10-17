//! krunner-rustwarden: a KRunner D-Bus runner for Bitwarden, backed by rbw.
//!
//! This is the Rust implementation of the ccarpo/krunner-bitwarden runner
//! (Python, fork branch work/rbw-backend). It exposes the same
//! org.kde.krunner1 D-Bus interface under the same bus name, reads the
//! same config file, and implements the same behaviour:
//!
//!   * search the vault as you type (multi-token narrowing)
//!   * copy password (default), copy username, copy TOTP, open URL
//!   * lock / unlock / sync / stop-agent matches
//!   * password generation from configurable presets (printed, not saved)
//!
//! The differences from the Python version are the point of this port:
//! no interpreter and no python-dbus / PyGObject runtime dependencies
//! (D-Bus is zbus, pure Rust), and clipboard access goes through Klipper
//! first instead of straight to wl-copy / xclip.

mod clipboard;
mod config;
mod rbw;
mod selection;

use std::collections::HashMap;
use std::sync::Mutex;

use zbus::interface;
use zbus::zvariant::Value;

use config::{Config, Preset, PresetKind};
use rbw::{Rbw, VaultState};

const BUS_NAME: &str = "org.kde.krunner.bitwarden";
const OBJ_PATH: &str = "/runner";
const ICON: &str = "password-vault";

// categoryRelevance values, mirroring KRunner::QueryMatch::CategoryRelevance
const CATEGORY_LOWEST: i32 = 0;
const CATEGORY_MODERATE: i32 = 50;
const CATEGORY_HIGH: i32 = 70;
const CATEGORY_HIGHEST: i32 = 100;

const ACTION_COPY_USERNAME: &str = "copy_username";
const ACTION_COPY_TOTP: &str = "copy_totp";
const ACTION_OPEN_URL: &str = "open_url";

type MatchResult = (String, String, String, i32, f64, HashMap<String, Value<'static>>);
type ActionResult = (String, String, String);

#[derive(Clone, Default)]
struct CachedItem {
    username: String,
    uris: Vec<String>,
}

pub struct Runner {
    cfg: Config,
    rbw: Rbw,
    /// Item UUID -> display data from the last Match. Holds no secrets:
    /// rbw list returns no passwords, so Run fetches secrets at action time.
    cache: Mutex<HashMap<String, CachedItem>>,
}

fn props(subtext: &str) -> HashMap<String, Value<'static>> {
    let mut m = HashMap::new();
    m.insert("subtext".to_string(), Value::from(subtext.to_string()));
    m.insert("category".to_string(), Value::from("Bitwarden".to_string()));
    m
}

fn make_match(
    id: &str,
    text: &str,
    category_relevance: i32,
    relevance: f64,
    subtext: &str,
) -> MatchResult {
    (
        id.to_string(),
        text.to_string(),
        ICON.to_string(),
        category_relevance,
        relevance,
        props(subtext),
    )
}

impl Runner {
    fn new(cfg: Config) -> Self {
        let rbw = Rbw::new(&cfg);
        Runner {
            cfg,
            rbw,
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn copy(&self, text: &str, secret: bool) {
        clipboard::copy(text, self.cfg.clipboard_clear_seconds, secret);
    }

    fn search_credentials(&self, tokens: &[String]) -> Vec<MatchResult> {
        let mut matches = Vec::new();
        if tokens.is_empty() {
            return matches;
        }
        let items = self.rbw.list_items();
        let lowered: Vec<String> = tokens.iter().map(|t| t.to_lowercase()).collect();
        let mut cache = self.cache.lock().unwrap();
        for (i, item) in items.iter().enumerate() {
            let haystack = format!(
                "{} {} {}",
                item.name,
                item.username,
                item.uris.join(" ")
            )
            .to_lowercase();
            if !lowered.iter().all(|t| haystack.contains(t.as_str())) {
                continue;
            }
            // Unique relevance per position so KRunner shows all entries.
            let relevance = (0.95f64 - i as f64 * 0.02).max(0.5);
            cache.insert(
                item.id.clone(),
                CachedItem {
                    username: item.username.clone(),
                    uris: item.uris.clone(),
                },
            );
            let subtext = if item.username.is_empty() {
                "no username"
            } else {
                &item.username
            };
            matches.push(make_match(
                &item.id,
                &item.name,
                CATEGORY_HIGHEST,
                relevance,
                subtext,
            ));
        }
        matches
    }

    fn preset_summary(preset: &Preset) -> String {
        match preset.kind {
            // rbw diceware: separator/capitalize/include_number unsupported
            PresetKind::Passphrase => format!("{} words, diceware", preset.words),
            PresetKind::Password => {
                let mut parts = vec![format!("{} chars", preset.length)];
                if preset.uppercase {
                    parts.push("upper".into());
                }
                if preset.lowercase {
                    parts.push("lower".into());
                }
                if preset.numbers {
                    parts.push("numbers".into());
                }
                if preset.special {
                    parts.push("special".into());
                }
                parts.join(" · ")
            }
        }
    }

    fn run_command(&self, cmd: &str) {
        if cmd == "status" {
            // informational only
        } else if cmd == "lock" {
            self.rbw.lock();
        } else if cmd == "unlock" {
            self.rbw.unlock();
        } else if cmd == "sync" {
            let rbw = Rbw::new(&self.cfg);
            std::thread::spawn(move || rbw.sync());
        } else if cmd == "stop_agent" {
            self.rbw.stop_agent();
        } else if let Some(json) = cmd.strip_prefix("generate:") {
            match serde_json::from_str::<serde_json::Value>(json) {
                Ok(v) => {
                    if let Some(preset) = self.preset_from_json(&v) {
                        if let Some(generated) = self.rbw.generate(&preset) {
                            self.copy(&generated, true);
                        }
                    }
                }
                Err(e) => eprintln!("[bitwarden] Generate preset parse error: {e}"),
            }
        }
    }

    /// The generate match ID embeds the preset as JSON (same scheme as the
    /// Python version). Rebuild a Preset from it.
    fn preset_from_json(&self, v: &serde_json::Value) -> Option<Preset> {
        let label = v.get("label")?.as_str()?.to_string();
        // Prefer the live preset from config; fall back to the JSON fields
        // so a stale match still generates something sane.
        if let Some(p) = self.cfg.presets.iter().find(|p| p.label == label) {
            return Some(p.clone());
        }
        let kind = match v.get("type").and_then(|t| t.as_str()) {
            Some("passphrase") => PresetKind::Passphrase,
            _ => PresetKind::Password,
        };
        let b = |key: &str, dflt: bool| v.get(key).and_then(|x| x.as_bool()).unwrap_or(dflt);
        let n = |key: &str, dflt: u64| v.get(key).and_then(|x| x.as_u64()).unwrap_or(dflt) as u32;
        Some(Preset {
            label,
            kind,
            length: n("length", 16),
            words: n("words", 3),
            uppercase: b("uppercase", true),
            lowercase: b("lowercase", true),
            numbers: b("numbers", true),
            special: b("special", false),
            separator: v
                .get("separator")
                .and_then(|s| s.as_str())
                .unwrap_or("-")
                .to_string(),
            capitalize: b("capitalize", false),
            include_number: b("include_number", false),
        })
    }

    fn preset_json(preset: &Preset) -> String {
        serde_json::json!({
            "label": preset.label,
            "type": match preset.kind { PresetKind::Passphrase => "passphrase", PresetKind::Password => "password" },
            "length": preset.length,
            "words": preset.words,
            "uppercase": preset.uppercase,
            "lowercase": preset.lowercase,
            "numbers": preset.numbers,
            "special": preset.special,
            "separator": preset.separator,
            "capitalize": preset.capitalize,
            "include_number": preset.include_number,
        })
        .to_string()
    }
}

#[interface(name = "org.kde.krunner1")]
impl Runner {
    #[zbus(name = "Actions")]
    fn actions(&self) -> Vec<ActionResult> {
        vec![
            (
                ACTION_COPY_USERNAME.to_string(),
                "Copy Username".to_string(),
                "edit-copy".to_string(),
            ),
            (
                ACTION_COPY_TOTP.to_string(),
                "Copy TOTP".to_string(),
                "accept_time_event".to_string(),
            ),
            (
                ACTION_OPEN_URL.to_string(),
                "Open URL".to_string(),
                "internet-web-browser".to_string(),
            ),
        ]
    }

    #[zbus(name = "Match")]
    fn match_query(&self, query: &str) -> Vec<MatchResult> {
        let query = query.trim();
        let mut matches = Vec::new();

        let state = self.rbw.vault_state();
        let available = state != VaultState::Missing;
        let locked = state != VaultState::Unlocked;
        let q = query.to_lowercase();

        // Status
        if q.contains("bitwarden") || q.contains("bw status") || query.is_empty() {
            let desc = if !available {
                "rbw not found (set rbw_executable_path in config.ini)"
            } else if locked {
                "Vault LOCKED"
            } else {
                "Vault UNLOCKED"
            };
            matches.push(make_match(
                "cmd:status",
                &format!("Bitwarden Status: {desc}"),
                CATEGORY_LOWEST,
                0.5,
                desc,
            ));
        }

        // Lock
        if available && !locked && (q.contains("lock") || q.contains("bitwarden")) {
            matches.push(make_match(
                "cmd:lock",
                "Lock Bitwarden",
                CATEGORY_MODERATE,
                0.6,
                "Lock the vault immediately (rbw lock)",
            ));
        }

        // Unlock. rbw prompts through pinentry; the plugin never sees the
        // master password.
        if available
            && locked
            && (q.contains("unlock") || q.contains("bitwarden") || q.contains("password"))
        {
            matches.push(make_match(
                "cmd:unlock",
                "Unlock Bitwarden",
                CATEGORY_HIGH,
                0.9,
                "Press Enter to unlock (rbw pinentry prompt)",
            ));
        }

        // Sync
        if available && !locked && (q.contains("sync") || q.contains("bitwarden")) {
            matches.push(make_match(
                "cmd:sync",
                "Sync Bitwarden Vault",
                CATEGORY_LOWEST,
                0.4,
                "Refresh the local vault copy (rbw sync)",
            ));
        }

        // Stop agent
        if available && !locked && (q.contains("stop") || q.contains("agent")) {
            matches.push(make_match(
                "cmd:stop_agent",
                "Stop rbw Agent",
                CATEGORY_LOWEST,
                0.4,
                "Stop rbw-agent, which also locks the vault",
            ));
        }

        // Generate password
        if q.contains("generate") || q.contains("password") || q.contains("passphrase") {
            for preset in &self.cfg.presets {
                let summary = Self::preset_summary(preset);
                matches.push(make_match(
                    &format!("cmd:generate:{}", Self::preset_json(preset)),
                    &format!("Generate Password ({})", preset.label),
                    CATEGORY_MODERATE,
                    0.5,
                    &summary,
                ));
            }
        }

        // Credential search. Gated on Unlocked so that a locked vault never
        // triggers a pinentry prompt from inside Match (per keystroke).
        if query.chars().count() >= 3 && available && !locked {
            let tokens: Vec<String> = query.split_whitespace().map(|s| s.to_string()).collect();
            matches.extend(self.search_credentials(&tokens));
        }

        matches
    }

    #[zbus(name = "Run")]
    fn run(&self, match_id: &str, action_id: &str) {
        if let Some(cmd) = match_id.strip_prefix("cmd:") {
            self.run_command(cmd);
            return;
        }
        // Credential result. match_id is the Bitwarden item UUID, which rbw
        // accepts directly as the entry name for get/code.
        let cached = self
            .cache
            .lock()
            .unwrap()
            .get(match_id)
            .cloned()
            .unwrap_or_default();
        if action_id == ACTION_COPY_USERNAME {
            let username = if !cached.username.is_empty() {
                cached.username
            } else {
                self.rbw.get_field(match_id, "username")
            };
            self.copy(&username, false);
        } else if action_id == ACTION_COPY_TOTP {
            let totp = self.rbw.totp(match_id);
            self.copy(&totp, true);
        } else if action_id == ACTION_OPEN_URL {
            let uris = if !cached.uris.is_empty() {
                cached.uris
            } else {
                self.rbw
                    .get_field(match_id, "uris")
                    .lines()
                    .map(|s| s.to_string())
                    .collect()
            };
            if let Some(url) = uris.first() {
                if !url.is_empty() {
                    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
                }
            }
        } else {
            // Default: copy password, fetched from rbw at action time.
            let password = self.rbw.password(match_id);
            self.copy(&password, true);
        }
    }
}

fn main() -> zbus::Result<()> {
    let cfg = config::load();
    let runner = Runner::new(cfg);
    let _conn = zbus::blocking::connection::Builder::session()?
        .name(BUS_NAME)?
        .replace_existing_names(true)
        .serve_at(OBJ_PATH, runner)?
        .build()?;
    eprintln!("[bitwarden] KRunner Bitwarden runner started (krunner-rustwarden, rbw backend)");
    // Park forever; systemd stops us with SIGTERM.
    loop {
        std::thread::park();
    }
}
