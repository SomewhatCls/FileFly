//! # File format (`folder_sizes.ffx`)
//! 
//! ```text
//! [ header 4 KiB ][ slot 0 ][ slot 1 ] ... [ slot 2^bits - 1 ]        every slot is 4 KiB
//! ```
//! * A path is reduced to a 64-bit key (stable FNV-1a + finalizer over its components). The top
//!   `bits` bits of the key choose the slot, so a lookup is: hash, ONE 4 KiB read, scan the slot.
//!   The path text itself is never stored, which is what makes the file small.
//! * Inside a slot the entries are sorted by key and delta-coded with varints:
//!   `key delta | size (varint) | mtime fingerprint (u32) | scan time - slot minimum (varint)`
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
    format::{decode, encode_payload, fill_slot, slot_off, walk, FLUSH_AT, MAX_TOMBS, PRUNE_EVERY, SLOT},
    io::{read_at, write_at},
    table::{apply, open_table, reset, write_header, Table},
    util::{key_of, KeyMap},
};
use super::model::{now_secs, CachedFolder, MutexExt, RwLockExt, TTL_WATCHED_SECS};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Condvar, Mutex, MutexGuard, RwLock,
    },
    time::Duration,
};

mod format;
mod io;
mod table;
mod util;

/// Name of the index file inside the index folder.
pub use table::DB_FILE;
/// Building blocks of a path key; the scan extends a parent's state with a child's name instead of
/// hashing the whole path of every folder.
#[cfg(windows)]
pub use util::extend_wide;
#[cfg(not(windows))]
pub use util::extend_name;
pub use util::{finish_key, key_state};

/// The index shared between the UI, the scan workers and the live updater.
pub type Cache = Arc<Store>;

// The store: pending writes + RAM cache + table

/// Which keys may be in the index file (a bit set with one hash). Once it is `ready`, a lookup of a
/// key whose bit is clear returns at once instead of reading a slot from the file: during a first
/// scan nearly every folder is such a miss. It only ever says "maybe" or "never stored", so
/// stale bits (after a prune or a removal) are harmless.
struct Filter {
    bits: Vec<AtomicU64>,
    ready: AtomicBool,
}

/// 2^26 bits = 8 MiB; about 3% false "maybe" answers for two million stored folders.
const FILTER_BITS: usize = 1 << 26;

impl Filter {
    fn new() -> Filter {
        Filter { bits: (0..FILTER_BITS / 64).map(|_| AtomicU64::new(0)).collect(), ready: AtomicBool::new(false) }
    }
    fn at(key: u64) -> (usize, u64) {
        let i = key as usize & (FILTER_BITS - 1); // the slot number uses the top bits, so use the low ones
        (i / 64, 1u64 << (i % 64))
    }
    fn add(&self, key: u64) {
        let (w, m) = Filter::at(key);
        self.bits[w].fetch_or(m, Ordering::Relaxed);
    }
    fn may_contain(&self, key: u64) -> bool {
        if !self.ready.load(Ordering::Acquire) {
            return true;
        }
        let (w, m) = Filter::at(key);
        self.bits[w].load(Ordering::Relaxed) & m != 0
    }
    fn clear(&self) {
        for w in &self.bits {
            w.store(0, Ordering::Relaxed);
        }
    }
}

/// Wakes the background flusher (see `spawn_flusher`).
struct Signal {
    go: Mutex<bool>,
    cv: Condvar,
    closed: AtomicBool,
}

impl Signal {
    fn wake(&self) {
        *self.go.lk() = true;
        self.cv.notify_one();
    }
}

struct Inner {
    /// Clean read cache. Anything in here is also on disk or pending.
    ram: KeyMap<CachedFolder>,
    /// Changes not on disk yet (None = deleted). Never evicted.
    pending: KeyMap<Option<CachedFolder>>,
    /// The batch that is being written right now; still visible to readers.
    flushing: Option<Arc<KeyMap<Option<CachedFolder>>>>,
    /// "Entries below this folder scanned before this time (unix seconds) are invalid." A tombstone
    /// older than the longest TTL is useless (everything it covers is too old to be fresh anyway),
    /// so they are dropped after that time and the list stays short.
    tombs: Vec<(PathBuf, u64)>,
    /// Counts removals, so a slow disk read cannot put a just-removed entry back into RAM.
    removals: u64,
}

fn expire_tombs(tombs: &mut Vec<(PathBuf, u64)>) {
    let now = now_secs();
    tombs.retain(|(_, ts)| now.saturating_sub(*ts) < TTL_WATCHED_SECS);
}

impl Inner {
    /// Some(None) = known to be deleted, None = not in memory.
    fn mem(&self, k: u64) -> Option<Option<CachedFolder>> {
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

    /// Is an entry scanned at `scanned` invalidated by a subtree removal? The path is only built
    /// (by `path`) if a tombstone is newer than the entry at all.
    fn tombed(&self, path: &dyn Fn() -> PathBuf, scanned: u32) -> bool {
        let scanned = scanned as u64;
        if !self.tombs.iter().any(|(_, ts)| scanned < *ts) {
            return false;
        }
        let p = path();
        self.tombs.iter().any(|(dir, ts)| scanned < *ts && p.starts_with(dir))
    }
}

/// How many pending entries make a flush worthwhile. Entries land in random slots (the key is a
/// hash), so a batch costs one 4 KiB read and write per slot it touches; a larger table needs
/// larger batches to keep that cost per entry down.
fn flush_at_for(bits: u32) -> usize {
    ((1usize << bits.min(20)) * 8).clamp(FLUSH_AT, 256 * 1024)
}

pub struct Store {
    /// None if the file can't be opened (e.g. a second window has it open): then RAM is all
    /// there is, the limit is ignored and nothing is saved.
    table: Option<RwLock<Table>>,
    /// Present whenever there is a table; see `Filter`.
    filter: Option<Filter>,
    inner: Mutex<Inner>,
    /// Serializes everything that modifies slots (flush, prune, clear).
    flush_lock: Mutex<()>,
    /// Something changed since the last `sync` (see `Indexer::save_if_dirty`).
    pub dirty: AtomicBool,
    /// Pending entries that trigger a background flush (grows with the table).
    flush_at: AtomicUsize,
    signal: Arc<Signal>,
    /// Called when the index file doubled (or was reset): its length is part of the folder it lives in.
    on_resize: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl Drop for Store {
    fn drop(&mut self) {
        self.signal.closed.store(true, Ordering::Release);
        self.signal.wake(); // lets the flusher thread end
    }
}

/// Starts the thread that writes the pending entries to the file whenever enough of them have
/// piled up, so that scan threads never wait for the disk. It ends when the store is dropped.
pub fn spawn_flusher(store: &Arc<Store>) {
    let weak = Arc::downgrade(store);
    let sig = Arc::clone(&store.signal);
    std::thread::spawn(move || {
        super::priority::lower_priority();
        loop {
            {
                let mut g = sig.go.lk();
                while !*g {
                    g = sig.cv.wait(g).unwrap_or_else(|e| e.into_inner());
                }
                *g = false;
            }
            if sig.closed.load(Ordering::Acquire) {
                break;
            }
            match weak.upgrade() {
                Some(s) => s.flush(),
                None => break,
            }
        }
    });
}

impl Store {
    pub fn open(dir: &Path) -> Store {
        let (table, mut tombs) = match open_table(dir) {
            Some((t, tb)) => (Some(RwLock::new(t)), tb),
            None => (None, Vec::new()),
        };
        expire_tombs(&mut tombs);
        // A new (empty) file needs no reading to know which keys it holds: none.
        let filter = table.as_ref().map(|t| {
            let f = Filter::new();
            if t.rd().count.load(Ordering::Relaxed) == 0 {
                f.ready.store(true, Ordering::Release);
            }
            f
        });
        let bits = table.as_ref().map_or(0, |t| t.rd().bits);
        Store {
            table,
            filter,
            inner: Mutex::new(Inner {
                ram: KeyMap::default(),
                pending: KeyMap::default(),
                flushing: None,
                tombs,
                removals: 0,
            }),
            flush_lock: Mutex::new(()),
            dirty: AtomicBool::new(false),
            flush_at: AtomicUsize::new(flush_at_for(bits)),
            signal: Arc::new(Signal { go: Mutex::new(false), cv: Condvar::new(), closed: AtomicBool::new(false) }),
            on_resize: Mutex::new(None),
        }
    }

    /// Registers what to do when the index file changes size (see `on_resize`).
    pub fn set_on_resize(&self, f: impl Fn() + Send + Sync + 'static) {
        *self.on_resize.lk() = Some(Box::new(f));
    }

    fn notify_resize(&self) {
        if let Some(f) = &*self.on_resize.lk() {
            f();
        }
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lk()
    }

    /// Wakes the flusher if `pending` entries are waiting and that is enough for a flush.
    fn kick(&self, pending: usize) {
        if self.table.is_some() && pending >= self.flush_at.load(Ordering::Relaxed) {
            self.signal.wake();
        }
    }

    pub fn get(&self, p: &Path) -> Option<CachedFolder> {
        self.get_key(key_of(p), &|| p.to_path_buf())
    }

    /// `get` for a key that is already known; `path` builds the folder's path, and is only called
    /// if a tombstone makes it necessary (see `Inner::tombed`).
    pub fn get_key(&self, k: u64, path: &dyn Fn() -> PathBuf) -> Option<CachedFolder> {
        // Every key is added to the filter before it can be anywhere else (`put_many`), so once the
        // filter is ready a clear bit means "nowhere": no lock, no file read. During a first scan
        // that is nearly every lookup.
        if self.filter.as_ref().is_some_and(|f| !f.may_contain(k)) {
            return None;
        }
        let epoch;
        {
            let g = self.inner();
            epoch = g.removals;
            if let Some(found) = g.mem(k) {
                let r = found?;
                return if g.tombed(path, r.scanned) { None } else { Some(r) };
            }
        }
        // Not in memory: one slot read from the file.
        let table = self.table.as_ref()?;
        let r = table.rd().get(k)?;
        let mut g = self.inner();
        if g.removals == epoch {
            // `or_insert`: a newer entry that a scan stored while we were reading must stay.
            g.ram.entry(k).or_insert(r);
        }
        if g.tombed(path, r.scanned) {
            None
        } else {
            Some(r)
        }
    }

    pub fn put_many(&self, items: Vec<(u64, CachedFolder)>) {
        if let Some(f) = &self.filter {
            for (k, _) in &items {
                f.add(*k); // before the entry can reach the file
            }
        }
        let persist = self.table.is_some();
        let pending = {
            let mut g = self.inner();
            for (k, r) in items {
                g.ram.insert(k, r);
                if persist {
                    g.pending.insert(k, Some(r));
                }
            }
            g.pending.len()
        };
        self.dirty.store(true, Ordering::Relaxed);
        self.kick(pending);
    }

    /// Removes `p` and every ancestor (their totals are no longer correct).
    pub fn remove_with_ancestors(&self, p: &Path) {
        self.remove_many_with_ancestors(std::iter::once(p));
    }

    /// `remove_with_ancestors` for a whole batch of paths with one lock. A batch of events usually
    /// hits the same few folders over and over, so each folder's chain is only followed up to the
    /// first ancestor that an earlier path of the batch already covered.
    pub fn remove_many_with_ancestors<P: AsRef<Path>>(&self, paths: impl IntoIterator<Item = P>) {
        let mut keys: KeyMap<()> = KeyMap::default();
        for p in paths {
            for a in p.as_ref().ancestors() {
                if keys.insert(key_of(a), ()).is_some() {
                    break; // this ancestor, and so all above it, is in the set already
                }
            }
        }
        if keys.is_empty() {
            return;
        }
        let persist = self.table.is_some();
        let pending = {
            let mut g = self.inner();
            g.removals += 1;
            for k in keys.keys() {
                g.ram.remove(k);
                if persist {
                    g.pending.insert(*k, None);
                }
            }
            g.pending.len()
        };
        self.dirty.store(true, Ordering::Relaxed);
        self.kick(pending);
    }

    /// Removes `dir`, everything below it and all its ancestors. Descendants cannot be listed
    /// (only hashes are stored), so they are invalidated by time: anything below `dir` that
    /// was scanned before now is ignored from here on and gets rescanned.
    pub fn remove_tree(&self, dir: &Path) {
        self.remove_with_ancestors(dir);
        let ts = now_secs();
        let mut g = self.inner();
        expire_tombs(&mut g.tombs);
        g.tombs.retain(|(d, _)| !d.starts_with(dir));
        g.tombs.push((dir.to_path_buf(), ts));
        let n = g.tombs.len();
        if n > MAX_TOMBS {
            g.tombs.drain(..n - MAX_TOMBS);
        }
    }

    /// Empties the index (RAM and file; the file stays open, so it is reset rather than deleted).
    pub fn clear(&self) {
        let _fl = self.flush_lock.lk();
        {
            let mut g = self.inner();
            g.removals += 1;
            g.ram = KeyMap::default();
            g.pending = KeyMap::default();
            g.flushing = None;
            g.tombs.clear();
        }
        self.dirty.store(false, Ordering::Relaxed);
        if let Some(table) = &self.table {
            let mut t = table.wr();
            let _ = reset(&mut t);
            drop(t);
            if let Some(f) = &self.filter {
                f.ready.store(false, Ordering::Release);
                f.clear();
                f.ready.store(true, Ordering::Release); // the file is empty again
            }
            self.flush_at.store(flush_at_for(format::MIN_BITS), Ordering::Relaxed);
            self.notify_resize(); // the file shrank back
        }
    }

    /// Number of indexed folders (all of them, not just the ones in RAM). Approximate:
    /// a rewritten entry that is still pending is counted twice.
    pub fn len(&self) -> usize {
        let g = self.inner();
        match &self.table {
            Some(table) => {
                let disk = table.rd().count.load(Ordering::Relaxed);
                disk as usize + g.pending.values().filter(|v| v.is_some()).count()
            }
            None => g.ram.len(),
        }
    }

    /// Writes the pending changes into the file (no fsync, see `sync`).
    pub fn flush(&self) {
        let Some(table) = &self.table else { return };
        let _fl = self.flush_lock.lk();
        let (batch, tombs) = {
            let mut g = self.inner();
            if g.pending.is_empty() {
                return;
            }
            expire_tombs(&mut g.tombs);
            let b = Arc::new(std::mem::take(&mut g.pending));
            g.flushing = Some(Arc::clone(&b));
            (b, g.tombs.clone())
        };
        // (On an I/O error the batch is dropped: this is only a cache, the folders get rescanned.)
        let bits = |t: &RwLock<Table>| t.rd().bits;
        let before = bits(table);
        let _ = apply(table, &batch, &tombs);
        self.inner().flushing = None;
        let after = bits(table);
        self.flush_at.store(flush_at_for(after), Ordering::Relaxed);
        if after != before {
            self.notify_resize(); // the file doubled
        }
    }

    /// Flush + fsync: everything written so far survives a crash or power loss.
    pub fn sync(&self) {
        self.flush();
        if let Some(table) = &self.table {
            let _ = table.rd().file.sync_data();
        }
    }

    /// Reads the index file once from start to end. That puts it into the system's file cache and
    /// fills `filter`, so that afterwards a lookup of a folder that was never stored costs no disk
    /// read at all. Skipped for very large files (lookups then always read their slot).
    pub fn warm(&self) {
        const MAX: u64 = 256 << 20;
        const CHUNK: u64 = 256; // slots per read
        let (Some(table), Some(filter)) = (&self.table, &self.filter) else { return };
        if filter.ready.load(Ordering::Acquire) {
            return;
        }
        let mut buf = vec![0u8; CHUNK as usize * SLOT];
        let mut slot = 0u64;
        loop {
            // Writers (flush, prune, clear) are held off per chunk, so no slot is read half-written.
            let _fl = self.flush_lock.lk();
            let t = table.rd();
            let total = 1u64 << t.bits;
            if slot_off(total) > MAX {
                return;
            }
            if slot >= total {
                break;
            }
            let n = (total - slot).min(CHUNK);
            if read_at(&t.file, &mut buf[..n as usize * SLOT], slot_off(slot)).is_err() {
                return;
            }
            for i in 0..n as usize {
                let _ = walk(&buf[i * SLOT..(i + 1) * SLOT], slot + i as u64, t.bits, |k, _| {
                    filter.add(k);
                    true
                });
            }
            slot += n;
        }
        filter.ready.store(true, Ordering::Release);
    }

    /// Drops entries not refreshed since `cutoff` (the PRUNE_SECS rule). Reads the whole file,
    /// so it runs at most once a day, slot by slot, without blocking other work for long.
    pub fn prune(&self, cutoff: u64) {
        self.inner().ram.retain(|_, r| r.scanned as u64 >= cutoff);
        let Some(table) = &self.table else { return };
        let last = table.rd().last_prune.load(Ordering::Relaxed);
        if now_secs().saturating_sub(last) < PRUNE_EVERY {
            return;
        }
        let mut buf = vec![0u8; SLOT];
        let mut b = 0u64;
        loop {
            let more = {
                let _fl = self.flush_lock.lk();
                let t = table.rd();
                if b >= (1u64 << t.bits) {
                    false
                } else {
                    if read_at(&t.file, &mut buf, slot_off(b)).is_ok() {
                        if let Some(entries) = decode(&buf, b, t.bits) {
                            let kept: Vec<_> =
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
        let _fl = self.flush_lock.lk();
        let t = table.rd();
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
            stamp: (n as u32).wrapping_mul(2_654_435_761),
            scanned: (1_700_000_000 + n % 5000) as u32,
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
            let s = Store::open(&dir);
            let all: Vec<u64> = (0..n).collect();
            for chunk in all.chunks(5000) {
                s.put_many(chunk.iter().map(|&i| (key_of(&path(&root, i)), cf(i))).collect());
            }
            s.sync();
            for i in (0..n).step_by(97) {
                assert_eq!(s.get(&path(&root, i)).map(|c| c.size), Some(cf(i).size));
            }
            assert!(s.get(&root.join("nope")).is_none());
        }
        // Reopen: RAM is empty, everything comes from the file.
        let s = Store::open(&dir);
        assert!(s.len() as u64 >= n * 99 / 100);
        for i in (0..n).step_by(31) {
            let got = s.get(&path(&root, i)).expect("entry survives a restart");
            assert_eq!(got.size, cf(i).size);
            assert_eq!(got.stamp, cf(i).stamp);
            assert_eq!(got.scanned, cf(i).scanned);
        }
        // Single removal (+ ancestors), persisted.
        s.remove_with_ancestors(&path(&root, 5));
        assert!(s.get(&path(&root, 5)).is_none());
        s.sync();
        drop(s);
        let s = Store::open(&dir);
        assert!(s.get(&path(&root, 5)).is_none());
        assert!(s.get(&path(&root, 6)).is_some());
        // Subtree removal: everything below is ignored, entries scanned later are valid again.
        s.remove_tree(&root.join("dir7"));
        assert!(s.get(&path(&root, 7)).is_none());
        assert!(s.get(&path(&root, 8)).is_some());
        let mut fresh = cf(7);
        fresh.scanned = (now_secs() + 5) as u32;
        s.put_many(vec![(key_of(&path(&root, 7)), fresh)]);
        assert!(s.get(&path(&root, 7)).is_some());
        let _ = fs::remove_dir_all(&dir);
    }
}
