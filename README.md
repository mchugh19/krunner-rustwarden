# krunner-rustwarden

A KRunner plugin for Bitwarden, written in Rust, backed by
[rbw](https://github.com/doy/rbw) instead of the official `bw` CLI.

## Why Rust

A plugin that hooks your password vault should not depend on a language
runtime staying put. An interpreted plugin breaks when the system
interpreter stack rebases underneath it, which on an image-based system
(Fedora Atomic / Aurora and friends) happens routinely, and layering
the packages to pin it is exactly what you are trying to avoid. This
binary's only dynamic dependencies are the C library and the usual
system basics (run `ldd` on it and see). D-Bus goes through zbus, which
is pure Rust.

## Requirements

* KDE Plasma 6 with KRunner
* `rbw` installed and configured (`rbw config set ...`, then register /
  log in as per the rbw README)
* A pinentry rbw can use for unlock prompts. For a GUI prompt:
  `rbw config set pinentry pinentry-qt`, then `rbw stop-agent` once.
* For building: a Rust toolchain (`cargo build --release`)

## Features

* No trigger prefix. Type at least 3 characters and matching vault
  entries appear; multiple words narrow the search, and tokens can match
  the entry name, its username, or any of its URIs.
* Actions per entry: copy password (default, Enter), copy username,
  copy TOTP code, open the entry's first URL in the browser.
* Copying a secret (password, TOTP code, generated password) sets the
  Wayland clipboard directly, from a selection owned by the runner
  itself, with KDE's `x-kde-passwordManagerHint` attached, so Klipper
  keeps it out of its clipboard history (the same mechanism KeePassXC
  and Firefox use). If that route is unavailable, and for username
  copies, copying goes through Klipper (Plasma's clipboard manager)
  over D-Bus, with `wl-copy` / `xclip` / `xsel` as further fallbacks.
  Copied secrets are cleared from the clipboard after a configurable
  delay; on the owned-selection route the clear happens only while the
  runner still owns the selection, and on the fallback route only if
  the clipboard still holds that exact text.
* Vault management matches: Lock Bitwarden, Unlock Bitwarden (pinentry
  prompt from rbw-agent; this plugin never sees your master password),
  Sync Bitwarden Vault, Stop rbw Agent, plus a status line.
* Password generation from presets in the config file. The generated
  password is copied to the clipboard; it is never saved to the vault.

## How it works

The binary is a D-Bus activated runner. The desktop file in
`~/.local/share/krunner/dbusplugins/` registers it with KRunner as
plugin `bitwarden` on bus name `org.kde.krunner.bitwarden`, path
`/runner`, interface `org.kde.krunner1` (`Match`, `Run`, `Actions`).
A systemd user service owns the process.

All vault access is short-lived `rbw` subprocess calls: `rbw list --raw`
for search, `rbw get` / `rbw code` for secrets at action time, `rbw lock`,
`rbw unlock`, `rbw sync`, `rbw stop-agent`. Search results are matched
locally; the runner caches only names, usernames and URIs, never
passwords. While the vault is locked, typing in KRunner does not call
`rbw list` at all, so it cannot pop a pinentry prompt per keystroke.

## Install

```sh
cargo build --release
./install.sh
systemctl --user start krunner-rustwarden
```

`install.sh` installs the binary, the KRunner desktop file and the
systemd user unit, writes a default config if none exists, and enables
the service. The last command starts it; after an upgrade, restart it
instead (`systemctl --user restart krunner-rustwarden`) so the new
binary is picked up.

Two things worth checking on a fresh install:

* In System Settings → Search → Plasma Search, make sure
  "Bitwarden (rbw)" is ticked. A newly installed runner can start
  unticked, in which case KRunner never queries it.
* The systemd user service runs with a minimal PATH. If the runner's
  status match says "rbw not found", set `rbw_executable_path` in the
  config to the full path of the `rbw` binary (`which rbw` in a shell)
  and restart the service.

## Uninstall

```sh
./uninstall.sh
```

The config file is left in place.

## Configuration

`~/.config/krunner-rustwarden/config.ini`:

```ini
[bitwarden]
rbw_executable_path =
rbw_profile =
clipboard_clear_seconds = 30

[generator.default]
type = password
length = 16
symbols = false
```

* `rbw_executable_path`: full path to rbw. Empty means resolve from
  PATH.
* `rbw_profile`: passed to every rbw call as `RBW_PROFILE`.
* `clipboard_clear_seconds`: delay before a copied secret is cleared
  from the clipboard (only when the clipboard still holds that secret).
  0 disables.
* `[generator.<label>]`: one KRunner match per preset. Presets are
  written in rbw's own terms, so every key takes effect:
  * `type = password`: `length` characters. `symbols = false` passes
    `--no-symbols` (symbols are included by default), `numbers_only =
    true` passes `--only-numbers`, and `nonconfusables = true` passes
    `--nonconfusables`.
  * `type = passphrase`: `words` diceware words (`--diceware`).

## Security notes

* The master password is never handled by this program. Unlock is
  `rbw unlock`, which prompts through rbw-agent and your pinentry.
* Passwords are fetched from rbw only when you run an action, and are
  passed to the clipboard service in memory. They are not written to
  disk by this program and not kept in the search cache.
* Clipboard history: on Wayland, secret copies carry
  `x-kde-passwordManagerHint`, so Klipper does not store them in its
  history. The fallback routes (Klipper's `setClipboardContents`,
  `wl-copy`, `xclip`) cannot attach that hint, so a secret copied
  through a fallback can appear in Klipper's history; the timed clear
  still removes it from the live clipboard. There is no X11 session
  support: current Plasma has dropped the X11 session, and the
  data-control protocols this uses are Wayland-only.

## License

MIT

## Prior art

* [ccarpo/krunner-bitwarden](https://github.com/ccarpo/krunner-bitwarden):
  a KRunner plugin for Bitwarden written in Python, backed by the
  official `bw` CLI.
