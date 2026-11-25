# VALSYNC (beta)

Copy your VALORANT settings from one account to another — **sensitivity, crosshair, keybinds, video settings and everything else** — in a few seconds. Every change is backed up first, so you can always undo it.

VALSYNC also shows your live match (agents, levels, skins) and your match history with rank, RR and peak.

## Download

Get **`valsync.exe`** from the [latest release](../../releases). That's the whole program: no installer, no setup and no admin rights.

Windows may say "Windows protected your PC" because the file isn't signed. Click **More info → Run anyway**.

## How to copy settings to another account

1. Open the **Riot Client** and sign in to the account that has the settings you like.
2. Open `valsync.exe` and press **1** (Save Current Settings). Give it a name, like `Main`.
3. In the Riot Client, **sign out and sign in to your other account**.
4. In VALSYNC press **2** (Load Saved Settings), pick `Main`, check the preview, and press **Enter**.
5. Start VALORANT. Your settings are there.

Don't like it? Press **3** (Restore Previous Settings) and choose **Undo**. The account goes back exactly the way it was.

> Keep VALORANT **closed** while loading or restoring settings. The Riot Client can stay open.

## What gets copied

| Setting | Copied? |
|---|---|
| Sensitivity, scoped/ADS sensitivity | ✅ |
| Crosshair (all profiles) | ✅ |
| Keybinds, including agent keybinds | ✅ |
| Minimap, HUD, enemy highlight color | ✅ |
| Audio volumes, accessibility, general options | ✅ |
| Resolution, graphics, display mode, FPS limit, NVIDIA Reflex | ✅ (the other account must have started VALORANT on this PC at least once) |
| Your audio devices and monitor | ❌ The other account keeps its own, on purpose |
| Skins, name, rank, anything tied to your account | ❌ Never touched |

You don't have to copy everything. On the preview screen, press **c** to choose categories, for example only your crosshair.

## The menu

```
[1] Save Current Settings       [6] Live Match
[2] Load Saved Settings         [7] Match History
[3] Restore Previous Settings   [8] Backup History
[4] Manage Settings Profiles    [9] Application Settings
[5] Manage Riot Accounts        [0] Exit
```

Use the arrow keys and **Enter**, or press the number. **Esc** goes back.

- **Restore Previous Settings:** undo or redo your last change, or put the account back to how it was before VALSYNC ever touched it.
- **Manage Settings Profiles:** rename, copy, delete, favourite (★) or compare profiles. You can also export a profile to a `.valsync` file to share it and import one from a friend.
- **Live Match:** shows everyone's agent and level, plus their Vandal and Phantom skins, in agent select and in game. Your own row adds your rank, RR, peak, K/D, HS% and win rate.
- **Match History:** your recent matches with K/D/A and HS%, plus your rank, RR and peak.
- **Backup History:** every backup VALSYNC made. Restore, check or delete them, or make one yourself.

Account switching happens in the Riot Client as usual, and VALSYNC follows automatically.

## Commands (optional)

If you prefer typing, every feature also works as a command:

```
valsync                     open the menu
valsync save "Main"         save your current settings
valsync load "Main"         load them onto the account you're signed in to
valsync undo                undo the last load
valsync redo                redo it
valsync restore             restore your previous settings
valsync restore --original  restore the settings from before VALSYNC
valsync profiles            list profiles
valsync compare "Main" "Old"
valsync export "Main"       → main.valsync
valsync import main.valsync
valsync backups             list backups
valsync live                live match screen
valsync history             your recent matches, rank, RR, peak
valsync status              who's signed in, game status
valsync doctor              check that everything works
valsync help
```

Add `--categories crosshair,sensitivity` to `save` or `load` to copy only some categories. Add `--dry-run` to `load` to preview without changing anything, and `-y` to skip confirmation prompts.

Category names: `sensitivity, scoped, crosshair, keybinds, agent-keybinds, resolution, graphics, display, fps, reflex, audio, minimap, hud, enemy-highlight, accessibility, gameplay`.

## Is it safe?

- **Your password is never asked for or seen.** VALSYNC uses the session of the Riot Client you're already signed into. This is the same method other popular settings and tracker tools use. Tokens stay in memory and only go to Riot's own servers.
- **No game hacking.** VALSYNC does no injection, reads no game memory, never touches Vanguard and modifies no game files. It changes your settings the same way the in-game settings menu saves them.
- **Fair play.**
  - Live Match only shows what the game shows you.
  - It never looks up other players' ranks.
  - Players who hid their name stay hidden, and so do hidden levels.
  - Turn on `hide_other_names` in Application Settings for streamer mode.
- **Everything is backed up first.**
  - If anything fails halfway, VALSYNC puts everything back automatically.
  - If the PC crashes mid-change, VALSYNC undoes the change the next time it starts.
- **It never writes to the wrong account.**
  - Account settings are only changed for the account signed into the Riot Client.
  - A backup can only be restored onto the account it came from.

This uses Riot's game services the way the game client does. Those services aren't an official public API, so Riot could change them. If something stops working after a patch, please open an issue.

## Beta notes

This is a beta. It's built and tested automatically on Windows, but it hasn't been tried on many real PCs yet. If something doesn't work, run `valsync doctor` and include the output in an issue.

Known limits:

- Video settings only copy to an account that has started VALORANT on this PC at least once, because the game creates that account's settings files on its first launch.
- Peak RR isn't available from Riot, so it shows N/A.

## For developers

```
cargo build --release      # target/release/valsync.exe
cargo test                 # 62 tests: transfers, undo/redo, crash recovery, a mock Riot Client and Riot services, CLI, TUI
valsync bench              # timings and memory use on this PC
```

It's written in Rust and uses:

| Library | Used for |
|---|---|
| Ratatui + Crossterm | The menu interface |
| Clap | Commands |
| SQLite | Profiles and backups, stored in `%APPDATA%\VALSYNC` |
| Tokio + reqwest | Talking to Riot |

How VALSYNC talks to Riot:

| What | Where |
|---|---|
| Local session | Riot Client `lockfile` → `/entitlements/v1/token`, `/chat/v1/session`, `/product-session/v1/external-sessions` |
| Account settings | `player-preferences-*.pp.sgp.pvp.net/playerPref/v3/{getPreference,savePreference}` (`Ares.PlayerSettings`, base64 + raw deflate JSON) |
| Video settings | `%LOCALAPPDATA%\VALORANT\Saved\Config\<PUUID>-<region>\Windows\GameUserSettings.ini` |
| Matches, rank, level, names | `pd.<shard>.a.pvp.net` |
| Live match and loadouts | `glz-<region>-1.<shard>.a.pvp.net` |
| Agent, map and skin names | valorant-api.com |

Startup takes a few milliseconds, the menu uses no CPU while idle, and peak memory is about 10 MB.
