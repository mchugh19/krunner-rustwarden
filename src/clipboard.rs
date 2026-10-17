//! Clipboard access, KDE first.
//!
//! Primary route is Klipper, Plasma's clipboard manager, over the session
//! bus: org.kde.klipper /klipper, interface org.kde.klipper.klipper, method
//! setClipboardContents. That needs no external tool, works the same on
//! X11 and Wayland, and gives us getClipboardContents /
//! clearClipboardContents for the auto-clear check below.
//!
//! If Klipper is not running (clipboard applet disabled), fall back to the
//! usual external tools: wl-copy, then xclip, then xsel.
//!
//! Klipper calls use their own short-lived bus connection, never the
//! runner's connection: copy() is invoked from inside D-Bus method
//! handlers, and making an outgoing blocking call on the same connection
//! that is dispatching the handler risks a deadlock.
//!
//! Note: setClipboardContents takes a plain string, so there is no way to
//! attach KDE's x-kde-passwordManagerHint. Secrets copied this way can land
//! in Klipper's history, exactly as they do with wl-copy / xclip. The
//! timed clear below limits how long they stay on the live clipboard.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

const KLIPPER_DEST: &str = "org.kde.klipper";
const KLIPPER_PATH: &str = "/klipper";
const KLIPPER_IFACE: &str = "org.kde.klipper.klipper";

fn klipper_set(text: &str) -> zbus::Result<()> {
    let conn = zbus::blocking::Connection::session()?;
    conn.call_method(
        Some(KLIPPER_DEST),
        KLIPPER_PATH,
        Some(KLIPPER_IFACE),
        "setClipboardContents",
        &text,
    )?;
    Ok(())
}

fn klipper_get() -> Option<String> {
    let conn = zbus::blocking::Connection::session().ok()?;
    let msg = conn
        .call_method(
            Some(KLIPPER_DEST),
            KLIPPER_PATH,
            Some(KLIPPER_IFACE),
            "getClipboardContents",
            &(),
        )
        .ok()?;
    msg.body().deserialize::<String>().ok()
}

fn klipper_clear() -> bool {
    let Ok(conn) = zbus::blocking::Connection::session() else {
        return false;
    };
    conn.call_method(
        Some(KLIPPER_DEST),
        KLIPPER_PATH,
        Some(KLIPPER_IFACE),
        "clearClipboardContents",
        &(),
    )
    .is_ok()
}

fn tool_copy(text: &str) -> bool {
    for cmd in [
        vec!["wl-copy"],
        vec!["xclip", "-selection", "clipboard"],
        vec!["xsel", "--clipboard", "--input"],
    ] {
        match Command::new(cmd[0])
            .args(&cmd[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(text.as_bytes());
                }
                if let Ok(status) = child.wait() {
                    if status.success() {
                        return true;
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => continue,
        }
    }
    false
}

fn tool_set_empty() {
    // Best-effort live-clipboard clear when Klipper is unavailable.
    let _ = tool_copy("");
}

/// Copy text to the clipboard.
///
/// Secret copies (passwords, TOTP codes, generated passwords) are first
/// offered through our own Wayland selection carrying
/// x-kde-passwordManagerHint, so Klipper keeps them out of its history
/// (see selection.rs). When that route is unavailable, and for
/// non-secret copies (usernames), the Klipper / external-tool chain
/// below is used. When clear_seconds > 0 on the fallback chain, a
/// detached thread clears the clipboard after that delay, but only if
/// the clipboard still holds exactly this text.
pub fn copy(text: &str, clear_seconds: u64, secret: bool) {
    if text.is_empty() {
        return;
    }
    if secret {
        let clear_after = if clear_seconds > 0 {
            Some(std::time::Duration::from_secs(clear_seconds))
        } else {
            None
        };
        if crate::selection::set_secret(text, clear_after) {
            return;
        }
        // Fall through: better an unmarked copy than none at all.
    }
    if klipper_set(text).is_err() && !tool_copy(text) {
        eprintln!("[bitwarden] No clipboard route worked (selection, Klipper, wl-copy, xclip, xsel)");
        return;
    }
    if clear_seconds > 0 {
        let secret = text.to_string();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(clear_seconds));
            match klipper_get() {
                Some(current) if current == secret => {
                    if !klipper_clear() {
                        tool_set_empty();
                    }
                }
                Some(_) => {} // clipboard has moved on; leave it alone
                None => {
                    // Cannot verify contents without Klipper. Clearing
                    // blindly could delete something the user copied
                    // after the secret, so do nothing.
                }
            }
        });
    }
}
