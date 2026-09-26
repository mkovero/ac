//! The saved-captures list (#256, `F` in the transfer view).
//!
//! Pure state, so what each key does is a unit test — the same split
//! `settings.rs` and `delay_entry.rs` use. The app routes `↑`/`↓` and the
//! slot digits here while the list is open; a digit loads the selected file
//! into that slot.

use std::path::{Path, PathBuf};

/// Longest list shown: the newest captures. Older files stay on disk.
pub const MAX_SHOWN: usize = 15;

pub struct FileList {
    entries: Vec<PathBuf>,
    selected: usize,
}

impl FileList {
    /// The `.acsnap` files in `dir`, newest first (by modification time),
    /// at most [`MAX_SHOWN`]. A missing directory is an empty list.
    pub fn read(dir: &Path) -> FileList {
        let mut files: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "acsnap"))
            .map(|p| {
                let t = std::fs::metadata(&p)
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                (t, p)
            })
            .collect();
        files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
        files.truncate(MAX_SHOWN);
        FileList {
            entries: files.into_iter().map(|(_, p)| p).collect(),
            selected: 0,
        }
    }

    pub fn entries(&self) -> &[PathBuf] {
        &self.entries
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn selected_path(&self) -> Option<&Path> {
        self.entries.get(self.selected).map(PathBuf::as_path)
    }

    /// `↑` / `↓`: move the selection, stopping at either end.
    pub fn move_selection(&mut self, down: bool) {
        if down {
            if self.selected + 1 < self.entries.len() {
                self.selected += 1;
            }
        } else {
            self.selected = self.selected.saturating_sub(1);
        }
    }

    /// A file's name for the list, without the directory.
    pub fn name(path: &Path) -> String {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_with(names: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ac-file-list-{}-{}",
            std::process::id(),
            names.len()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (i, n) in names.iter().enumerate() {
            let p = dir.join(n);
            std::fs::write(&p, b"x").unwrap();
            let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000 + i as u64);
            std::fs::File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_modified(t)
                .unwrap();
        }
        dir
    }

    #[test]
    fn lists_acsnap_files_newest_first_and_nothing_else() {
        let dir = dir_with(&["old.acsnap", "note.txt", "new.acsnap"]);
        let list = FileList::read(&dir);
        let names: Vec<String> = list.entries().iter().map(|p| FileList::name(p)).collect();
        assert_eq!(names, ["new.acsnap", "old.acsnap"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn selection_stops_at_both_ends() {
        let dir = dir_with(&["a.acsnap", "b.acsnap"]);
        let mut list = FileList::read(&dir);
        list.move_selection(false);
        assert_eq!(list.selected(), 0);
        list.move_selection(true);
        list.move_selection(true);
        assert_eq!(list.selected(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_directory_is_an_empty_list() {
        let list = FileList::read(Path::new("/nonexistent/ac-captures"));
        assert!(list.entries().is_empty() && list.selected_path().is_none());
    }
}
