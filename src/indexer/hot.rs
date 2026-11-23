//! Files that were changed a moment ago ("hot" files).
//!
//! Why this exists: the directory listing that a scan reads (`read_dir`) carries a *copy* of each
//! file's size, and NTFS refreshes that copy lazily - often only when the writer closes the file.
//! For a file that is being written right now (a download, a log, a VM disk, a build output) the
//! listing is stale, and so are the folder totals that were summed from it.
//!
//! So a hot file is handled differently:
//!  * the scan asks the file itself (a handle-based stat, always exact) instead of trusting the listing;
//!  * it is polled every second or so (backing off to 8 s while it stays unchanged) and dropped
//!    45 s after its last change, so a writer that does not produce events is still noticed.
//!
//! The table is small (`MAX_HOT` entries) and nothing is ever polled that has not just changed.

use super::model::{MutexExt, RwLockExt};
use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex, RwLock,
    },
    time::{Duration, Instant},
};

/// The names of the hot files, grouped by the folder they live in.
pub type HotDirs = HashMap<PathBuf, Vec<OsString>>;

/// Most files tracked at once; the least recently changed one is dropped for a new one.
const MAX_HOT: usize = 256;
/// A file stays hot this long after its last observed change.
const LIFETIME: Duration = Duration::from_secs(45);
/// Polling interval right after a change, and the slowest interval while nothing changes.
const POLL_MIN: Duration = Duration::from_secs(1);
const POLL_MAX: Duration = Duration::from_secs(8);

struct Hot {
    size: u64,
    last_change: Instant,
    next_poll: Instant,
    every: Duration,
}

/// File names compare case-insensitively on Windows.
#[cfg(windows)]
pub fn same_name(a: &OsStr, b: &OsStr) -> bool {
    a.eq_ignore_ascii_case(b)
}
#[cfg(not(windows))]
pub fn same_name(a: &OsStr, b: &OsStr) -> bool {
    a == b
}

#[derive(Default)]
pub struct HotFiles {
    /// One file that is ALWAYS read exactly, never from the listing: the index file. The app keeps
    /// it open for its whole life, so NTFS never refreshes its listed size while we run.
    pinned: Mutex<Option<(PathBuf, OsString)>>,
    map: Mutex<HashMap<PathBuf, Hot>>,
    /// Mirror of `map.len()`, so scan threads can skip the lock while nothing is hot.
    len: AtomicUsize,
    /// The set of hot files changed since `by_dir` was built.
    stale: AtomicBool,
    /// The hot files' names grouped by folder. A scan takes one `snapshot` of this when it starts
    /// and then reads it without any lock. It is rebuilt (once) by the first reader after the set
    /// of hot files changed, never by the writers, so a burst of events costs one rebuild, not one each.
    by_dir: RwLock<Arc<HotDirs>>,
}

impl HotFiles {
    /// Called with `map` locked after every change of the *set* of hot files (not of a size).
    fn mark(&self, m: &HashMap<PathBuf, Hot>) {
        self.len.store(m.len(), Ordering::Relaxed);
        self.stale.store(true, Ordering::Release);
    }

    /// Marks `file` as always-exact (replaces an earlier pin).
    pub fn pin(&self, file: &Path) {
        let v = match (file.parent(), file.file_name()) {
            (Some(d), Some(n)) => Some((d.to_path_buf(), n.to_os_string())),
            _ => None,
        };
        *self.pinned.lk() = v;
    }

    /// The pinned file as (folder, name). A scan copies this once at its start.
    pub fn pinned(&self) -> Option<(PathBuf, OsString)> {
        self.pinned.lk().clone()
    }

    fn is_active(&self) -> bool {
        self.len.load(Ordering::Relaxed) > 0
    }

    /// The hot files' names by folder, as they are now (empty almost always). A scan takes this once
    /// at its start, so the per-folder work of the scan never touches a lock; a file that turns hot
    /// during a scan is handled by the live updater's rescan instead.
    pub fn snapshot(&self) -> Arc<HotDirs> {
        if !self.is_active() {
            return Arc::default();
        }
        if self.stale.swap(false, Ordering::Acquire) {
            let mut by: HotDirs = HashMap::new();
            for p in self.map.lk().keys() {
                if let (Some(d), Some(n)) = (p.parent(), p.file_name()) {
                    by.entry(d.to_path_buf()).or_default().push(n.to_os_string());
                }
            }
            *self.by_dir.wr() = Arc::new(by);
        }
        Arc::clone(&self.by_dir.rd())
    }

    /// Records that `p` changed and now has `size` bytes. True if this is news (a new hot file or
    /// a different size), false if the size is what we already knew.
    pub fn touch(&self, p: &Path, size: u64, now: Instant) -> bool {
        let mut m = self.map.lk();
        let news = match m.get_mut(p) {
            Some(h) => {
                let changed = h.size != size;
                h.size = size;
                h.last_change = now;
                h.every = POLL_MIN;
                h.next_poll = now + POLL_MIN;
                changed
            }
            None => {
                if m.len() >= MAX_HOT {
                    let oldest = m.iter().min_by_key(|(_, h)| h.last_change).map(|(k, _)| k.clone());
                    if let Some(k) = oldest {
                        m.remove(&k);
                    }
                }
                m.insert(
                    p.to_path_buf(),
                    Hot { size, last_change: now, next_poll: now + POLL_MIN, every: POLL_MIN },
                );
                self.mark(&m);
                true
            }
        };
        news
    }

    pub fn remove(&self, p: &Path) {
        let mut m = self.map.lk();
        if m.remove(p).is_some() {
            self.mark(&m);
        }
    }

    /// Files whose poll time has come. Also forgets files that have been quiet for `LIFETIME`.
    pub fn due(&self, now: Instant) -> Vec<PathBuf> {
        let mut m = self.map.lk();
        let before = m.len();
        m.retain(|_, h| now.saturating_duration_since(h.last_change) < LIFETIME);
        if m.len() != before {
            self.mark(&m);
        }
        m.iter_mut()
            .filter(|(_, h)| h.next_poll <= now)
            .map(|(p, h)| {
                h.next_poll = now + h.every;
                p.clone()
            })
            .collect()
    }

    /// Result of a poll: `size` is None if the file is gone or is no longer a file.
    /// True if it changed (or vanished), i.e. the folder totals above it are out of date.
    pub fn record(&self, p: &Path, size: Option<u64>, now: Instant) -> bool {
        let mut m = self.map.lk();
        let Some(size) = size else {
            if m.remove(p).is_some() {
                self.mark(&m);
            }
            return true;
        };
        let Some(h) = m.get_mut(p) else { return false };
        if h.size != size {
            h.size = size;
            h.last_change = now;
            h.every = POLL_MIN;
            h.next_poll = now + POLL_MIN;
            true
        } else {
            h.every = (h.every * 2).min(POLL_MAX);
            h.next_poll = now + h.every;
            false
        }
    }

    /// When the next poll is due.
    pub fn next_due(&self) -> Option<Instant> {
        self.map.lk().values().map(|h| h.next_poll).min()
    }
}
