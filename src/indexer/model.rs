//! Plain data and small helpers shared by every part of the indexer.

use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

/// Maximum age before a cached folder is re-scanned even if its mtime is unchanged.
///
/// This is only the *safety net* for places the live updater cannot see (folders that are not
/// being watched, network shares, changes made while the app was closed). Folders under an active
/// watch are corrected within about a second by `live`, not after this time.
pub const TTL_SECS: u64 = 3600;
/// Entries not refreshed for this long are dropped from the file (checked at most once a day).
pub const PRUNE_SECS: u64 = 30 * 24 * 3600;

/// Rough RAM cost of one cached folder; only used to turn the "MB" setting into an entry count.
/// 16 B of data + 8 B key, in a hash table that is never completely full: ~40-50 B.
/// (The old path-keyed cache cost ~170 B per folder, so the same limit now holds ~3.5x more.)
pub const ENTRY_BYTES: usize = 48;

/// Number of scan worker threads per `Indexer::start()`.
pub const WORKERS: usize = 3;

pub fn entries_for_mb(mb: usize) -> usize {
    mb.saturating_mul(1 << 20) / ENTRY_BYTES
}

#[derive(Clone)]
pub struct CachedFolder {
    pub size: u64,
    /// 32-bit fingerprint of the folder's modification time (see `ns`). It is only ever compared
    /// for equality, so it does not have to be the real timestamp.
    pub modified_ns: u64,
    /// Unix seconds of the scan.
    pub scanned_at: u64,
}

pub enum IndexMsg {
    Size(PathBuf, u64),
}

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Fingerprint of a modification time: the nanoseconds since 1970, scrambled and cut to 32 bits.
/// Two different mtimes collide with a chance of 1 in 4 billion, which only means a changed folder
/// is trusted until the TTL runs out. It saves 4 bytes per folder on disk and in RAM.
pub fn ns(t: SystemTime) -> u64 {
    let n = t.duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
    n.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32
}

/// An entry is trusted while the folder's mtime fingerprint matches *and* it is younger than the TTL.
pub fn is_fresh(c: &CachedFolder, stamp: u64, now: u64) -> bool {
    c.modified_ns == stamp && now.saturating_sub(c.scanned_at) < TTL_SECS
}
