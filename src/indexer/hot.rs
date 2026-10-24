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

use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex, MutexGuard,
    },
    time::{Duration, Instant},
};

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
}

impl HotFiles {
    fn map(&self) -> MutexGuard<'_, HashMap<PathBuf, Hot>> {
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Marks `file` as always-exact (replaces an earlier pin).
    pub fn pin(&self, file: &Path) {
        let v = match (file.parent(), file.file_name()) {
            (Some(d), Some(n)) => Some((d.to_path_buf(), n.to_os_string())),
            _ => None,
        };
        *self.pinned.lock().unwrap_or_else(|e| e.into_inner()) = v;
    }

    /// The pinned file's name, if it lives directly in `dir`. Asked once per folder by the scan.
    pub fn pinned_name_in(&self, dir: &Path) -> Option<OsString> {
        let g = self.pinned.lock().unwrap_or_else(|e| e.into_inner());
        let (d, n) = g.as_ref()?;
        same_name(d.as_os_str(), dir.as_os_str()).then(|| n.clone())
    }

    /// Cheap check for the scan loop: is anything hot at all?
    pub fn is_active(&self) -> bool {
        self.len.load(Ordering::Relaxed) > 0
    }

    pub fn contains(&self, p: &Path) -> bool {
        self.is_active() && self.map().contains_key(p)
    }

    /// Records that `p` changed and now has `size` bytes. True if this is news (a new hot file or
    /// a different size), false if the size is what we already knew.
    pub fn touch(&self, p: &Path, size: u64, now: Instant) -> bool {
        let mut m = self.map();
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
                true
            }
        };
        self.len.store(m.len(), Ordering::Relaxed);
        news
    }

    pub fn remove(&self, p: &Path) {
        let mut m = self.map();
        m.remove(p);
        self.len.store(m.len(), Ordering::Relaxed);
    }

    /// Files whose poll time has come. Also forgets files that have been quiet for `LIFETIME`.
    pub fn due(&self, now: Instant) -> Vec<PathBuf> {
        let mut m = self.map();
        m.retain(|_, h| now.saturating_duration_since(h.last_change) < LIFETIME);
        self.len.store(m.len(), Ordering::Relaxed);
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
        let mut m = self.map();
        let Some(size) = size else {
            m.remove(p);
            self.len.store(m.len(), Ordering::Relaxed);
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
        self.map().values().map(|h| h.next_poll).min()
    }
}
