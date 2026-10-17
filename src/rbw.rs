//! rbw subprocess layer.
//!
//! Every vault operation is a short-lived `rbw` call, mirroring the Python
//! rbw port of this runner. rbw has no HTTP API; its agent (rbw-agent)
//! holds the vault keys in memory and prompts via pinentry on unlock.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value as Json;

use crate::config::Config;

pub struct Rbw {
    exe: String,
    profile: String,
    not_found: AtomicBool,
}

pub struct Output {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub username: String,
    pub uris: Vec<String>,
}

/// rbw entry types that are not logins. A missing or unknown type is kept,
/// so a spelling difference in rbw's type names cannot hide credentials.
const NON_LOGIN_TYPES: &[&str] = &[
    "card",
    "creditcard",
    "identity",
    "note",
    "securenote",
    "secure note",
    "sshkey",
    "ssh key",
    "ssh",
];

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VaultState {
    Unlocked,
    Locked,
    Missing,
}

impl Rbw {
    pub fn new(cfg: &Config) -> Self {
        Rbw {
            exe: cfg.rbw_exe.clone(),
            profile: cfg.rbw_profile.clone(),
            not_found: AtomicBool::new(false),
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.exe);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if !self.profile.is_empty() {
            cmd.env("RBW_PROFILE", &self.profile);
        }
        cmd
    }

    /// Run rbw with a deadline. Stdout/stderr are drained on threads so a
    /// large `rbw list --raw` cannot block on a full pipe while we wait.
    pub fn run(&self, args: &[&str], timeout: Duration) -> Option<Output> {
        let mut child: Child = match self.command(args).spawn() {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.not_found.store(true, Ordering::Relaxed);
                eprintln!("[bitwarden] rbw executable not found: {}", self.exe);
                return None;
            }
            Err(e) => {
                eprintln!("[bitwarden] rbw spawn error ({args:?}): {e}");
                return None;
            }
        };
        let mut out_pipe = child.stdout.take().expect("stdout piped");
        let mut err_pipe = child.stderr.take().expect("stderr piped");
        let out_thread = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = out_pipe.read_to_end(&mut buf);
            buf
        });
        let err_thread = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = err_pipe.read_to_end(&mut buf);
            buf
        });

        let deadline = Instant::now() + timeout;
        let code = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.code(),
                Ok(None) => {
                    if Instant::now() >= deadline {
                        eprintln!("[bitwarden] rbw timed out: {args:?}");
                        let _ = child.kill();
                        let _ = child.wait();
                        break None;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break None,
            }
        };
        let stdout = String::from_utf8_lossy(&out_thread.join().unwrap_or_default()).into_owned();
        let stderr = String::from_utf8_lossy(&err_thread.join().unwrap_or_default()).into_owned();
        Some(Output {
            code,
            stdout,
            stderr,
        })
    }

    /// Stdout of a successful rbw call, without its trailing newline.
    /// Only one trailing "\n" (and a preceding "\r") is stripped, so a
    /// password ending in other whitespace survives untouched.
    pub fn output(&self, args: &[&str], timeout: Duration) -> String {
        match self.run(args, timeout) {
            Some(out) if out.code == Some(0) => {
                let mut s = out.stdout;
                if s.ends_with('\n') {
                    s.pop();
                    if s.ends_with('\r') {
                        s.pop();
                    }
                }
                s
            }
            Some(out) => {
                if let Some(first) = out.stderr.lines().next() {
                    if !first.is_empty() {
                        eprintln!("[bitwarden] rbw {args:?}: {first}");
                    }
                }
                String::new()
            }
            None => String::new(),
        }
    }

    pub fn is_not_found(&self) -> bool {
        self.not_found.load(Ordering::Relaxed)
    }

    /// Vault state via `rbw unlocked` (exit 0 = unlocked). Older rbw without
    /// that subcommand reports Locked here: searching would otherwise fire
    /// a pinentry prompt from inside Match(), which runs per keystroke.
    pub fn vault_state(&self) -> VaultState {
        if self.is_not_found() {
            return VaultState::Missing;
        }
        match self.run(&["unlocked"], Duration::from_secs(5)) {
            None => {
                if self.is_not_found() {
                    VaultState::Missing
                } else {
                    VaultState::Locked
                }
            }
            Some(out) if out.code == Some(0) => VaultState::Unlocked,
            Some(_) => VaultState::Locked,
        }
    }

    /// All login entries from `rbw list --raw`. Raw entries look like:
    /// {"id": "<uuid>", "name": "...", "user": "..." | null,
    ///  "folder": "..." | null, "uris": ["https://..."] | null,
    ///  "type": "login" | null}
    /// Only call when the vault is unlocked (see vault_state gating).
    pub fn list_items(&self) -> Vec<Item> {
        let out = match self.run(&["list", "--raw"], Duration::from_secs(10)) {
            Some(o) if o.code == Some(0) => o,
            Some(o) => {
                if let Some(first) = o.stderr.lines().next() {
                    eprintln!("[bitwarden] rbw list: {first}");
                }
                return Vec::new();
            }
            None => return Vec::new(),
        };
        let parsed: Json = match serde_json::from_str(&out.stdout) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[bitwarden] Cannot parse rbw list output: {e}");
                return Vec::new();
            }
        };
        let mut items = Vec::new();
        for raw in parsed.as_array().cloned().unwrap_or_default() {
            let id = raw.get("id").and_then(Json::as_str).unwrap_or_default();
            if id.is_empty() {
                continue;
            }
            let entry_type = raw
                .get("type")
                .and_then(Json::as_str)
                .unwrap_or_default()
                .trim()
                .to_lowercase();
            if NON_LOGIN_TYPES.contains(&entry_type.as_str()) {
                continue;
            }
            let mut uris = Vec::new();
            if let Some(list) = raw.get("uris").and_then(Json::as_array) {
                for uri in list {
                    match uri {
                        Json::String(s) => uris.push(s.clone()),
                        Json::Object(o) => {
                            if let Some(s) = o.get("uri").and_then(Json::as_str) {
                                uris.push(s.to_string());
                            }
                        }
                        _ => {}
                    }
                }
            }
            items.push(Item {
                id: id.to_string(),
                name: raw
                    .get("name")
                    .and_then(Json::as_str)
                    .unwrap_or_default()
                    .to_string(),
                // `rbw list` calls the field "user".
                username: raw
                    .get("user")
                    .and_then(Json::as_str)
                    .or_else(|| raw.get("username").and_then(Json::as_str))
                    .unwrap_or_default()
                    .to_string(),
                uris,
            });
        }
        items
    }

    pub fn get_field(&self, item_id: &str, field: &str) -> String {
        self.output(&["get", "--field", field, item_id], Duration::from_secs(10))
    }

    pub fn password(&self, item_id: &str) -> String {
        self.output(&["get", item_id], Duration::from_secs(10))
    }

    pub fn totp(&self, item_id: &str) -> String {
        self.output(&["code", item_id], Duration::from_secs(10))
    }

    pub fn lock(&self) {
        if let Some(out) = self.run(&["lock"], Duration::from_secs(10)) {
            if out.code == Some(0) {
                eprintln!("[bitwarden] Vault locked");
            }
        }
    }

    pub fn unlock(&self) {
        // Non-blocking: rbw-agent presents the pinentry prompt. Blocking the
        // D-Bus Run() call on it would freeze KRunner's view of this runner.
        let mut cmd = Command::new(&self.exe);
        cmd.arg("unlock")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if !self.profile.is_empty() {
            cmd.env("RBW_PROFILE", &self.profile);
        }
        match cmd.spawn() {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.not_found.store(true, Ordering::Relaxed);
                eprintln!("[bitwarden] rbw executable not found: {}", self.exe);
            }
            Err(e) => eprintln!("[bitwarden] Unlock error: {e}"),
        }
    }

    pub fn sync(&self) {
        self.run(&["sync"], Duration::from_secs(60));
    }

    pub fn stop_agent(&self) {
        if let Some(out) = self.run(&["stop-agent"], Duration::from_secs(10)) {
            if out.code == Some(0) {
                eprintln!("[bitwarden] rbw-agent stopped");
            }
        }
    }

    /// Generate a password with rbw. No entry name is passed, so rbw prints
    /// the result instead of saving it to the vault.
    ///
    /// Preset mapping, and its limits (rbw has fewer knobs than bw):
    ///   passphrase preset   -> rbw generate <words> --diceware
    ///   numbers-only preset -> rbw generate <length> --only-numbers
    ///   special = false     -> --no-symbols
    ///   special = true      -> no flag (symbols included)
    pub fn generate(&self, preset: &crate::config::Preset) -> Option<String> {
        use crate::config::PresetKind;
        let args: Vec<String> = match preset.kind {
            PresetKind::Passphrase => vec![
                "generate".into(),
                preset.words.to_string(),
                "--diceware".into(),
            ],
            PresetKind::Password => {
                let mut a = vec!["generate".into(), preset.length.to_string()];
                let numbers_only =
                    preset.numbers && !preset.uppercase && !preset.lowercase && !preset.special;
                if numbers_only {
                    a.push("--only-numbers".into());
                } else if !preset.special {
                    a.push("--no-symbols".into());
                }
                a
            }
        };
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = self.output(&arg_refs, Duration::from_secs(10));
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }
}
