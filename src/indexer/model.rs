//! Plain data and small helpers shared by every part of the indexer.

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Condvar, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// How long a cached folder is trusted (while its mtime fingerprint still matches) when nothing
/// is watching it: the safety net for network shares, shallow watches, folders beyond the watch
/// limits and changes made while the app was closed.
pub const TTL_SECS: u64 = 3600;
/// The same for a folder under an active recursive watch that was already running when the folder
/// was scanned. The live updater corrects such folders within about a second of any change, so
/// this only has to cover events that get lost without a trace (sleep, a driver hiccup).
/// Overflows, watcher errors and dropped watches withdraw the trust at once (`ChangeSink`).
pub const TTL_WATCHED_SECS: u64 = 24 * 3600;
/// Entries not refreshed for this long are dropped from the file (checked at most once a day).
pub const PRUNE_SECS: u64 = 30 * 24 * 3600;

/// Number of scan worker threads per `Indexer::start()`. They share one walk over all listed
/// folders (see `scan::scan_many`), so this is not limited by the number of folders.
pub const WORKERS: usize = 8;

/// One indexed folder. This is also the record that is stored in the index file.
#[derive(Clone, Copy)]
pub struct CachedFolder {
    pub size: u64,
    /// Fingerprint of the folder's modification time (see `stamp_of`). It is only ever compared
    /// for equality, so it does not have to be the real timestamp.
    pub stamp: u32,
    /// Unix seconds of the scan.
    pub scanned: u32,
}

pub enum IndexMsg {
    Size(PathBuf, u64),
    /// Running total of a scan that is still in progress (a lower bound; `Size` follows when done).
    Partial(PathBuf, u64),
}

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Fingerprint of a modification time: the nanoseconds since 1970, scrambled and cut to 32 bits.
/// Two different mtimes collide with a chance of 1 in 4 billion, which only means a changed folder
/// is trusted until the TTL runs out. It saves 4 bytes per folder on disk and in RAM.
pub fn stamp_of(t: SystemTime) -> u32 {
    let n = t.duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
    (n.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32) as u32
}

/// An entry is trusted while the folder's mtime fingerprint matches *and* it is young enough:
/// `TTL_WATCHED_SECS` if it was scanned after `watched_since` (the moment a recursive watch
/// started covering the folder, see `ChangeSink::watched_since`), `TTL_SECS` otherwise.
pub fn is_fresh(c: &CachedFolder, stamp: u32, now: u64, watched_since: Option<u64>) -> bool {
    let ttl = if watched_since.is_some_and(|w| c.scanned as u64 >= w) { TTL_WATCHED_SECS } else { TTL_SECS };
    c.stamp == stamp && now.saturating_sub(c.scanned as u64) < ttl
}

/// "Is this scan still wanted?": the shared generation counter, as it was when the scan started.
/// Whoever bumps the counter cancels every scan that was started before.
#[derive(Clone)]
pub struct Cancel {
    generation: Arc<AtomicU64>,
    mine: u64,
}

impl Cancel {
    pub fn new(generation: &Arc<AtomicU64>, mine: u64) -> Cancel {
        Cancel { generation: Arc::clone(generation), mine }
    }

    pub fn is_set(&self) -> bool {
        self.generation.load(Ordering::Relaxed) != self.mine
    }
}

// Locks. A poisoned lock only means that another thread panicked; the data is still usable here
// (everything behind these locks is a cache), so it is simply taken.

pub trait MutexExt<T> {
    fn lk(&self) -> MutexGuard<'_, T>;
}

impl<T> MutexExt<T> for Mutex<T> {
    fn lk(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub trait RwLockExt<T> {
    fn rd(&self) -> RwLockReadGuard<'_, T>;
    fn wr(&self) -> RwLockWriteGuard<'_, T>;
}

impl<T> RwLockExt<T> for RwLock<T> {
    fn rd(&self) -> RwLockReadGuard<'_, T> {
        self.read().unwrap_or_else(|e| e.into_inner())
    }
    fn wr(&self) -> RwLockWriteGuard<'_, T> {
        self.write().unwrap_or_else(|e| e.into_inner())
    }
}

/// `Condvar::wait_timeout` without the poison handling and the timeout flag.
pub fn wait_for<'a, T>(cv: &Condvar, g: MutexGuard<'a, T>, d: Duration) -> MutexGuard<'a, T> {
    cv.wait_timeout(g, d).unwrap_or_else(|e| e.into_inner()).0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(scanned: u32) -> CachedFolder {
        CachedFolder { size: 1, stamp: 7, scanned }
    }

    #[test]
    fn ttl_depends_on_the_watch() {
        let now = 1_000_000_000u64;
        let two_hours = (now - 2 * 3600) as u32;
        // Nobody watches: the short TTL applies.
        assert!(!is_fresh(&folder(two_hours), 7, now, None));
        assert!(is_fresh(&folder((now - 600) as u32), 7, now, None));
        // Watched since before the scan: the long TTL applies.
        assert!(is_fresh(&folder(two_hours), 7, now, Some(two_hours as u64)));
        // The watch started after the scan: nothing vouches for it.
        assert!(!is_fresh(&folder(two_hours), 7, now, Some(two_hours as u64 + 1)));
        // Even a watched entry does not live forever, and a changed mtime always wins.
        assert!(!is_fresh(&folder((now - 25 * 3600) as u32), 7, now, Some(0)));
        assert!(!is_fresh(&folder(two_hours), 8, now, Some(0)));
    }
}
