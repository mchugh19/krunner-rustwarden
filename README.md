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
* Optional, both off by default (see Configuration): an "Open in Web
  Vault" action on each entry, which opens the entry's page in the web
  vault, the place to read an entry's notes; and secure note entries
  among the search results, where running a note entry opens it in the
  web vault too. Note text is never copied: it is free-form, can be
  long and can carry many fields, so the clipboard is a poor fit.

## Usage

Open KRunner (Alt+Space by default) and type. There is no trigger
prefix to remember.

### Finding and using an entry

Type at least three characters. An entry matches when the text
appears in its name, its username, or any of its URIs. With several
words, every word must match somewhere, so `bank example` finds the
entry named "Bank of Ireland" through its `bank.example.ie` address.
Matching is case-insensitive.

What Enter and the match's action buttons do depends on the kind of
match:

| Match | Enter | Actions |
| --- | --- | --- |
| Login entry | Copy password | Copy Username, Copy TOTP, Open URL; plus Open in Web Vault when `webvault_action` is on |
| Secure note entry (with `show_secure_notes` on) | Open in Web Vault | Open in Web Vault |
| Generator preset | Copy the generated password | none |
| Lock, Unlock, Sync, Stop agent | Run the command | none |
| Status line | nothing, it is informational | none |

Secrets (passwords, TOTP codes, generated passwords) are copied
with KDE's password hint attached, so Klipper keeps them out of its
history, and they are cleared from the clipboard after the
configured delay while they are still the clipboard content.
Username copies are ordinary clipboard text.

### Vault commands

* `bitwarden`: the status line, plus Lock and Sync while the vault
  is unlocked. The status line also appears on an empty query.
* `lock`: Lock Bitwarden (only while unlocked).
* `unlock`: Unlock Bitwarden (only while locked). rbw-agent presents
  the pinentry prompt; this plugin never sees the master password.
* `sync`: Sync Bitwarden Vault, refreshing the local vault copy.
* `stop` or `agent`: Stop rbw Agent, which also locks the vault.

### Generating a password

Type `generate`, `password` or `passphrase`. There is one match per
preset defined in the config file, and its subtext summarizes the
preset (length and symbols for passwords, word count for
passphrases). Enter copies the generated password to the clipboard.
It is never saved to the vault. Typing `password` also offers the
Unlock match while the vault is locked.

### While the vault is locked

Search does not run: no entry results appear, and typing in KRunner
never triggers a pinentry prompt per keystroke. The status line
reports LOCKED and the Unlock match is offered. After unlocking,
search works until rbw locks the vault again under its own settings.

## How it works

The binary is a D-Bus activated runner. The desktop file in
`~/.local/share/krunner/dbusplugins/` registers it with KRunner as
plugin `bitwarden` on bus name `org.kde.krunner.bitwarden`, path
`/runner`, interface `org.kde.krunner1` (`Match`, `Run`, `Actions`).
A systemd user service owns the process.

All vault access is short-lived `rbw` subprocess calls: `rbw list --raw`
for search, `rbw get` / `rbw code` for secrets at action time, `rbw lock`,
`rbw unlock`, `rbw sync`, `rbw stop-agent`, and `rbw config show` when
the web vault action runs, to take the vault address from rbw's own
`ui_url` / `base_url` settings. Search results are matched
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
webvault_action = false
show_secure_notes = false

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
* `webvault_action`: when `true`, login entry matches gain an "Open
  in Web Vault" action that opens `itemId` deep links in the web
  vault. The vault address is resolved from rbw's own configuration
  at action time (`ui_url` if set, otherwise `base_url`, otherwise
  the Bitwarden cloud vault), so self-hosted and EU accounts land in
  the right vault with nothing extra to configure here.
* `show_secure_notes`: when `true`, secure note entries appear in
  search results alongside logins. Running one opens the entry in the
  web vault, using the same address resolution as the web vault
  action, whether or not that action is enabled; it is also the
  note's only action, since a note has no password, username, TOTP
  or URL to copy or open. Note text is never copied and never part
  of the search data.

Actions are assigned per match: logins offer the copy and open
actions (plus the web vault action when enabled), notes offer only
the web vault, and the management and generator matches offer no
actions at all, since Enter alone does their job.
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
