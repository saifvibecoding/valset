# VALSYNC

VALSYNC is a VALORANT settings manager that runs in CMD, PowerShell or Windows Terminal. It saves, transfers and restores settings across Riot accounts, and it includes a match tracker. It has no GUI and runs no background service, and it never touches the game, Vanguard or your Riot credentials.

```
 ██╗   ██╗ █████╗ ██╗     ███████╗██╗   ██╗███╗   ██╗ ██████╗
 ██║   ██║██╔══██╗██║     ██╔════╝╚██╗ ██╔╝████╗  ██║██╔════╝
 ██║   ██║███████║██║     ███████╗ ╚████╔╝ ██╔██╗ ██║██║
 ╚██╗ ██╔╝██╔══██║██║     ╚════██║  ╚██╔╝  ██║╚██╗██║██║
  ╚████╔╝ ██║  ██║███████╗███████║   ██║   ██║ ╚████║╚██████╗
   ╚═══╝  ╚═╝  ╚═╝╚══════╝╚══════╝   ╚═╝   ╚═╝  ╚═══╝ ╚═════╝
```

* **One standalone `valsync.exe`** (about 5 MB). It needs no installer or runtime and imports only Windows system DLLs.
* **About 8 MB peak memory** and 1–5 ms per command. Idle CPU in the interactive menu measured 0.00 % (see [Benchmarks](#benchmarks)).
* **Every change is transactional.** VALSYNC takes an automatic backup first, then writes atomically, verifies byte by byte, and rolls back automatically on failure. Undo, redo, restoring the original settings and crash recovery are built in.

---

## Contents

1. [Installation](#installation)
2. [Quick start: copy settings from account A to account B](#quick-start)
3. [What can and cannot be transferred](#what-can-and-cannot-be-transferred)
4. [Interactive menu](#interactive-menu)
5. [Direct commands](#direct-commands)
6. [Backups, undo/redo and recovery](#backups-undoredo-and-recovery)
7. [Riot accounts](#riot-accounts)
8. [Live Match and Match History](#live-match-and-match-history)
9. [Configuration](#configuration)
10. [Security and fair-play guarantees](#security-and-fair-play-guarantees)
11. [Research notes and what needs Riot approval](#research-notes-and-what-needs-riot-approval)
12. [Benchmarks](#benchmarks)
13. [Building and testing](#building-and-testing)
14. [Architecture](#architecture)

---

## Installation

1. Copy `dist/valsync.exe` (or a build from [Building](#building-and-testing)) to a folder, for example `C:\Tools\VALSYNC\`.
2. Optionally add that folder to your `PATH` so `valsync` works from any terminal:
   ```powershell
   setx PATH "$env:PATH;C:\Tools\VALSYNC"
   ```
3. Open **Windows Terminal**, **PowerShell** or **CMD** and run:
   ```powershell
   valsync          # interactive menu
   valsync doctor   # check paths, permissions, database and API access
   ```

VALSYNC stores its data in `%APPDATA%\VALSYNC\`:

| File | Contents |
|------|----------|
| `valsync.db` | SQLite database with accounts, profiles, backups, the operation journal and the match cache |
| `config.toml` | Application settings |

You don't need administrator rights. To uninstall, delete the exe and `%APPDATA%\VALSYNC`.

> Windows SmartScreen may warn about an unsigned executable. Build from source if you prefer (see below).

---

## Quick start

| Step | What you do |
|------|-------------|
| 1 | Log into **account A** in the Riot Client and start VALORANT once, so it writes A's config. Then close the game. |
| 2 | Run `valsync`, choose **[1] Save Current Settings** and name the profile, e.g. `Main`. |
| 3 | Switch to **account B** in the Riot Client the normal way and start VALORANT once. Then close the game. VALSYNC detects the switch. |
| 4 | Choose **[2] Load Saved Settings**, pick `Main`, select categories (Space toggles, `a` selects all) and review the preview. |
| 5 | Confirm. VALSYNC backs up B's current settings, applies the compatible settings and verifies every file. |
| 6 | Work through the **manual checklist** VALSYNC prints for account-synced settings (see below), then start VALORANT. |
| ↶ | Changed your mind? Choose **[3] Restore Previous Settings → Undo**, or run `valsync undo`. |

The same steps as commands:

```powershell
valsync save "Main"          # on account A
# switch to account B in the Riot Client, launch VALORANT once, close it
valsync load "Main"          # preview + confirmation + automatic backup
valsync restore              # puts B's previous settings back
```

Profiles are not tied to any account, so you can load the same profile onto any future account.

---

## What can and cannot be transferred

VALORANT keeps two kinds of settings, and VALSYNC labels every setting with its scope instead of pretending everything transferred.

| Category | Where VALORANT stores it | VALSYNC |
|---|---|---|
| Resolution | Local `.ini` | ✅ Saved, transferred, restored |
| Graphics quality | Local `.ini` | ✅ Saved, transferred, restored |
| Display mode | Local `.ini` | ✅ Saved, transferred, restored |
| FPS limits | Local `.ini` | ✅ Saved, transferred, restored |
| NVIDIA Reflex | Local `.ini` | ✅ Saved, transferred, restored |
| Mouse sensitivity | Riot servers (account-synced) | 📝 Recorded; manual checklist |
| Scoped / ADS sensitivity | Riot servers | 📝 Recorded; manual checklist |
| Crosshair profiles | Riot servers | 📝 Recorded; manual checklist. Store your crosshair code with `--crosshair-code` |
| Keybinds | Riot servers | 📝 Recorded; manual checklist |
| Agent-specific keybinds | Riot servers | 📝 Recorded; manual checklist |
| Audio | Riot servers (audio *quality* is local) | ✅ local keys / 📝 rest |
| Minimap, HUD, enemy highlight colour, accessibility, general gameplay | Riot servers | 📝 Recorded; manual checklist |
| PUUIDs, Riot IDs, tokens, session data | Account specific | ⛔ Never stored or transferred |
| Monitor device IDs, window position, GPU/CPU benchmark results | Specific to this PC | ⛔ Excluded |
| Skins, chromas, buddies, cards, titles, sprays | Account-owned cosmetics | ⛔ Excluded |

**Why can't sensitivity and crosshair be applied automatically?** Since VALORANT added settings sync, these values live on Riot's servers. The client downloads them at login and overwrites any local copy. Riot publishes no supported API to read or write them; the only ways to do it are private endpoints that need your session token, which VALSYNC refuses to use. So VALSYNC does three things:

* It captures any copy of these values that it finds in the local cache files and shows them as a **checklist** after loading. You can also attach your own values to a profile, e.g. `valsync profiles note Main "Crosshair code" "0;P;c;5;…"`. VALORANT can import crosshair codes in-game.
* It offers a best-effort option, `write_cloud_cache = true`, that also writes those values into the local cache files. This is **off by default and unreliable**, because the game normally replaces the values from the server at login. Everything it writes is still backed up and can be undone.
* `valsync doctor` and `valsync save` print this support table for your actual files.

Key names differ between patches, so VALSYNC classifies keys by pattern (`src/catalog.rs`). It never drops an unknown key silently. Unknown keys are captured under *General gameplay* or *Other*. Keys that look like identifiers are rejected: any UUID value, anything containing `token`, `auth`, `session` or `puuid`, and any value containing the source account's PUUID.

---

## Interactive menu

```
[1] Save Current Settings       [6] Live Match
[2] Load Saved Settings         [7] Match History
[3] Restore Previous Settings   [8] Backup History
[4] Manage Settings Profiles    [9] Application Settings
[5] Manage Riot Accounts        [0] Exit
```

The header shows:

* the active Riot account and how it was detected
* Riot Client and VALORANT status (coloured indicators)
* the detected game patch
* the last saved profile
* how many backups exist
* whether undo or redo is available

Navigate with **↑/↓**, **Enter**, **Esc** and the **number keys**. Every screen lists its own keys in the footer. A spinner appears during background work, and the layout adapts automatically when the terminal is resized.

| Screen | Keys |
|---|---|
| Profiles | `Enter` view · `r` rename · `d` duplicate · `x` delete · `f` favourite · `c` compare (press on two profiles) · `e` export · `i` import · `p` set as preferred for the active account · `v` patch/compatibility check |
| Accounts | `s` select (manual fallback) · `n` set Riot ID · `i` look up Riot ID via API · `l` link with RSO · `u` withdraw consent · `p` preferred profile · `a` automatic detection · `x` forget |
| Backups | `Enter` preview and restore · `v` verify · `V` verify all · `m` manual backup · `x` delete · `a` all accounts |

---

## Direct commands

```
valsync                          interactive menu
valsync status                   account, Riot Client / VALORANT status, patch, backups
valsync accounts                 remembered accounts
   accounts name "Name#TAG"      set/verify the Riot ID of the active account
   accounts select <account>     manual fallback selection (validated against local folders)
   accounts auto                 back to automatic detection
   accounts link                 Riot Sign On opt-in (approved RSO client required)
   accounts unlink               withdraw consent + delete stored match data
   accounts prefer <profile>     preferred profile for an account
   accounts forget <account>     forget an account (game files and backups are kept)
   accounts switches             detected account switches
valsync save "Main"              save the active account's settings
   --overwrite  --categories graphics,fps  --note "…"  --crosshair-code "…"  --sens 0.35  --account <acct>
valsync load "Main"              preview → confirm → backup → apply → verify
   --categories sensitivity,crosshair,graphics  --dry-run  --account <acct>
valsync restore                  restore the settings from before the last change (= undo)
   --backup <id>  --original  --dry-run  --account <acct>
valsync undo / valsync redo      move through the transfer history of the active account
valsync profiles                 list profiles (★ favourites first)
   profiles show|rename|duplicate|delete|favorite|check|note
valsync backups [--all]          backup history
   backups create [label] | verify [id] | show <id> | delete <id>
valsync compare "Main" "Old"     setting-by-setting comparison
valsync export "Main" [-o file]  write main.valsync (no account data)
valsync import main.valsync      checksum-verified, re-sanitised import [--name] [--overwrite]
valsync live [--once]            live match dashboard (opens the TUI screen)
valsync history [-n 10]          match history for an RSO-linked account [--offline]
valsync doctor                   diagnostics
valsync config [show|get|set|path]
valsync bench                    measure timings and memory on this PC (synthetic data)
valsync help                     help (also: valsync <command> --help)
```

Global flags are `-y/--yes`, which answers confirmation prompts, and `--no-color`. When input isn't interactive, a command that needs confirmation refuses to run unless you pass `--yes`, so scripts can't overwrite anything silently.

You can refer to an account (`<account>`) by PUUID, PUUID prefix (8+ characters), config folder name, or known `Name#TAG`. Category names are:

`sensitivity, scoped, crosshair, keybinds, agent-keybinds, resolution, graphics, display, fps, reflex, audio, minimap, hud, enemy-highlight, accessibility, gameplay, other`

---

## Backups, undo/redo and recovery

Every change (load, restore, undo, redo) runs through the same transaction:

1. **Refuse if VALORANT is running.** The game rewrites its config when it exits. You can stay logged into the Riot Client.
2. **Take the *original* snapshot**, the first time VALSYNC touches an account. **Restore original settings** uses it.
3. **Take a pre-change snapshot** of every config file of the account. Each file is stored with its SHA-256 hash, plus a manifest hash, and committed to SQLite before anything is written.
4. **Write a `pending` row to the operation journal.**
5. **Write each file atomically:** temp file → fsync → rename. The read-only attribute is preserved if you set one.
6. **Re-read and verify every file byte by byte.** If verification or a write fails, every file is rolled back automatically.
7. **Take a post-change snapshot** (used by redo) and mark the operation `committed`.

The backup system gives you:

* **One-click restore and undo.** In **Restore Previous Settings**, undo is the first entry; from the command line, run `valsync restore` or `valsync undo`. For example, if account B had sensitivity 0.25 and the profile changed it to 0.176, undo puts back 0.25 together with every other setting that changed, and restores the files byte for byte.
* **Redo.** Undone transfers can be re-applied. A new transfer clears the redo stack, as in any editor.
* **Restore original settings** with `valsync restore --original`.
* **Account-specific history.** A backup always targets its own account's config folder. VALSYNC refuses to restore it while a different account is active, unless you pass `--account` explicitly, so it can never put one account's settings onto another.
* **Preview.** Every restore, undo and redo first shows the exact setting changes.
* **Integrity verification.** Backups are verified before every restore, and with `backups verify` or `doctor`. A corrupted backup is flagged and never restored.
* **Crash recovery.** If VALSYNC is killed or the PC loses power mid-transfer, the `pending` journal row survives. On the next start, VALSYNC rolls the folder back from the pre-change snapshot and removes stray temp files. If VALORANT is running at that moment, recovery waits until the game is closed.
* **Unlimited manual backups** with `valsync backups create "before patch"`. You can cap automatic backups with `auto_backup_retention`; manual and original backups are never pruned.

---

## Riot accounts

VALSYNC detects accounts only from VALORANT's normal config files:

| Source | What VALSYNC learns |
|---|---|
| `Config\<PUUID>-<shard>\` | One folder per account that has launched VALORANT on this PC. This gives the PUUID and region shard. |
| `Config\RiotLocalMachine.ini` → `[UserInfo] LastKnownUser=` | The account that most recently ran VALORANT. This is the *active* account. |

How detection works:

* **Switching.** When `LastKnownUser` changes, VALSYNC records an account switch, which you can list with `accounts switches`, and shows a banner in the menu. Switch accounts normally in the Riot Client, then launch VALORANT once.
* **Fallbacks.** If there is no last-user record, VALSYNC uses your manual selection, which it validates against an existing account folder. If there is only one folder, it uses that. Otherwise it uses the most recently modified folder and **asks you to confirm** before writing anything.
* **Riot ID** (`Name#TAG` and region). VALSYNC fills this in from your own entry (`accounts name`). If an API key is configured, it verifies the Riot ID against ACCOUNT-V1 and marks the account *API verified* when the PUUIDs match. If RSO is configured, signing in marks the account *RSO verified*.
* **What VALSYNC never touches.** It never asks for passwords. It never reads `RiotGamesPrivateSettings.yaml`, cookies, the Riot Client lockfile or local API, or any token.

### Riot Sign On (RSO)

Riot requires every VALORANT product to get each player's opt-in through RSO. RSO is OAuth 2.0: you sign in on Riot's own page in your browser.

`valsync accounts link` does the following:

1. Opens `auth.riotgames.com`.
2. Receives the authorization code on `http://127.0.0.1:<port>/oauth-callback`.
3. Exchanges the code for an access token.
4. Calls `ACCOUNT-V1 /accounts/me` once, then discards the token.

Only the PUUID, the Riot ID and the consent timestamp are stored. RSO needs a **client ID and secret issued by Riot to an approved product**, together with the registered redirect URI. See [What needs Riot approval](#research-notes-and-what-needs-riot-approval).

---

## Live Match and Match History

* **Live Match** (menu **[6]** or `valsync live`) shows a 5v5 dashboard with these columns: Riot ID, agent, rank, peak recorded rank, RR, peak RR, level, K/D, HS%, win rate, Vandal skin, Phantom skin and variant. Ranks use their own colours: Iron gray, Bronze brown, Silver light gray, Gold yellow, Platinum cyan, Diamond purple, Ascendant green, Immortal red, Radiant bright gold.
* **There is no supported live-match API for VALORANT.** VAL-MATCH-V1 returns completed matches only. During a match the screen therefore shows only:
  * your own map, read from your own client's log
  * *your* consented stats: current rank, peak recorded rank, level, K/D, HS% and win rate, computed from your stored matches

  Every other cell shows **N/A**. VALSYNC never identifies, ranks or scouts other players in a live match.
* **Post-match dashboard (fallback).** When the game is closed, or a new match appears, Live Match shows the latest completed match: map, mode, score, team composition, each player's agent, K/D/A and HS%. These are the values on the end-of-game scoreboard. Rank and level are shown only for players who consented through VALSYNC. Turn on `hide_other_names` to hide other players' Riot IDs.
* **Polling** is gentle. The default interval is 120 s (`live_poll_secs`, minimum 60 s). VALSYNC polls only while VALORANT is running and the screen is open. Match lists are cached for 60 s, matches are stored permanently, and content (agent and map names) is cached for 24 h. A `429` response is respected with `Retry-After`.
* **Not available through any supported API, so always N/A:** current RR, peak RR, equipped Vandal and Phantom skins, and skin variants. "Peak recorded rank" means the highest tier among matches VALSYNC has stored for you.
* **Match History** (menu **[7]** or `valsync history`) lists matches with the result, score, agent, K/D/A, HS% and the rank shown in that match. Press `Enter` on a match for the full 5v5 post-match dashboard.

---

## Configuration

Run `valsync config show`, or open **[9] Application Settings**. Settings are stored in `%APPDATA%\VALSYNC\config.toml`.

| Key | Default | Meaning |
|---|---|---|
| `riot_api_key` | — | Riot API key. The `RIOT_API_KEY` environment variable takes precedence. **VALORANT endpoints require a production key.** |
| `default_shard` | — | `na`, `latam`, `br`, `eu`, `ap` or `kr`. Used when the shard is unknown. |
| `rso_client_id` / `rso_client_secret` | — | RSO credentials of an approved product. The `VALSYNC_RSO_CLIENT_SECRET` environment variable is preferred for the secret. |
| `rso_redirect_port` | 51734 | Local redirect port |
| `write_cloud_cache` | false | Best-effort write of account-synced values into the local cache |
| `auto_backup_retention` | 0 | Maximum automatic pre-change backups per account (0 = unlimited) |
| `live_poll_secs` | 120 | Live refresh interval (minimum 60) |
| `hide_other_names` | false | Hide other players' Riot IDs in dashboards |
| `manual_active_account` | — | Manual fallback account |
| `color` | true | Coloured output (`NO_COLOR` is also honoured) |
| `locale` | en-US | Locale for agent and map names |

Environment variables for overrides and testing:

| Variable | Purpose |
|---|---|
| `VALSYNC_HOME` | Data folder |
| `VALSYNC_VALORANT_DIR` | The `VALORANT\Saved` folder |
| `VALSYNC_VALORANT_CONFIG` | The `Config` folder |
| `VALSYNC_PRINT_PEAK_MEM=1` | Print peak memory and time on exit |
| `VALSYNC_FAKE_PROCESSES` | Test hook that overrides process detection |

---

## Security and fair-play guarantees

VALSYNC uses only:

* normal reads and writes of VALORANT's user config files
* the process *name* list from the Windows ToolHelp API, to see whether the Riot Client or VALORANT is running
* the game's own log file, for the patch version and your current map
* documented Riot APIs over HTTPS

VALSYNC does **not** use:

* DLL injection, kernel drivers or game memory access
* process hooks or anti-cheat bypasses
* the Riot Client local API or lockfile
* private or undocumented endpoints
* credential or token extraction

VALSYNC does not modify game binaries or protected files, and does not interact with or disable Vanguard. It refuses to write while the game is running.

Data handling:

* Profiles (`.valsync`) contain setting values only. They carry a SHA-256 checksum, and on import VALSYNC re-checks every entry, dropping identifiers, unsafe paths and line-injection attempts.
* Backups are restored only into their own account's folder, and path-traversal protection applies to every stored path.
* The only secrets VALSYNC stores are your *own* developer credentials (API key and RSO secret), and only if you put them in `config.toml`. Use environment variables if you prefer.

---

## Research notes and what needs Riot approval

I researched this on 2026-11-24. The sandbox could not open developer.riotgames.com directly, so the Riot developer-policy points below come from search-result excerpts of Riot's pages, plus the endpoint and DTO definitions in the open-source `riven` crate (v2.81). **Re-check Riot's current policies before distributing VALSYNC.**

* **Config location.** Riot Support documents VALORANT's configuration folder as `%LOCALAPPDATA%\VALORANT\Saved\Config` and tells players to back it up before resetting. Community tooling shows per-account sub-folders named after the account's ID. VALSYNC parses `<uuid>[-<shard>]` folder names defensively.
* **Settings sync.** Riot Support's "Error retrieving settings from server … changes to settings will not be saved on the server" article confirms that settings are stored on Riot's servers. That is the basis of the local/cloud split above.
* **API access.** Riot's developer docs state that VALORANT APIs are available only with **production API keys**, that **personal and development keys do not get responses**, and that **all VALORANT apps must have players opt in via RSO**. VAL-MATCH-V1 is a completed-match API; its recent-matches feed is delayed. There is no VALORANT live-game or spectator endpoint.
* **Endpoints used:**

  | API | Endpoints | Routing |
  |---|---|---|
  | ACCOUNT-V1 | `by-puuid`, `by-riot-id`, `accounts/me` (RSO), `active-shards/by-game/val` | Regional hosts: americas, europe, asia |
  | VAL-MATCH-V1 | `matchlists/by-puuid`, `matches/{id}` | Shard hosts: na, latam, br, eu, ap, kr |
  | VAL-CONTENT-V1 | `contents` | Shard hosts |
  | VAL-STATUS-V1 | `platform-data` | Shard hosts |

**What requires Riot approval:**

| Feature | Requirement |
|---|---|
| Riot ID verification, server status, agent/map names | A Riot API key. VALORANT endpoints answer only production keys. |
| Match history, post-match dashboard, live "own stats" | A production key **and** an RSO client (client ID/secret plus a registered redirect URI), approved by Riot for the product. Without them these screens explain what is missing and show N/A. |
| Distributing VALSYNC with built-in API access | Riot forbids shipping API keys inside client apps. A public release needs its own approved product registration. Each user can also supply credentials via `config.toml` or environment variables. |

**What no supported interface provides** (shown as N/A or handled as a manual checklist):

* writing account-synced settings (sensitivity, crosshair, keybinds, HUD and similar)
* live rosters and opponents
* RR and peak RR
* equipped skins and variants

The settings manager, backup engine, undo/redo, recovery, profile management and account detection need **no** Riot approval and work fully offline.

---

## Benchmarks

These were measured on 2026-11-24 with the **release build of the same source on Linux x86_64** (4 vCPU cloud container). The Windows `.exe` was cross-compiled in that container but could not be run there: Wine crashed during its own setup. To reproduce on your PC, run `valsync bench` (synthetic config in a temporary folder; it never touches your real files). Prefix any command with `VALSYNC_PRINT_PEAK_MEM=1` to see that command's own figures. The `build` GitHub Actions workflow also runs `valsync.exe bench` natively on `windows-latest`.

| Measurement | Result |
|---|---|
| Process start → command dispatch | 6.5 ms |
| `valsync status` (complete run) | ~4 ms, 8.3 MB peak memory |
| `valsync profiles` / `backups` / `doctor` | ~1 ms, ~8.4 MB |
| `valsync undo` / `redo` (complete, including verification) | ~4 ms, 8.4 MB |
| Transfer (snapshot + atomic write + verify + post-snapshot) | 2.8 ms |
| Undo (integrity check + restore + verify) | 3.4 ms |
| Capture profile / plan transfer | 0.11 ms / 0.04 ms |
| Interactive menu idle CPU (33 s sample) | **0.00 % of one core**, 8.6 MB RSS |
| Background services / processes | none |
| Binary size | 5.3 MB (`valsync.exe`), 5.6 MB (Linux) |

What these figures mean in practice:

* Startup is far below the one-second target.
* Basic commands stay below the 20 MB memory target.
* VALSYNC never runs while you play unless you open it, and even then the idle menu only wakes about once per second to check status cheaply.

---

## Building and testing

You need **Rust 1.88 or newer** (edition 2024).

```powershell
# Native Windows (MSVC toolchain)
cargo build --release              # → target\release\valsync.exe
cargo test                         # full test-suite
```

Cross-compile from Linux:

```bash
rustup target add x86_64-pc-windows-gnu
sudo apt install gcc-mingw-w64-x86-64
cargo build --release --target x86_64-pc-windows-gnu
# → target/x86_64-pc-windows-gnu/release/valsync.exe  (statically linked, no MinGW DLLs)
```

The release profile uses `opt-level=3`, fat LTO, one codegen unit, `panic=abort` and stripped symbols.

### Test-suite

There are 48 tests: unit tests plus `tests/*.rs`. They all run against synthetic VALORANT folders in temporary directories.

| Area | Tests |
|---|---|
| Account switching | Detection from `LastKnownUser`, switch events, manual fallback validation, guessed-account confirmation |
| Cross-account transfers | Save on A, load on B, local values applied, machine and identity keys untouched, cloud values on the checklist, profile reuse on a third account, selective categories, the 0.25 → 0.176 → 0.25 sensitivity example |
| Corrupted backups | Tampered blob, missing file row, refusal to restore, corrupted flag |
| Interrupted writes | Injected I/O failure mid-transaction with full rollback, corrupted write caught by verification, simulated crash with a torn file and stray temp file recovered on next start, recovery deferred while the game runs |
| Restoration | Byte-identical undo, multi-step undo/redo stack, redo invalidation, restore original, cross-account restore refused, read-only files |
| Profiles | Export/import round trip, checksum tamper detection, hostile-profile sanitisation (path traversal, tokens, UUIDs, newline injection), compare, rename/favourite/preferred, patch warnings, retention |
| Riot API | Local mock server: RSO consent required, match sync and caching, 5v5 view building, HS%, peak/current rank, win rate, name hiding, Riot ID verification, production-key error message |
| CLI | Every command run end to end through the real binary, including confirmation refusal without `--yes` and refusal while VALORANT runs |
| TUI | Headless rendering of every screen at several terminal sizes; a full load → undo through the interface |

---

## Architecture

```
src/
  main.rs        entry point: CLI dispatch or TUI
  cli.rs         clap command definitions
  commands.rs    direct command implementations
  tui/           Ratatui interface (mod.rs = state machine & jobs, ui.rs = rendering)
  engine.rs      capture, plan/apply transfers, backups, restore, undo/redo, recovery
  txn.rs         atomic file writes, fault injection hooks
  db.rs          SQLite schema & queries (WAL, synchronous=FULL)
  ini.rs         lossless Unreal-Engine INI parser/writer (CRLF/LF, BOM, repeated keys)
  catalog.rs     key → category / scope / exclusion classification
  profile.rs     profile model, .valsync format, sanitisation, compare
  accounts.rs    account folder / LastKnownUser detection, switch tracking
  game.rs        process-name detection, patch & map from the game log
  riot/          ACCOUNT-V1, VAL-MATCH/CONTENT/STATUS-V1 client with caching; RSO flow
  matches.rs     5v5 dashboards, owner stats, privacy rules
  rank.rs        competitive tiers & colours
  service.rs     shared high-level operations (status, identity, history)
  bench.rs       `valsync bench`
  sample.rs      synthetic config generator for tests/benchmarks
```

Tokio runs network work only: Riot API calls and the RSO redirect listener. It uses a current-thread runtime on short-lived worker threads, so the interface never blocks and no threads stay alive in the background.
