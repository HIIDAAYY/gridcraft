//! Working copies: how a workbook opened from Android's storage keeps a file of its own.
//!
//! Android hands the app a file's *contents* and a `content://` address it may write to, never a
//! path. GridCraft's Save, AutoSave and `CELL("filename")` all work with paths, so a workbook
//! opened from storage is written to a private working copy (`files/docs/<n>/<its name>`) and
//! opened from there: it has a path, the title shows its name, and Save writes the copy as on the
//! desktop. After each save the shell hands the copy to the activity, which writes it over the
//! original (unless the original changed elsewhere since: a conflict, which is never overwritten).
//!
//! The index (`workcopies.json` in the profile) survives restarts, so a copy whose write-back
//! didn't finish (a conflict, a failure, the app closed first) is kept until it's dealt with; the
//! others are cleared away on the next start.
//!
//! Pure file handling with no Android dependency, so it is tested on the desktop.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::mailbox::{Sync, safe_name};

/// Where a working copy stands with its original.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    /// The original has what the copy had when last handed over (or when opened).
    InSync,
    /// Handed to the activity; its answer hasn't come yet.
    Writing,
    /// The original changed elsewhere: never written again (Save As keeps the changes).
    Conflict,
    /// The last write-back failed; the next save tries again.
    Failed,
}

/// One working copy.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// The original's address.
    pub uri: String,
    /// The copy's size and modification time when last handed over (or opened).
    pub stamp: Option<(u64, u64)>,
    pub state: State,
}

/// What [`WorkCopies::answered`] tells the user, if anything.
#[derive(Debug, PartialEq)]
pub enum Notice {
    /// Nothing to say: written.
    None,
    /// The original changed elsewhere and was left alone.
    Conflict(String),
    /// Writing failed (the reason).
    Failed(String, String),
}

#[derive(Default, Serialize, Deserialize)]
struct Index {
    copies: BTreeMap<String, Entry>,
}

pub struct WorkCopies {
    /// `files/docs`: one numbered folder per copy.
    root: PathBuf,
    /// The index file.
    index_path: PathBuf,
    index: Index,
}

/// A file's size and modification time (nanoseconds since 1970), the way a save is noticed.
fn stamp(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some((meta.len(), u64::try_from(modified.as_nanos()).unwrap_or(u64::MAX)))
}

impl WorkCopies {
    /// The copies under `root`, with the index at `index_path` (missing or unreadable: none).
    pub fn load(root: PathBuf, index_path: PathBuf) -> Self {
        let index = std::fs::read(&index_path)
            .ok()
            .filter(|b| b.len() < (16 << 20))
            .and_then(|b| serde_json::from_slice::<Index>(&b).ok())
            .unwrap_or_default();
        Self { root, index_path, index }
    }

    /// At start, before anything is open: delete the copies whose originals are up to date, and
    /// folders the index doesn't know. Copies with changes their originals lack are kept and
    /// returned, so the app can open them again; one whose write-back never answered (the app
    /// was closed meanwhile) counts as failed, and the next save tries again.
    pub fn tidy(&mut self) -> Vec<(String, State)> {
        self.index.copies.retain(|path, e| {
            let keep = matches!(e.state, State::Conflict | State::Failed | State::Writing) && Path::new(path).is_file();
            if !keep {
                log::info!("working copies: clearing {path}");
            }
            keep
        });
        for e in self.index.copies.values_mut() {
            if e.state == State::Writing {
                e.state = State::Failed;
            }
        }
        let kept: Vec<PathBuf> = self.index.copies.keys().filter_map(|p| Path::new(p).parent().map(Path::to_path_buf)).collect();
        if let Ok(entries) = std::fs::read_dir(&self.root) {
            for dir in entries.flatten().map(|e| e.path()) {
                if !kept.contains(&dir)
                    && let Err(e) = std::fs::remove_dir_all(&dir)
                {
                    log::warn!("working copies: cannot remove {}: {e}", dir.display());
                }
            }
        }
        for (path, e) in &self.index.copies {
            log::warn!("working copies: {path} still has changes its original ({}) lacks: {:?}", e.uri, e.state);
        }
        self.store();
        self.index.copies.iter().map(|(p, e)| (p.clone(), e.state)).collect()
    }

    /// Drop the copy at `path` and its folder: its workbook was saved elsewhere (Save As), or
    /// closed after the user was told its original couldn't take the changes.
    pub fn forget(&mut self, path: &str) {
        if self.index.copies.remove(path).is_none() {
            return;
        }
        if let Some(dir) = Path::new(path).parent().filter(|d| d.starts_with(&self.root) && *d != self.root)
            && let Err(e) = std::fs::remove_dir_all(dir)
        {
            log::warn!("working copies: cannot remove {}: {e}", dir.display());
        }
        self.store();
    }

    /// Write `bytes` as a new working copy named `name` and link it to the original at `uri`.
    /// Returns the copy's path.
    pub fn create(&mut self, name: &str, bytes: &[u8], uri: &str) -> std::io::Result<PathBuf> {
        let path = self.new_path(name)?;
        std::fs::write(&path, bytes)?;
        self.link(&path, uri);
        Ok(path)
    }

    /// A path for a new copy named `name`, in a folder of its own (created).
    pub fn new_path(&self, name: &str) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(&self.root)?;
        // Android only: the web app's no-clock rule (clippy.toml) doesn't apply.
        #[allow(clippy::disallowed_methods)]
        let start = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        for n in 0..1000u128 {
            let dir = self.root.join(format!("{}", start.saturating_add(n)));
            match std::fs::create_dir(&dir) {
                Ok(()) => return Ok(dir.join(safe_name(name))),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(std::io::Error::other("no free folder for a working copy"))
    }

    /// The copy at `path` belongs to the original at `uri`, which has its current content.
    pub fn link(&mut self, path: &Path, uri: &str) {
        let entry = Entry { uri: uri.to_string(), stamp: stamp(path), state: State::InSync };
        self.index.copies.insert(path.to_string_lossy().into_owned(), entry);
        self.store();
    }

    /// Whether `path` is a working copy.
    pub fn contains(&self, path: &str) -> bool {
        self.index.copies.contains_key(path)
    }

    /// The copies saved since they were last handed over, among `open` (the open workbooks'
    /// paths): each is marked as being written, and returned with its original's address.
    pub fn saved(&mut self, open: &[String]) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for path in open {
            let Some(e) = self.index.copies.get_mut(path) else { continue };
            if matches!(e.state, State::Writing | State::Conflict) {
                continue;
            }
            let now = stamp(Path::new(path));
            if now.is_some() && now != e.stamp {
                e.stamp = now;
                e.state = State::Writing;
                out.push((path.clone(), e.uri.clone()));
            }
        }
        if !out.is_empty() {
            self.store();
        }
        out
    }

    /// The activity's answer for the copy at `path`.
    pub fn answered(&mut self, path: &str, outcome: Sync) -> Notice {
        let Some(e) = self.index.copies.get_mut(path) else { return Notice::None };
        let name = Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let notice = match outcome {
            Sync::Written => {
                e.state = State::InSync;
                Notice::None
            }
            Sync::Conflict => {
                e.state = State::Conflict;
                Notice::Conflict(name)
            }
            Sync::Failed(reason) => {
                e.state = State::Failed;
                Notice::Failed(name, reason)
            }
        };
        self.store();
        notice
    }

    /// The original's address of the copy at `path`.
    pub fn uri(&self, path: &str) -> Option<&str> {
        self.index.copies.get(path).map(|e| e.uri.as_str())
    }

    /// The state of the copy at `path`.
    pub fn state(&self, path: &str) -> Option<State> {
        self.index.copies.get(path).map(|e| e.state)
    }

    fn store(&self) {
        let Ok(json) = serde_json::to_vec_pretty(&self.index) else { return };
        let tmp = self.index_path.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, json).and_then(|()| std::fs::rename(&tmp, &self.index_path)) {
            log::warn!("working copies: cannot save the index {}: {e}", self.index_path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gridcraft-android-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A different modification time than the file had, as a save a moment later would leave.
    fn touch(path: &Path, bytes: &[u8]) {
        let before = stamp(path);
        for _ in 0..200 {
            std::fs::write(path, bytes).unwrap();
            if stamp(path) != before {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("the file's stamp never changed");
    }

    #[test]
    fn a_save_is_handed_over_once_and_again_after_the_next_save() {
        let dir = temp("save");
        let mut w = WorkCopies::load(dir.join("docs"), dir.join("index.json"));
        let path = w.create("../Budget.xlsx", b"one", "content://a/1").unwrap();
        assert_eq!(path.file_name().unwrap(), "Budget.xlsx");
        let open = vec![path.to_string_lossy().into_owned()];
        assert!(w.saved(&open).is_empty(), "nothing changed since it was opened");
        touch(&path, b"two");
        assert_eq!(w.saved(&open), vec![(open[0].clone(), "content://a/1".to_string())]);
        assert!(w.saved(&open).is_empty(), "handed over once");
        touch(&path, b"three");
        assert!(w.saved(&open).is_empty(), "the next save waits for the answer");
        assert_eq!(w.answered(&open[0], Sync::Written), Notice::None);
        assert_eq!(w.saved(&open).len(), 1, "the save made while the first was being written goes next");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_conflict_is_never_written_and_survives_a_restart() {
        let dir = temp("conflict");
        let mut w = WorkCopies::load(dir.join("docs"), dir.join("index.json"));
        let synced = w.create("Synced.xlsx", b"x", "content://a/1").unwrap();
        let path = w.create("Budget.xlsx", b"one", "content://a/2").unwrap();
        let open = vec![path.to_string_lossy().into_owned()];
        touch(&path, b"two");
        assert_eq!(w.saved(&open).len(), 1);
        assert_eq!(w.answered(&open[0], Sync::Conflict), Notice::Conflict("Budget.xlsx".into()));
        touch(&path, b"three");
        assert!(w.saved(&open).is_empty(), "a conflicted original is left alone");
        std::fs::create_dir_all(dir.join("docs/stray")).unwrap();

        let mut again = WorkCopies::load(dir.join("docs"), dir.join("index.json"));
        assert_eq!(again.tidy(), vec![(open[0].clone(), State::Conflict)], "the kept copy is reported, to open again");
        assert_eq!(again.state(&open[0]), Some(State::Conflict), "the changes are kept");
        assert!(path.is_file());
        assert!(!synced.exists() && again.state(&synced.to_string_lossy()).is_none(), "an up-to-date copy is cleared");
        assert!(!dir.join("docs/stray").exists(), "unknown folders are cleared");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_unanswered_write_is_retried_after_a_restart_and_forgetting_removes_the_copy() {
        let dir = temp("unanswered");
        let mut w = WorkCopies::load(dir.join("docs"), dir.join("index.json"));
        let path = w.create("B.xlsx", b"one", "content://a/1").unwrap();
        let open = vec![path.to_string_lossy().into_owned()];
        touch(&path, b"two");
        assert_eq!(w.saved(&open).len(), 1, "handed over; the app closes before the answer");

        let mut again = WorkCopies::load(dir.join("docs"), dir.join("index.json"));
        assert_eq!(again.tidy(), vec![(open[0].clone(), State::Failed)]);
        touch(&path, b"three");
        assert_eq!(again.saved(&open).len(), 1, "the next save tries again instead of waiting forever");
        again.forget(&open[0]);
        assert!(!path.exists() && again.state(&open[0]).is_none());
        again.forget(&open[0]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failed_write_is_retried_on_the_next_save() {
        let dir = temp("failed");
        let mut w = WorkCopies::load(dir.join("docs"), dir.join("index.json"));
        let path = w.create("B.xlsx", b"one", "content://a/1").unwrap();
        let open = vec![path.to_string_lossy().into_owned()];
        touch(&path, b"two");
        assert_eq!(w.saved(&open).len(), 1);
        assert_eq!(w.answered(&open[0], Sync::Failed("no permission".into())), Notice::Failed("B.xlsx".into(), "no permission".into()));
        assert!(w.saved(&open).is_empty(), "not retried in a loop");
        touch(&path, b"three");
        assert_eq!(w.saved(&open).len(), 1, "the next save tries again");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn copies_of_the_same_name_get_folders_of_their_own_and_unknown_paths_are_ignored() {
        let dir = temp("names");
        let mut w = WorkCopies::load(dir.join("docs"), dir.join("index.json"));
        let a = w.create("B.xlsx", b"a", "content://a/1").unwrap();
        let b = w.create("B.xlsx", b"b", "content://a/2").unwrap();
        assert_ne!(a, b);
        assert_eq!(std::fs::read(&a).unwrap(), b"a");
        assert!(w.saved(&["/elsewhere/C.xlsx".to_string()]).is_empty());
        assert_eq!(w.answered("/elsewhere/C.xlsx", Sync::Written), Notice::None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
