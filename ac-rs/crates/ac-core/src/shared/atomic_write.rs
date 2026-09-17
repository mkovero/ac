//! All-or-nothing file replacement, shared by every store under
//! `~/.config/ac/` (`config.json`, `cal.json`).
//!
//! [`write_atomic`] writes a sibling temporary, flushes it to disk, and
//! renames it over the target. `rename(2)` within a directory is atomic, so a
//! reader sees either the whole previous file or the whole new one — never a
//! truncated one — and a failure before the rename leaves the previous file
//! exactly as it was.
//!
//! What this does not cover: on a filesystem that rejects `fsync` on a
//! directory, a power loss right after the rename can still roll the
//! directory entry back to the previous file. That file is whole, never
//! torn.

use std::path::Path;

use anyhow::{Context, Result};

/// Replace `path` with `bytes` atomically. The parent directory must already
/// exist. On any error before the rename the target is untouched and the
/// temporary is removed.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;

    let file_name = path
        .file_name()
        .with_context(|| format!("{} has no file name", path.display()))?;
    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(format!(".tmp.{}", std::process::id()));
    // Same directory as the target: `rename` across filesystems is not
    // atomic (and on Linux fails outright), so a temp dir would not do.
    let tmp = path.with_file_name(tmp_name);

    let written = (|| -> Result<()> {
        let mut f =
            std::fs::File::create(&tmp).with_context(|| format!("writing {}", tmp.display()))?;
        f.write_all(bytes)
            .with_context(|| format!("writing {}", tmp.display()))?;
        f.sync_all()
            .with_context(|| format!("syncing {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()))
    })();
    if let Err(e) = written {
        // Leaving the temp behind would accumulate one file per failed
        // write next to the real store.
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }

    // Best effort, error ignored on purpose: once the rename has succeeded
    // the new file is what every reader sees, so reporting the write as
    // failed here would be false. Some filesystems also reject fsync on a
    // directory outright.
    if let Some(dir) = path.parent() {
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leftover_temps(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp."))
            .collect()
    }

    #[test]
    fn failed_replacement_leaves_the_target_intact_and_no_temp() {
        // The target is a non-empty directory, so the temp write succeeds
        // and the rename over it fails — the replacement step itself.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.json");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("keep"), b"inside").unwrap();

        let err = write_atomic(&target, b"{}").expect_err("rename must fail");
        assert!(format!("{err:#}").contains("renaming"), "{err:#}");
        assert!(target.is_dir());
        assert_eq!(std::fs::read(target.join("keep")).unwrap(), b"inside");
        assert!(
            leftover_temps(dir.path()).is_empty(),
            "temp left behind: {:?}",
            leftover_temps(dir.path())
        );
    }

    #[test]
    fn a_second_write_replaces_the_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("cal.json");
        write_atomic(&target, b"a much longer first payload").unwrap();
        write_atomic(&target, b"short").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"short");
        assert!(leftover_temps(dir.path()).is_empty());
    }
}
