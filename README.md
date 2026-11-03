# wowaddons

Check and update World of Warcraft addons from the command line, on Windows and Linux, without the CurseForge app or a browser.

WoW Forever is the first target. Retail, Classic, Classic Era and custom variants work the same way through profiles.

> **Reliability first:** wowaddons refuses an uncertain update rather than risk breaking an `AddOns` folder. Every change runs as a transaction with a backup, and is rolled back automatically on failure.

## Install

```bash
cargo build --release
# Windows: target/release/wowaddons.exe
# Linux:   target/release/wowaddons
```

Requires stable Rust 1.89 or newer.

## API key

Network commands (`check`, `update`, `link`, `doctor`) use the official CurseForge API. You need your own key from the [CurseForge for Studios console](https://console.curseforge.com/). Check that the API terms allow your use.

Store it once in your system's secret store:

```bash
wowaddons auth set-key     # asks for the key (hidden), checks it, then saves it
wowaddons auth status      # where the key comes from (never prints it)
wowaddons auth clear       # removes it
```

The key goes to the Windows Credential Manager, the macOS Keychain, or the Secret Service on Linux (GNOME Keyring / KWallet). For scripts: `echo "$KEY" | wowaddons auth set-key --stdin`.

The `WOWADDONS_CURSEFORGE_API_KEY` environment variable still works and takes precedence over the stored key. On a Linux machine without a desktop session (no Secret Service), use the variable and set `WOWADDONS_NO_KEYRING=1` to skip the secret store.

The key is never written to the config file, the logs or the JSON output. It is only sent to `api.curseforge.com` and the `forgecdn.net` CDN, always as a header. A redirect to any other host is refused.

Without a key, everything that works offline keeps working: `scan`, `list`, `config`, `profile`, `link --offline`, `inbox`, `import`, `history` and `rollback`.

## Quick start

```bash
wowaddons init
wowaddons auth set-key     # once
wowaddons profile add forever --client forever \
  --addons-path "C:\Games\World of Warcraft\_forever_\Interface\AddOns"
wowaddons list          # offline: what is installed
wowaddons check         # what can be updated
wowaddons update --adopt   # first time: take over addons installed by hand
wowaddons update        # from then on
```

The first profile you add becomes the default one. Use `--profile <name>` for another one, or `wowaddons profile use <name>` to change the default.

## Addon statuses

| Status | Meaning |
|---|---|
| `UP_TO_DATE` / `UPDATE_AVAILABLE` | Installed by wowaddons; the exact remote file is known. |
| `UNMANAGED` | The project is known (TOC header or `link`) but the addon was installed by hand. `update --adopt` takes it over. |
| `UNTRACKED` | No project is known. Use `wowaddons link <addon> --project-id <id>`. Never touched. |
| `NO_COMPATIBLE_RELEASE` | No file is tagged for this client. Nothing is changed. |
| `DOWNLOAD_NOT_ALLOWED` | The author blocks third-party downloads. The message gives the link to download the file by hand (see below). |
| `MISSING` | A managed folder was deleted. `update --reinstall` repairs it. |
| `PROTECTED` | Symlink, junction or `Blizzard_` folder. Never touched. |
| `CONFLICT` | TOC files disagree on the project ID, or a folder is claimed by two projects. |
| `ERROR` | The check failed for this addon (see the message). |

Updates are decided from file IDs and publication dates, never from version strings. wowaddons never downgrades on its own; `--file-id` is the only way to install an older file.

## Addons that block third-party downloads

Some authors do not allow tools like wowaddons to download their files. wowaddons does not work around this, but it keeps the manual step short:

```bash
wowaddons check     # DOWNLOAD_NOT_ALLOWED, with a direct link to the file on CurseForge
wowaddons inbox     # opens the folder where the downloaded ZIP files go
wowaddons import    # installs them (offline, no API key needed)
```

`import` uses the same safety checks, plan, confirmation, transaction and backup as `update`, and `rollback` can undo it. It finds the addon from the TOC files in the ZIP (or the installed folders with the same names), only updates addons already installed, and removes the ZIP files from the inbox once installed. The archive's SHA-1 is recorded, so the next `check` knows exactly which CurseForge file is installed.

Archives that cannot be installed stay in the inbox, with a reason: `UNTRACKED` (no project found; `link` the installed addon first), `NOT_INSTALLED`, `DUPLICATE` (two ZIP files for one addon), `CONFLICT`, `ERROR` (invalid archive). An archive identical to the installed one is `ALREADY_INSTALLED` and is removed.

## Commands

| Command | What it does |
|---|---|
| `init` | Create the config file (never overwrites it). |
| `scan` | Detailed analysis of the AddOns folder: chosen TOC, metadata, warnings. Read-only. |
| `list` | Short table of addons and their tracking status. Offline, read-only. |
| `check` | Compare with compatible remote files. Read-only. `--fail-on-updates` exits with 10 when updates exist. |
| `update` | Download, check, show the plan, confirm, then install in one transaction. |
| `doctor` | Check config, paths, volumes, free space, API access and pending transactions. |
| `inbox` | Open the profile's inbox, the folder for ZIP files downloaded by hand. `--no-open` only prints its path. |
| `import` | Install the ZIP files of the inbox in one transaction. Offline. `--dry-run`, `--yes`. |
| `link <addon> --project-id <id>` / `unlink <addon>` | Explicit link between a folder and a CurseForge project. |
| `history` | Past transactions of the profile. |
| `rollback [<id>]` | Revert the most recent committed transaction (strict reverse order). |
| `config list\|get\|set\|unset` | Global settings. `config list` shows each value and its source. |
| `profile add\|list\|show\|set\|unset\|use\|remove` | Profiles. `remove` never touches `AddOns`; `--purge` also deletes its state and backups. |
| `auth set-key\|status\|clear` | Store the API key in the system secret store, show where it comes from, or remove it. |

Useful `update` options: `--dry-run`, `--yes`, `--addon <name>` (repeatable), `--adopt`, `--channel beta`, `--file-id <id>` (with one `--addon`), `--reinstall`, `--allow-client-mismatch`.

One-off overrides for `check` and `update`: `--client`, `--game-version`, `--game-version-type-id`, `--channel`. They never change the config. On `update`, a client other than the profile's is refused unless `--allow-client-mismatch` is given.

Global options: `--profile`, `--json`, `-v`/`-q`, `--config <path>`, `--offline`.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success (including `check` that finds updates) |
| 1 | General error |
| 2 | Usage error (invalid arguments; confirmation needed without a terminal) |
| 3 | Missing API key, or authentication refused |
| 4 | Installation failed and was rolled back cleanly |
| 5 | Blocked by an incompatibility or a conflict |
| 6 | Partial failure: at least one addon failed |
| 7 | Rollback or recovery failed: manual action needed (run `doctor`) |
| 8 | Cancelled by the user; nothing changed |
| 10 | Updates available (`check --fail-on-updates`) |

## Scripts and JSON

`--json` prints one JSON document on stdout:

```json
{ "schema_version": 1, "command": "check", "ok": true, "exit_code": 0, "result": { ... } }
```

On error, `result` is replaced by `"error": { "kind", "message", "hint" }`. Logs, warnings and progress always go to stderr. Without a terminal, or with `--json`, commands that need confirmation fail unless `--yes` is given.

## Files

| What | Windows | Linux |
|---|---|---|
| Config | `%APPDATA%\wowaddons\config.toml` | `$XDG_CONFIG_HOME/wowaddons/config.toml` (`~/.config/…`) |
| State, journals, backups | `%LOCALAPPDATA%\wowaddons\` | `$XDG_DATA_HOME/wowaddons/` (`~/.local/share/…`) |
| Inbox (manual downloads) | `%LOCALAPPDATA%\wowaddons\profiles\<profile>\inbox\` | `$XDG_DATA_HOME/wowaddons/profiles/<profile>/inbox/` |
| Work dir (staging) | `<parent of AddOns>\.wowaddons-work\<profile>\` | same |

Overrides: `--config` or `WOWADDONS_CONFIG`, and `WOWADDONS_DATA_DIR`.

The work dir must be on the same volume as `AddOns`, so that folders can be swapped with atomic renames. Change it with `wowaddons profile set <profile> work_dir <path>`.

## How an update is made safe

1. The profile is locked. A second process fails right away.
2. Any interrupted transaction is finished or rolled back first.
3. Archives are downloaded and checked before anything changes: size, SHA-1, ZIP safety (no `..`, absolute paths, symlinks, Windows reserved names, case clashes, size and entry limits), and every root folder must be a valid addon.
4. The plan lists the folders that will be added, replaced and removed. Folders owned by another addon, protected folders and folders identified as another project block that addon.
5. In the transaction, every step is journaled before it runs. Old folders are moved aside, new ones moved in, then the state is written and the transaction committed.
6. On any failure, or on Ctrl+C, everything is restored. If the process is killed, the next `update` or `rollback` restores it.
7. Replaced folders are kept as a backup (`defaults.backup_retention`, 5 per profile by default).

`WTF` is never read or written.

## Limits and assumptions

wowaddons relies on a few facts about CurseForge and the game clients. They were checked against the live API and a WoW Forever install on 2026-11-03, and each one is kept in a single place:

| Fact | Status | Where |
|---|---|---|
| WoW is `gameId` 1 on CurseForge. | Confirmed | `src/providers/curseforge.rs` |
| Published files have `fileStatus` 4 (Approved) or 10 (Released). | Only 4 seen on real files; 10 comes from the API docs | `src/providers/curseforge.rs` |
| Version types: Forever = 88568, Retail = 517, Classic (Mists of Pandaria) = 79434, Classic Era = 67408. CurseForge calls Classic Era just "WoW Classic". | Confirmed | aliases in `src/domain/client.rs` |
| Forever reads `_Camelot` TOC files. | Likely: the client lists `camelot` among its game types, but no addon ships a `_Camelot.toc` yet. Forever addons use the plain `.toc` (`## Interface: 16001`), which works. | `src/domain/client.rs` |

The version type is found from the type's name, so a rename on CurseForge can break it. If that happens, or for a client with no built-in entry (for example Burning Crusade Classic, 73246), set the ID yourself:

```bash
wowaddons profile set <profile> game_version_type_id <id>
```

Not in this version: a GUI, installing new projects, dependency resolution, Wago/WoWInterface/GitHub sources, fingerprint matching.

## Development

```bash
cargo build
cargo test
cargo fmt
cargo clippy --all-targets -- -D warnings
```

Tests never need a real API key or network access: they use a fake CurseForge server (`wiremock`) and temporary folders.

For manual tests, keep your real settings untouched:

```bash
WOWADDONS_CONFIG=./dev/config.toml WOWADDONS_DATA_DIR=./dev/data cargo run -- list
```

| Module | Role |
|---|---|
| `cli`, `commands/` | Argument parsing and one module per command |
| `output` | Text and JSON output, prompts, progress |
| `config` | Config file reading, validation and edits (comments kept) |
| `scanner/` | `AddOns` scan, TOC parsing, grouping into projects |
| `providers/` | `AddonProvider` trait and the CurseForge backend |
| `net` | HTTP client: retries, backoff, redirect allowlist, key masking |
| `resolver/` | Version types, eligibility, release selection |
| `installer/` | Archive checks, plan, journal, transactions, backups, recovery |
| `storage/` | Paths, locks, state, atomic writes |
