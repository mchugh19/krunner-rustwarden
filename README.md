# krunner-rustwarden

A KRunner plugin for Bitwarden, written in Rust, backed by
[rbw](https://github.com/doy/rbw) instead of the official `bw` CLI.

It is a from-scratch Rust implementation of the behaviour of
[ccarpo/krunner-bitwarden](https://github.com/ccarpo/krunner-bitwarden)
(as patched to an rbw backend on the `work/rbw-backend` branch of
[mchugh19/krunner-bitwarden](https://github.com/mchugh19/krunner-bitwarden)):
same D-Bus interface, same bus name, same config file, same features.
What changes is the runtime footprint: one compiled binary instead of a
Python interpreter plus dbus-python and PyGObject.

## Why Rust

The Python runner depends on the system Python stack matching what the
plugin was installed against. On an image-based system (Fedora Atomic /
Aurora and friends) that stack rebases underneath you, and layering the
packages is exactly what you are trying to avoid. This binary's only
dynamic dependencies are the C library and the usual system basics (run
`ldd` on it and see). D-Bus goes through zbus, which is pure Rust.

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
* Copying goes through Klipper (Plasma's clipboard manager) over D-Bus,
  with `wl-copy` / `xclip` / `xsel` as fallbacks if Klipper is not
  running. Copied secrets are cleared from the clipboard after a
  configurable delay, but only if the clipboard still holds that exact
  text.
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
```

`install.sh` installs the binary, the KRunner desktop file and the
systemd user unit, writes a default config if none exists, and enables
the service. Do not run this and the Python version at the same time:
both own the bus name `org.kde.krunner.bitwarden`. `install.sh` warns
if the Python service is enabled.

## Uninstall

```sh
./uninstall.sh
```

The config file is left in place (it is shared with the Python version).

## Configuration

`~/.config/krunner-bitwarden/config.ini`, shared with the Python
version, so presets and paths carry over:

```ini
[bitwarden]
rbw_executable_path =
rbw_profile =
clipboard_clear_seconds = 30

[generator.default]
type = password
length = 16
uppercase = true
lowercase = true
numbers = true
special = false
```

* `rbw_executable_path`: full path to rbw. Empty means resolve from
  PATH. (The old `bw_executable_path` key is still honoured as an alias,
  and an old literal value of `bw` is treated as unset.)
* `rbw_profile`: passed to every rbw call as `RBW_PROFILE`.
* `clipboard_clear_seconds`: delay before a copied secret is cleared
  from the clipboard (only when the clipboard still holds that secret).
  0 disables.
* `[generator.<label>]`: one KRunner match per preset.

### Generator preset mapping (rbw limits)

rbw's generator has fewer knobs than the config format (which comes from
`bw generate`). The mapping is:

* passphrase preset: `rbw generate <words> --diceware`. The `separator`,
  `capitalize` and `include_number` options have no diceware equivalent
  in rbw and are ignored.
* numbers-only preset (numbers on, upper/lower/special off):
  `rbw generate <length> --only-numbers`.
* `special = false`: `--no-symbols`.
* `special = true`: no flag (rbw includes symbols by default).
* The `uppercase` / `lowercase` toggles have no rbw flag and are ignored,
  apart from their role in the numbers-only detection above.

## Security notes

* The master password is never handled by this program. Unlock is
  `rbw unlock`, which prompts through rbw-agent and your pinentry.
* Passwords are fetched from rbw only when you run an action, and are
  passed to the clipboard service in memory. They are not written to
  disk by this program and not kept in the search cache.
* Klipper caveat: copying through Klipper's `setClipboardContents`
  cannot attach KDE's `x-kde-passwordManagerHint`, so a copied secret
  can appear in Klipper's clipboard history, the same as with `wl-copy`
  or `xclip`. The timed clear removes it from the live clipboard; clear
  Klipper history (or exclude passwords there) if that matters to you.

## License

MIT
