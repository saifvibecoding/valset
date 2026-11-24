//! Transactional file operations.
//!
//! Every file VALSYNC changes is written with the classic *write temp → fsync →
//! rename* sequence so a file is either fully old or fully new, never torn. The
//! multi-file transaction (journal, rollback, crash recovery) lives in
//! [`crate::engine`]; this module provides the primitives and a fault-injection
//! hook used by the test-suite to simulate failures and crashes.

use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const TMP_SUFFIX: &str = ".valsync-tmp";

fn tmp_path_for(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "file".into());
    path.with_file_name(format!(".{name}{TMP_SUFFIX}"))
}

fn sync_dir(dir: &Path) {
    // Directory fsync makes the rename durable on POSIX. Not supported on Windows
    // (NTFS rename is journaled by the filesystem), so errors are ignored.
    #[cfg(unix)]
    if let Ok(f) = fs::File::open(dir) {
        let _ = f.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Atomically replace `path` with `bytes`.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    let tmp = tmp_path_for(path);
    {
        let mut f = fs::File::create(&tmp).with_context(|| format!("cannot create {}", tmp.display()))?;
        f.write_all(bytes).with_context(|| format!("cannot write {}", tmp.display()))?;
        f.sync_all().with_context(|| format!("cannot flush {}", tmp.display()))?;
    }
    // A read-only target (some players lock GameUserSettings.ini) would make the
    // rename fail on Windows. Temporarily clear the flag and restore it afterwards.
    let mut was_readonly = false;
    if let Ok(meta) = fs::metadata(path) {
        let mut perms = meta.permissions();
        if perms.readonly() {
            was_readonly = true;
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            let _ = fs::set_permissions(path, perms);
        }
    }
    let res = fs::rename(&tmp, path).with_context(|| format!("cannot replace {}", path.display()));
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    if was_readonly && let Ok(meta) = fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_readonly(true);
        let _ = fs::set_permissions(path, perms);
    }
    res?;
    sync_dir(parent);
    Ok(())
}

/// Remove a file if it exists (durably).
pub fn remove_file(path: &Path) -> Result<()> {
    match fs::metadata(path) {
        Ok(meta) => {
            let mut perms = meta.permissions();
            if perms.readonly() {
                #[allow(clippy::permissions_set_readonly_false)]
                perms.set_readonly(false);
                let _ = fs::set_permissions(path, perms);
            }
            fs::remove_file(path).with_context(|| format!("cannot remove {}", path.display()))?;
            if let Some(p) = path.parent() {
                sync_dir(p);
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("cannot stat {}", path.display())),
    }
}

/// Delete leftover temp files from an interrupted write. Returns how many were removed.
pub fn clean_stray_tmp(dir: &Path) -> usize {
    let mut n = 0;
    let Ok(rd) = fs::read_dir(dir) else { return 0 };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            n += clean_stray_tmp(&p);
        } else if p.file_name().map(|f| f.to_string_lossy().ends_with(TMP_SUFFIX)).unwrap_or(false) && fs::remove_file(&p).is_ok() {
            n += 1;
        }
    }
    n
}

/// One file change inside a transaction. `contents == None` deletes the file.
#[derive(Debug, Clone)]
pub struct FileOp {
    pub rel_path: String,
    pub contents: Option<Vec<u8>>,
}

/// Fault injection for tests (all `None` in production).
#[derive(Debug, Clone, Default)]
pub struct Faults {
    /// Return an I/O error before performing write number `n` (0-based).
    pub fail_before_write: Option<usize>,
    /// Simulate a process crash during write `n`: leaves a partially written
    /// temp file *and* a torn target file, and skips rollback/journal completion.
    pub crash_during_write: Option<usize>,
    /// Write garbage instead of the intended content for write `n` (e.g. an
    /// antivirus or the game rewriting the file) so verification must catch it.
    pub corrupt_write: Option<usize>,
}

#[derive(Debug)]
pub enum ApplyError {
    /// Ordinary failure: `applied` ops were completed and must be rolled back.
    Failed { applied: usize, error: anyhow::Error },
    /// Simulated crash: the caller must behave as if the process died.
    Crashed,
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApplyError::Failed { applied, error } => write!(f, "write failed after {applied} file(s): {error:#}"),
            ApplyError::Crashed => write!(f, "simulated crash"),
        }
    }
}

/// Apply file operations under `root` in order.
pub fn apply_ops(root: &Path, ops: &[FileOp], faults: &Faults) -> std::result::Result<(), ApplyError> {
    for (i, op) in ops.iter().enumerate() {
        let rel = crate::paths::safe_relative(&op.rel_path).ok_or_else(|| ApplyError::Failed {
            applied: i,
            error: anyhow::anyhow!("refusing unsafe path '{}'", op.rel_path),
        })?;
        let path = root.join(rel);
        if faults.fail_before_write == Some(i) {
            return Err(ApplyError::Failed { applied: i, error: anyhow::anyhow!("injected I/O failure on {}", op.rel_path) });
        }
        if faults.crash_during_write == Some(i) {
            if let Some(bytes) = &op.contents {
                let half = &bytes[..bytes.len() / 2];
                let _ = fs::create_dir_all(path.parent().unwrap_or(root));
                let _ = fs::write(tmp_path_for(&path), half);
                let _ = fs::write(&path, half); // torn write, worst case
            }
            return Err(ApplyError::Crashed);
        }
        let res = match &op.contents {
            Some(bytes) if faults.corrupt_write == Some(i) => {
                let mut garbage = bytes.clone();
                garbage.extend_from_slice(b"\r\n#corrupted");
                atomic_write(&path, &garbage)
            }
            Some(bytes) => atomic_write(&path, bytes),
            None => remove_file(&path),
        };
        if let Err(error) = res {
            return Err(ApplyError::Failed { applied: i, error });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_and_cleans() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a").join("x.ini");
        atomic_write(&p, b"one").unwrap();
        atomic_write(&p, b"two").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        assert!(!tmp_path_for(&p).exists());
    }

    #[test]
    fn readonly_target_is_replaced_and_flag_kept() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.ini");
        fs::write(&p, b"old").unwrap();
        let mut perms = fs::metadata(&p).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&p, perms).unwrap();
        atomic_write(&p, b"new").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"new");
        assert!(fs::metadata(&p).unwrap().permissions().readonly());
    }

    #[test]
    fn stray_tmp_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("w")).unwrap();
        fs::write(dir.path().join("w").join(".x.ini.valsync-tmp"), b"half").unwrap();
        assert_eq!(clean_stray_tmp(dir.path()), 1);
    }
}
