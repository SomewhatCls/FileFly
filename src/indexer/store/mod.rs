//! # File format (`folder_sizes.ffx`)
//! 
//! ```text
//! [ header 4 KiB ][ slot 0 ][ slot 1 ] ... [ slot 2^bits - 1 ]        every slot is 4 KiB
//! ```
//! * A path is reduced to a 48-bit key (stable FNV-1a + finalizer over its components). The top
//!   `bits` bits of the key choose the slot, so a lookup is: hash, ONE 4 KiB read, scan the slot.
//!   The path text itself is never stored, which is what makes the file small.
//! * Inside a slot the entries are sorted by key and delta-coded with varints:
//!   `key delta | size (varint) | mtime fingerprint (u32) | scanned_at - slot minimum (varint)`
//!   which is ~16 bytes per folder (a path-keyed B-tree needs 150+).
//! * Every slot has a CRC. A torn or corrupt slot simply counts as empty (it is only a cache).
//! * The table doubles in place when the slots get ~70% full (slot i becomes 2i and 2i+1, done
//!   from the back so nothing is overwritten before it is read). A crash during that leaves a flag
//!   in the header and the file is recreated empty at the next start.
//! * If a slot overflows, its oldest entries are dropped (they get rescanned when needed).
//! * Writes are batched: results collect in RAM (`pending`) and are merged into the slots they
//!   touch, 32k entries at a time or when `save_if_dirty` runs. The file never has garbage that
//!   needs compacting, its size only changes when it doubles.
//! * Subtree invalidation cannot enumerate descendants (no path text), so it records
//!   "everything below DIR scanned before TIME is invalid" instead (kept in the header).
//!
//! Files: `util` (hashing, CRC, varints), `io` (positional I/O), `format` (slot codec),
//! `table` (header, growth, batched writes); this file holds the `Store` that ties them together.

use self::{
    format::{decode, encode_payload, fill_slot, slot_off, Rec, MAX_TOMBS, PRUNE_EVERY, SLOT},
    io::{read_at, write_at},
    table::{apply, open_table, reset, write_header, Table},
    util::{key_of, mix64, KeyMap},
};
use super::model::{now_secs, CachedFolder};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex, MutexGuard, RwLock,
    },
    time::Duration,
};

mod format;
mod io;
mod table;
mod util;

/// Pending (not yet written) entries that trigger a write.
pub use format::FLUSH_AT;
/// Name of the index file inside the index folder.
pub use table::DB_FILE;

/// The index shared between the UI, the scan workers and the live updater.
pub type Cache = Arc<Store>;

// The store: pending writes + RAM cache + table

struct Inner {
    /// Clean read cache (limited by `max_ram`). Anything in here is also on disk or pending.
    ram: KeyMap<Rec>,
    /// Changes not on disk yet (None = deleted). Never evicted.
    pending: KeyMap<Option<Rec>>,
    /// The batch that is being written right now; still visible to readers.
    flushing: Option<Arc<KeyMap<Option<Rec>>>>,
    /// "Entries below this folder scanned before this time (unix seconds) are invalid."
    tombs: Vec<(PathBuf, u64)>,
    /// Counts removals, so a slow disk read cannot put a just-removed entry back into RAM.
    removals: u64,
    salt: u64,
}

impl Inner {
    /// Some(None) = known to be deleted, None = not in memory.
    fn mem(&self, k: u64) -> Option<Option<Rec>> {
        if let Some(v) = self.pending.get(&k) {
            return Some(*v);
        }
        if let Some(f) = &self.flushing {
            if let Some(v) = f.get(&k) {
                return Some(*v);
            }
        }
        self.ram.get(&k).map(|r| Some(*r))
    }

    fn tombed(&self, p: &Path, scanned: u32) -> bool {
        self.tombs.iter().any(|(dir, ts)| (scanned as u64) < *ts && p.starts_with(dir))
    }
}

pub struct Store {
    /// None if the file can't be opened (e.g. a second window has it open): then RAM is all
    /// there is, the limit is ignored and nothing is saved.
    table: Option<RwLock<Table>>,
    inner: Mutex<Inner>,
    /// Serializes everything that modifies slots (flush, prune, clear).
    flush_lock: Mutex<()>,
    /// Entry limit of the RAM cache (usize::MAX = unlimited).
    pub max_ram: AtomicUsize,
    /// Set while a background flush is running.
    pub bg_flush: AtomicBool,
    /// Called when the index file doubled (or was reset): its length is part of the folder it lives in.
    on_resize: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl Store {
    pub fn open(dir: &Path, max_ram: usize) -> Store {
        let (table, tombs) = match open_table(dir) {
            Some((t, tb)) => (Some(RwLock::new(t)), tb),
            None => (None, Vec::new()),
        };
        Store {
            table,
            inner: Mutex::new(Inner {
                ram: KeyMap::default(),
                pending: KeyMap::default(),
                flushing: None,
                tombs,
                removals: 0,
                salt: 0,
            }),
            flush_lock: Mutex::new(()),
            max_ram: AtomicUsize::new(max_ram),
            bg_flush: AtomicBool::new(false),
            on_resize: Mutex::new(None),
        }
    }

    /// Registers what to do when the index file changes size (see `on_resize`).
    pub fn set_on_resize(&self, f: impl Fn() + Send + Sync + 'static) {
        *self.on_resize.lock().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(f));
    }

    fn notify_resize(&self) {
        if let Some(f) = &*self.on_resize.lock().unwrap_or_else(|e| e.into_inner()) {
            f();
        }
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Once 10% over the limit, drops random entries back down to the limit.
    /// (Safe, because the file or `pending` still has them. Without a file nothing is dropped.)
    fn trim(&self, g: &mut Inner) {
        if self.table.is_none() {
            return;
        }
        let max = self.max_ram.load(Ordering::Relaxed);
        let len = g.ram.len();
        if len <= max.saturating_add(max / 10) {
            return;
        }
        g.salt = g.salt.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let salt = g.salt;
        let thr = ((max as u128 * u64::MAX as u128) / len as u128) as u64;
        g.ram.retain(|k, _| mix64(*k ^ salt) <= thr);
    }

    /// Re-applies the RAM limit (after it was changed).
    pub fn shrink(&self) {
        let mut g = self.inner();
        self.trim(&mut g);
    }

    pub fn get(&self, p: &Path) -> Option<CachedFolder> {
        let k = key_of(p);
        let epoch;
        {
            let g = self.inner();
            epoch = g.removals;
            if let Some(found) = g.mem(k) {
                let r = found?;
                return if g.tombed(p, r.scanned) { None } else { Some(r.to_cf()) };
            }
        }
        // Not in memory: one slot read from the file.
        let table = self.table.as_ref()?;
        let r = table.read().unwrap_or_else(|e| e.into_inner()).get(k)?;
        let mut g = self.inner();
        if g.removals == epoch {
            g.ram.insert(k, r);
            self.trim(&mut g);
        }
        if g.tombed(p, r.scanned) {
            None
        } else {
            Some(r.to_cf())
        }
    }

    pub fn put_many(&self, items: Vec<(PathBuf, CachedFolder)>) {
        let recs: Vec<(u64, Rec)> = items.iter().map(|(p, v)| (key_of(p), Rec::from_cf(v))).collect();
        let persist = self.table.is_some();
        let need_flush = {
            let mut g = self.inner();
            for (k, r) in recs {
                g.ram.insert(k, r);
                if persist {
                    g.pending.insert(k, Some(r));
                }
            }
            self.trim(&mut g);
            persist && g.pending.len() >= FLUSH_AT
        };
        if need_flush {
            self.flush();
        }
    }

    /// Removes `p` and every ancestor (their totals are no longer correct).
    pub fn remove_with_ancestors(&self, p: &Path) {
        let keys: Vec<u64> = p.ancestors().map(key_of).collect();
        let persist = self.table.is_some();
        let mut g = self.inner();
        g.removals += 1;
        for k in keys {
            g.ram.remove(&k);
            if persist {
                g.pending.insert(k, None);
            }
        }
    }

    /// Removes `dir`, everything below it and all its ancestors. Descendants cannot be listed
    /// (only hashes are stored), so they are invalidated by time: anything below `dir` that
    /// was scanned before now is ignored from here on and gets rescanned.
    pub fn remove_tree(&self, dir: &Path) {
        self.remove_with_ancestors(dir);
        let ts = now_secs();
        let mut g = self.inner();
        g.tombs.retain(|(d, _)| !d.starts_with(dir));
        g.tombs.push((dir.to_path_buf(), ts));
        let n = g.tombs.len();
        if n > MAX_TOMBS {
            g.tombs.drain(..n - MAX_TOMBS);
        }
    }

    pub fn clear(&self) {
        let _fl = self.flush_lock.lock().unwrap_or_else(|e| e.into_inner());
        {
            let mut g = self.inner();
            g.removals += 1;
            g.ram = KeyMap::default();
            g.pending = KeyMap::default();
            g.flushing = None;
            g.tombs.clear();
        }
        if let Some(table) = &self.table {
            let mut t = table.write().unwrap_or_else(|e| e.into_inner());
            let _ = reset(&mut t);
            drop(t);
            self.notify_resize(); // the file shrank back
        }
    }

    /// Number of indexed folders (all of them, not just the ones in RAM). Approximate:
    /// a rewritten entry that is still pending is counted twice.
    pub fn len(&self) -> usize {
        let g = self.inner();
        match &self.table {
            Some(table) => {
                let disk = table.read().unwrap_or_else(|e| e.into_inner()).count.load(Ordering::Relaxed);
                disk as usize + g.pending.values().filter(|v| v.is_some()).count()
            }
            None => g.ram.len(),
        }
    }

    pub fn pending_len(&self) -> usize {
        self.inner().pending.len()
    }

    /// Writes the pending changes into the file (no fsync, see `sync`).
    pub fn flush(&self) {
        let Some(table) = &self.table else { return };
        let _fl = self.flush_lock.lock().unwrap_or_else(|e| e.into_inner());
        let (batch, tombs) = {
            let mut g = self.inner();
            if g.pending.is_empty() {
                return;
            }
            let b = Arc::new(std::mem::take(&mut g.pending));
            g.flushing = Some(Arc::clone(&b));
            (b, g.tombs.clone())
        };
        // (On an I/O error the batch is dropped: this is only a cache, the folders get rescanned.)
        let bits = |t: &RwLock<Table>| t.read().unwrap_or_else(|e| e.into_inner()).bits;
        let before = bits(table);
        let _ = apply(table, &batch, &tombs);
        self.inner().flushing = None;
        if bits(table) != before {
            self.notify_resize(); // the file doubled
        }
    }

    /// Flush + fsync: everything written so far survives a crash or power loss.
    pub fn sync(&self) {
        self.flush();
        if let Some(table) = &self.table {
            let _ = table.read().unwrap_or_else(|e| e.into_inner()).file.sync_data();
        }
    }

    /// Drops entries not refreshed since `cutoff` (the PRUNE_SECS rule). Reads the whole file,
    /// so it runs at most once a day, slot by slot, without blocking other work for long.
    pub fn prune(&self, cutoff: u64) {
        {
            let mut g = self.inner();
            g.ram.retain(|_, r| r.scanned as u64 >= cutoff);
            g.tombs.retain(|(_, ts)| *ts >= cutoff);
        }
        let Some(table) = &self.table else { return };
        let last = table.read().unwrap_or_else(|e| e.into_inner()).last_prune.load(Ordering::Relaxed);
        if now_secs().saturating_sub(last) < PRUNE_EVERY {
            return;
        }
        let mut buf = vec![0u8; SLOT];
        let mut b = 0u64;
        loop {
            let more = {
                let _fl = self.flush_lock.lock().unwrap_or_else(|e| e.into_inner());
                let t = table.read().unwrap_or_else(|e| e.into_inner());
                if b >= (1u64 << t.bits) {
                    false
                } else {
                    if read_at(&t.file, &mut buf, slot_off(b)).is_ok() {
                        if let Some(entries) = decode(&buf, b, t.bits) {
                            let kept: Vec<(u64, Rec)> =
                                entries.iter().copied().filter(|e| e.1.scanned as u64 >= cutoff).collect();
                            if kept.len() != entries.len() {
                                fill_slot(&mut buf, &encode_payload(&kept, t.bits), kept.len());
                                if write_at(&t.file, &buf, slot_off(b)).is_ok() {
                                    let removed = (entries.len() - kept.len()) as u64;
                                    let c = t.count.load(Ordering::Relaxed).saturating_sub(removed);
                                    t.count.store(c, Ordering::Relaxed);
                                }
                            }
                        }
                    }
                    true
                }
            };
            if !more {
                break;
            }
            b += 1;
            if b % 64 == 0 {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        let tombs = self.inner().tombs.clone();
        let _fl = self.flush_lock.lock().unwrap_or_else(|e| e.into_inner());
        let t = table.read().unwrap_or_else(|e| e.into_inner());
        t.last_prune.store(now_secs(), Ordering::Relaxed);
        let _ = write_header(&t, false, &tombs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ff_idx_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }
    fn cf(n: u64) -> CachedFolder {
        CachedFolder {
            size: n * 1000 + 7,
            modified_ns: (n as u32).wrapping_mul(2_654_435_761) as u64,
            scanned_at: 1_700_000_000 + n % 5000,
        }
    }
    fn path(root: &Path, i: u64) -> PathBuf {
        root.join(format!("dir{i}")).join("sub")
    }

    #[test]
    fn roundtrip_grow_reopen_remove() {
        let dir = tmp("rt");
        let root = PathBuf::from("data_root");
        let n = 60_000u64;
        {
            let s = Store::open(&dir, 1000);
            let all: Vec<u64> = (0..n).collect();
            for chunk in all.chunks(5000) {
                s.put_many(chunk.iter().map(|&i| (path(&root, i), cf(i))).collect());
            }
            s.sync();
            for i in (0..n).step_by(97) {
                assert_eq!(s.get(&path(&root, i)).map(|c| c.size), Some(cf(i).size));
            }
            assert!(s.get(&root.join("nope")).is_none());
        }
        // Reopen: RAM is empty, everything comes from the file.
        let s = Store::open(&dir, 1000);
        assert!(s.len() as u64 >= n * 99 / 100);
        for i in (0..n).step_by(31) {
            let got = s.get(&path(&root, i)).expect("entry survives a restart");
            assert_eq!(got.size, cf(i).size);
            assert_eq!(got.modified_ns, cf(i).modified_ns);
            assert_eq!(got.scanned_at, cf(i).scanned_at);
        }
        // Single removal (+ ancestors), persisted.
        s.remove_with_ancestors(&path(&root, 5));
        assert!(s.get(&path(&root, 5)).is_none());
        s.sync();
        drop(s);
        let s = Store::open(&dir, 1000);
        assert!(s.get(&path(&root, 5)).is_none());
        assert!(s.get(&path(&root, 6)).is_some());
        // Subtree removal: everything below is ignored, entries scanned later are valid again.
        s.remove_tree(&root.join("dir7"));
        assert!(s.get(&path(&root, 7)).is_none());
        assert!(s.get(&path(&root, 8)).is_some());
        let mut fresh = cf(7);
        fresh.scanned_at = now_secs() + 5;
        s.put_many(vec![(path(&root, 7), fresh)]);
        assert!(s.get(&path(&root, 7)).is_some());
        let _ = fs::remove_dir_all(&dir);
    }
}
