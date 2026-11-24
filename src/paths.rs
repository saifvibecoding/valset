//! Filesystem locations used by VALSYNC.
//!
//! * VALORANT configuration: `%LOCALAPPDATA%\VALORANT\Saved\Config`
//!   (override with `VALSYNC_VALORANT_DIR`, pointing at the `VALORANT\Saved` folder,
//!   or `VALSYNC_VALORANT_CONFIG`, pointing at the `Config` folder directly).
//! * VALSYNC data (SQLite database, config, exports): `%APPDATA%\VALSYNC`
//!   (override with `VALSYNC_HOME`).

use std::env;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Paths {
    /// `...\VALORANT\Saved\Config`
    pub valorant_config: PathBuf,
    /// `...\VALORANT\Saved\Logs`
    pub valorant_logs: PathBuf,
    /// VALSYNC data directory.
    pub data_dir: PathBuf,
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn home_dir() -> PathBuf {
    env_path("USERPROFILE").or_else(|| env_path("HOME")).unwrap_or_else(|| PathBuf::from("."))
}

impl Paths {
    pub fn detect() -> Self {
        let saved = env_path("VALSYNC_VALORANT_DIR").unwrap_or_else(|| {
            let local = env_path("LOCALAPPDATA").unwrap_or_else(|| home_dir().join("AppData").join("Local"));
            local.join("VALORANT").join("Saved")
        });
        let valorant_config = env_path("VALSYNC_VALORANT_CONFIG").unwrap_or_else(|| saved.join("Config"));
        let valorant_logs = valorant_config.parent().map(|p| p.join("Logs")).unwrap_or_else(|| saved.join("Logs"));
        let data_dir = env_path("VALSYNC_HOME").unwrap_or_else(|| {
            let roaming = env_path("APPDATA").unwrap_or_else(|| {
                if cfg!(windows) { home_dir().join("AppData").join("Roaming") } else { home_dir().join(".config") }
            });
            roaming.join("VALSYNC")
        });
        Paths { valorant_config, valorant_logs, data_dir }
    }

    pub fn with_roots(valorant_saved: &Path, data_dir: &Path) -> Self {
        Paths {
            valorant_config: valorant_saved.join("Config"),
            valorant_logs: valorant_saved.join("Logs"),
            data_dir: data_dir.to_path_buf(),
        }
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("valsync.db")
    }

    pub fn config_path(&self) -> PathBuf {
        self.data_dir.join("config.toml")
    }

    pub fn exports_dir(&self) -> PathBuf {
        self.data_dir.join("exports")
    }

    pub fn lock_path(&self) -> PathBuf {
        self.data_dir.join("valsync.lock")
    }

    pub fn account_dir(&self, folder_name: &str) -> PathBuf {
        self.valorant_config.join(folder_name)
    }
}

/// Validate a relative path stored in a backup/profile so it can never escape
/// the account directory (`..`, absolute paths, drive letters, UNC prefixes).
pub fn safe_relative(rel: &str) -> Option<PathBuf> {
    if rel.is_empty() || rel.contains('\0') || rel.contains(':') || rel.starts_with('/') || rel.starts_with('\\') {
        return None;
    }
    let mut out = PathBuf::new();
    for part in rel.split(['/', '\\']) {
        match part {
            "" | "." => continue,
            ".." => return None,
            p => out.push(p),
        }
    }
    if out.as_os_str().is_empty() { None } else { Some(out) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal() {
        assert!(safe_relative("../x.ini").is_none());
        assert!(safe_relative("Windows/../../x.ini").is_none());
        assert!(safe_relative("C:/x.ini").is_none());
        assert!(safe_relative("/etc/passwd").is_none());
        assert!(safe_relative("\\\\server\\share").is_none());
        assert_eq!(safe_relative("Windows/GameUserSettings.ini").unwrap(), PathBuf::from("Windows").join("GameUserSettings.ini"));
    }
}
