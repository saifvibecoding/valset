//! Command-line interface definition.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "valsync",
    version,
    about = "VALSYNC — save, transfer and restore VALORANT settings across Riot accounts",
    long_about = "VALSYNC — VALORANT terminal settings manager.\n\nRun without arguments to open the interactive menu.",
    after_help = "Examples:\n  valsync save \"Main\"\n  valsync load \"Main\" --categories graphics,resolution,fps\n  valsync undo\n  valsync compare \"Main\" \"Old\"\n  valsync doctor"
)]
pub struct Cli {
    /// Answer "yes" to confirmation prompts (non-interactive use)
    #[arg(short = 'y', long, global = true)]
    pub yes: bool,
    /// Disable colored output
    #[arg(long, global = true)]
    pub no_color: bool,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show account, Riot Client / VALORANT status, patch and backup summary
    Status,
    /// List and manage remembered Riot accounts
    Accounts {
        #[command(subcommand)]
        action: Option<AccountAction>,
    },
    /// Save the active account's current settings as a profile
    Save {
        /// Profile name
        name: String,
        /// Replace an existing profile with the same name
        #[arg(long)]
        overwrite: bool,
        /// Only save these categories (comma separated, e.g. graphics,fps) or "all"
        #[arg(long, default_value = "all")]
        categories: String,
        /// Free-form note stored with the profile
        #[arg(long)]
        note: Option<String>,
        /// Record a crosshair code to paste in-game (cloud setting)
        #[arg(long)]
        crosshair_code: Option<String>,
        /// Record your sensitivity for the manual checklist (cloud setting)
        #[arg(long)]
        sens: Option<String>,
        /// Save from this account instead of the active one (PUUID, folder or Name#TAG)
        #[arg(long)]
        account: Option<String>,
    },
    /// Apply a saved profile to the active account (automatic backup first)
    Load {
        /// Profile name
        name: String,
        /// Categories to transfer (comma separated) or "all"
        #[arg(long, default_value = "all")]
        categories: String,
        /// Show the changes without applying them
        #[arg(long)]
        dry_run: bool,
        /// Target account (PUUID, folder or Name#TAG) instead of the detected one
        #[arg(long)]
        account: Option<String>,
    },
    /// Restore previous settings (default: undo the last transfer)
    Restore {
        /// Restore this backup id (see `valsync backups`)
        #[arg(long)]
        backup: Option<i64>,
        /// Restore the original settings VALSYNC first recorded for the account
        #[arg(long)]
        original: bool,
        /// Account the backup belongs to, if it is not the active account
        #[arg(long)]
        account: Option<String>,
        /// Show the changes without applying them
        #[arg(long)]
        dry_run: bool,
    },
    /// Undo the last transfer/restore for the active account
    Undo {
        #[arg(long)]
        account: Option<String>,
    },
    /// Redo the last undone transfer for the active account
    Redo {
        #[arg(long)]
        account: Option<String>,
    },
    /// List and manage settings profiles
    Profiles {
        #[command(subcommand)]
        action: Option<ProfileAction>,
    },
    /// List and manage backups
    Backups {
        /// Show backups of every account
        #[arg(long)]
        all: bool,
        #[command(subcommand)]
        action: Option<BackupAction>,
    },
    /// Compare two profiles
    Compare { a: String, b: String },
    /// Export a profile to a portable .valsync file
    Export {
        name: String,
        /// Output file (default: ./<name>.valsync)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Import a .valsync profile file
    Import {
        file: PathBuf,
        /// Store under a different name
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        overwrite: bool,
    },
    /// Live match dashboard (refreshes from supported APIs only)
    Live {
        /// Render once and exit (no refresh loop)
        #[arg(long)]
        once: bool,
    },
    /// Match history for an RSO-linked account
    History {
        #[arg(short = 'n', long, default_value_t = 10)]
        count: usize,
        #[arg(long)]
        account: Option<String>,
        /// Use stored matches only (no network)
        #[arg(long)]
        offline: bool,
    },
    /// Diagnose paths, permissions, database, backups and API access
    Doctor,
    /// View or change application settings
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
    },
    /// Measure startup / operation timings and memory use
    Bench,
}

#[derive(Debug, Subcommand)]
pub enum AccountAction {
    /// List accounts (default)
    List,
    /// Set the Riot ID (Name#TAG) of a local account; verified via ACCOUNT-V1 when an API key is configured
    Name { riot_id: String, #[arg(long)] account: Option<String> },
    /// Manually select the active account (fallback when detection fails)
    Select { account: String },
    /// Clear the manual selection and use automatic detection
    Auto,
    /// Opt in with Riot Sign On (requires an approved RSO client)
    Link {
        #[arg(long)]
        account: Option<String>,
        /// Print the URL instead of opening a browser
        #[arg(long)]
        no_browser: bool,
    },
    /// Withdraw RSO consent and delete stored match data for the account
    Unlink { account: Option<String> },
    /// Set the preferred profile for an account
    Prefer { profile: String, #[arg(long)] account: Option<String> },
    /// Forget an account (does not touch its game files)
    Forget { account: String },
    /// Show detected account switches
    Switches,
}

#[derive(Debug, Subcommand)]
pub enum ProfileAction {
    /// List profiles (default)
    List,
    /// Show the settings in a profile
    Show { name: String },
    Rename { old: String, new: String },
    Duplicate { name: String, new: String },
    Delete { name: String },
    /// Toggle favorite
    Favorite { name: String },
    /// Check profile against this PC's patch and config files
    Check { name: String },
    /// Add a manual (cloud) value to a profile, e.g. a crosshair code
    Note { name: String, label: String, value: String },
}

#[derive(Debug, Subcommand)]
pub enum BackupAction {
    /// List backups (default)
    List,
    /// Create a manual backup of the active account
    Create { label: Option<String> },
    /// Verify integrity of one backup or all backups
    Verify { id: Option<i64> },
    /// Show what restoring a backup would change
    Show { id: i64 },
    /// Delete a backup
    Delete { id: i64 },
}

#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Show all settings (default)
    Show,
    Get { key: String },
    Set { key: String, value: String },
    /// Print file locations
    Path,
}
