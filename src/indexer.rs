//! Folder-size index.
//!
//! Two tiers:
//!  * EVERY folder size that was ever scanned lives in an on-disk database (`redb`). It is written
//!    while the scan runs, so a cancelled scan or a crash keeps its progress, and there is no big
//!    "save everything" step on exit and no big "load everything" step at startup;
//!  * a bounded in-RAM cache sits in front of it (the limit is a user setting, `limit_mb`). A lookup
//!    that misses RAM reads the database and copies the entry into RAM. When RAM goes over the
//!    limit, entries are dropped from RAM only (the database still has them).
//!
//! Also:
//!  * the cache is shared (`Arc`) instead of being cloned on every navigation;
//!  * scanning is iterative, runs on a small worker pool at below-normal priority and yields regularly;
//!  * every finished sub-folder is stored immediately, so cancelled scans keep their progress;
//!  * entries are only trusted while the folder's mtime matches *and* they are younger than `TTL_SECS`
//!    (a directory's mtime does not change when a file deeper down grows, so mtime alone goes stale);
//!  * a file watcher invalidates the changed path and all its ancestors, so new files show up live.

use eframe::egui;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::Sender,
        Arc, Mutex, RwLock,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const DB_FILE: &str = "folder_sizes.redb";
/// Files of the earlier index formats; removed at startup.
const OLD_FILES: [&str; 2] = ["folder_sizes.json", "folder_sizes.bin"];
/// Maximum age before a cached folder is re-scanned even if its mtime is unchanged.
const TTL_SECS: u64 = 6 * 3600;
/// Entries not refreshed for this long are dropped from the database at startup.
const PRUNE_SECS: u64 = 30 * 24 * 3600;
/// redb's own page cache. Its default is 1 GiB, far too much for a file manager.
const DB_PAGE_CACHE: usize = 32 << 20;

/// Rough RAM cost of one cached folder; only used to turn the "MB" setting into an entry count.
/// An estimate for ~80-character Windows paths, not a measurement:
///   hash table slot (PathBuf 24-32 B + CachedFolder 24 B + 1 control byte, times ~1.6 because the
///   table is never full)                                         ~ 85 B
///   path text on the heap (~80 B, rounded up by the allocator)   ~ 95 B
const ENTRY_BYTES: usize = 170;

fn entries_for_mb(mb: usize) -> usize {
    mb.saturating_mul(1 << 20) / ENTRY_BYTES
}

#[derive(Clone)]
pub struct CachedFolder {
    pub size: u64,
    /// Modification time of the folder, nanoseconds since 1970 (fits a u64 until the year 2554).
    pub modified_ns: u64,
    /// Unix seconds of the scan.
    pub scanned_at: u64,
}

pub enum IndexMsg {
    Size(PathBuf, u64),
}

/// Number of scan worker threads per `start()`.
const WORKERS: usize = 3;

/// Counts a live scan worker: incremented before the thread is spawned, decremented when the
/// thread ends for any reason (finished, cancelled, channel closed, panic).
struct Live(Arc<AtomicUsize>);
impl Drop for Live {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}
fn ns(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0)
}

// ----------------------------------------------------------------------------------------------
// Storage: database on disk + bounded cache in RAM

const TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("folders");

/// 24 bytes per folder on disk: size, modified, scanned_at.
fn enc(v: &CachedFolder) -> [u8; 24] {
    let mut b = [0u8; 24];
    b[..8].copy_from_slice(&v.size.to_le_bytes());
    b[8..16].copy_from_slice(&v.modified_ns.to_le_bytes());
    b[16..].copy_from_slice(&v.scanned_at.to_le_bytes());
    b
}

fn dec(b: &[u8]) -> Option<CachedFolder> {
    if b.len() != 24 {
        return None;
    }
    Some(CachedFolder {
        size: u64::from_le_bytes(b[..8].try_into().ok()?),
        modified_ns: u64::from_le_bytes(b[8..16].try_into().ok()?),
        scanned_at: u64::from_le_bytes(b[16..].try_into().ok()?),
    })
}

struct Store {
    /// None if the database can't be opened (e.g. a second window has it open): then RAM is all
    /// there is, the limit is ignored and nothing is saved.
    db: Option<Database>,
    ram: RwLock<HashMap<PathBuf, CachedFolder>>,
    /// Entry limit of `ram` (usize::MAX = unlimited).
    max_ram: AtomicUsize,
}

type Cache = Arc<Store>;

fn open_db(dir: &Path) -> Option<Database> {
    fs::create_dir_all(dir).ok()?;
    let db = Database::builder().set_cache_size(DB_PAGE_CACHE).create(dir.join(DB_FILE)).ok()?;
    let tx = db.begin_write().ok()?;
    tx.open_table(TABLE).ok()?; // make sure the table exists
    tx.commit().ok()?;
    Some(db)
}

impl Store {
    fn open(dir: &Path, max_ram: usize) -> Store {
        Store { db: open_db(dir), ram: RwLock::new(HashMap::new()), max_ram: AtomicUsize::new(max_ram) }
    }

    /// Adds to RAM. Once 10% over the limit, drops arbitrary entries back down to the limit.
    /// (Safe, because the database still has them. Without a database nothing is dropped.)
    fn ram_insert(&self, items: Vec<(PathBuf, CachedFolder)>) {
        let Ok(mut m) = self.ram.write() else { return };
        m.extend(items);
        let max = if self.db.is_some() { self.max_ram.load(Ordering::Relaxed) } else { usize::MAX };
        if m.len() > max.saturating_add(max / 10) {
            let doomed: Vec<PathBuf> = m.keys().take(m.len() - max).cloned().collect();
            for k in doomed {
                m.remove(&k);
            }
        }
    }

    fn disk_get(&self, p: &Path) -> Option<CachedFolder> {
        let db = self.db.as_ref()?;
        let rt = db.begin_read().ok()?;
        let t = rt.open_table(TABLE).ok()?;
        let g = t.get(p.to_str()?.as_bytes()).ok()??;
        let v = dec(g.value());
        v
    }

    /// Runs `f` in one write transaction. The commit does not fsync (see `sync_to_disk`).
    fn disk_write(&self, f: impl FnOnce(&mut redb::Table<'_, &'static [u8], &'static [u8]>)) {
        let Some(db) = &self.db else { return };
        let Ok(mut tx) = db.begin_write() else { return };
        let _ = tx.set_durability(redb::Durability::None);
        if let Ok(mut t) = tx.open_table(TABLE) {
            f(&mut t);
        }
        let _ = tx.commit();
    }

    fn get(&self, p: &Path) -> Option<CachedFolder> {
        if let Some(v) = self.ram.read().ok().and_then(|m| m.get(p).cloned()) {
            return Some(v);
        }
        let v = self.disk_get(p)?;
        self.ram_insert(vec![(p.to_path_buf(), v.clone())]);
        Some(v)
    }

    fn put_many(&self, items: Vec<(PathBuf, CachedFolder)>) {
        self.disk_write(|t| {
            for (p, v) in &items {
                if let Some(k) = p.to_str() {
                    // (a path that is not valid Unicode is only kept in RAM)
                    let _ = t.insert(k.as_bytes(), enc(v).as_slice());
                }
            }
        });
        self.ram_insert(items);
    }

    /// Removes `p` and every ancestor (their totals are no longer correct).
    fn remove_with_ancestors(&self, p: &Path) {
        self.disk_write(|t| {
            for c in p.ancestors() {
                if let Some(k) = c.to_str() {
                    let _ = t.remove(k.as_bytes());
                }
            }
        });
        if let Ok(mut m) = self.ram.write() {
            for c in p.ancestors() {
                m.remove(c);
            }
        }
    }

    /// Removes `dir`, everything below it and all its ancestors.
    fn remove_tree(&self, dir: &Path) {
        self.disk_write(|t| {
            if let Some(d) = dir.to_str() {
                // Keys are sorted, so everything below "C:\a" is one run starting at "C:\a\"
                // (the separator keeps "C:\ab" out of it).
                let mut prefix = d.to_owned();
                if !prefix.ends_with(std::path::MAIN_SEPARATOR) {
                    prefix.push(std::path::MAIN_SEPARATOR);
                }
                let doomed: Vec<Vec<u8>> = match t.range(prefix.as_bytes()..) {
                    Ok(it) => it
                        .flatten()
                        .map(|(k, _)| k.value().to_vec())
                        .take_while(|k| k.starts_with(prefix.as_bytes()))
                        .collect(),
                    Err(_) => Vec::new(),
                };
                for k in doomed {
                    let _ = t.remove(k.as_slice());
                }
            }
            for c in dir.ancestors() {
                if let Some(k) = c.to_str() {
                    let _ = t.remove(k.as_bytes());
                }
            }
        });
        if let Ok(mut m) = self.ram.write() {
            m.retain(|k, _| !k.starts_with(dir));
            for c in dir.ancestors() {
                m.remove(c);
            }
        }
    }

    fn clear(&self) {
        if let Some(db) = &self.db {
            if let Ok(tx) = db.begin_write() {
                let _ = tx.delete_table(TABLE);
                let _ = tx.commit();
            }
        }
        if let Ok(mut m) = self.ram.write() {
            m.clear();
        }
    }

    /// Number of indexed folders (all of them, not just the ones in RAM).
    fn len(&self) -> usize {
        match &self.db {
            Some(db) => db
                .begin_read()
                .ok()
                .and_then(|rt| rt.open_table(TABLE).ok().and_then(|t| t.len().ok()))
                .unwrap_or(0) as usize,
            None => self.ram.read().map(|m| m.len()).unwrap_or(0),
        }
    }

    /// One real (fsynced) commit makes all the earlier no-fsync commits permanent.
    fn sync_to_disk(&self) {
        if let Some(db) = &self.db {
            if let Ok(tx) = db.begin_write() {
                let _ = tx.commit();
            }
        }
    }

    /// Drops entries not refreshed since `cutoff` (the PRUNE_SECS rule).
    fn drop_older_than(&self, cutoff: u64) {
        if let Some(db) = &self.db {
            let old: Vec<Vec<u8>> = {
                let Ok(rt) = db.begin_read() else { return };
                let Ok(t) = rt.open_table(TABLE) else { return };
                let Ok(it) = t.iter() else { return };
                it.flatten()
                    .filter(|(_, v)| dec(v.value()).is_some_and(|f| f.scanned_at < cutoff))
                    .map(|(k, _)| k.value().to_vec())
                    .collect()
            };
            self.disk_write(|t| {
                for k in old {
                    let _ = t.remove(k.as_slice());
                }
            });
        }
        if let Ok(mut m) = self.ram.write() {
            m.retain(|_, v| v.scanned_at >= cutoff);
        }
    }
}

// ----------------------------------------------------------------------------------------------
// Indexer

/// Opens the database in `dir`, clears out leftovers of older formats and starts the background
/// pruning of entries that haven't been refreshed for `PRUNE_SECS`.
fn open_store(dir: &Path, max_entries: usize) -> Cache {
    for old in OLD_FILES {
        let _ = fs::remove_file(dir.join(old));
    }
    let store = Arc::new(Store::open(dir, max_entries));
    let pruner = Arc::clone(&store);
    std::thread::spawn(move || pruner.drop_older_than(now_secs().saturating_sub(PRUNE_SECS)));
    store
}

pub struct Indexer {
    cache: Cache,
    /// Folder the database lives in.
    dir: PathBuf,
    generation: Arc<AtomicU64>,
    cache_dirty: Arc<AtomicBool>,
    live: Arc<AtomicUsize>,
    tx: Sender<IndexMsg>,
    ctx: egui::Context,
}

impl Indexer {
    /// `dir` is where the database lives. `limit_mb` is the RAM cache limit (0 = unlimited).
    pub fn new(ctx: egui::Context, tx: Sender<IndexMsg>, dir: &Path, limit_mb: usize) -> Self {
        let me = Self {
            cache: open_store(dir, entries_for_mb(limit_mb)),
            dir: dir.to_path_buf(),
            generation: Arc::new(AtomicU64::new(0)),
            cache_dirty: Arc::new(AtomicBool::new(false)),
            live: Arc::new(AtomicUsize::new(0)),
            tx,
            ctx,
        };
        me
    }

    /// Switches to the database in `dir` (created if it doesn't exist there yet). This is what
    /// replaces the old `load(dir)` when the user picks another index folder. The old folder's
    /// database is left where it is.
    pub fn set_dir(&mut self, dir: &Path) {
        if dir == self.dir {
            return;
        }
        self.generation.fetch_add(1, Ordering::SeqCst); // stop scans that still write to the old database
        self.cache.sync_to_disk(); // make the old database's last writes permanent
        let max_entries = self.cache.max_ram.load(Ordering::Relaxed);
        self.cache = open_store(dir, max_entries);
        self.dir = dir.to_path_buf();
        self.cache_dirty.store(false, Ordering::Relaxed);
    }

    /// Changes the RAM cache limit and trims RAM if it is now over the limit.
    pub fn set_limit_mb(&self, mb: usize, unlimited_cache: bool) {
        if unlimited_cache {
            self.cache.max_ram.store(usize::MAX, Ordering::Relaxed);
        } else {
            self.cache.max_ram.store(entries_for_mb(mb), Ordering::Relaxed);
        }
        self.cache.ram_insert(Vec::new());
        eprintln!("Cache limit set to {} MB", if unlimited_cache { usize::MAX } else { mb });
    }

    /// Number of indexed folders (all of them, not just the ones currently in RAM).
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Cached size regardless of age (shown immediately; fresh value follows).
    /// A folder that is not in RAM is read from the database, so call this once per folder
    /// (when a listing loads), not for every row on every frame.
    pub fn lookup(&self, path: &Path) -> Option<u64> {
        self.cache.get(path).map(|c| c.size)
    }

    /// Drop the path and every ancestor: their totals are no longer correct.
    pub fn invalidate(&self, path: &Path) {
        self.cache.remove_with_ancestors(path);
        self.cache_dirty.store(true, Ordering::Relaxed);
    }

    /// Drop `dir`, everything below it and all its ancestors (used when too many changes happened
    /// to list them individually).
    pub fn invalidate_tree(&self, dir: &Path) {
        self.cache.remove_tree(dir);
        self.cache_dirty.store(true, Ordering::Relaxed);
    }

    /// True while a scan started by `start()` still has a live worker thread.
    pub fn is_running(&self) -> bool {
        self.live.load(Ordering::SeqCst) > 0
    }

    /// Forgets everything: stops a running scan and empties the index (RAM and database).
    /// (The database file stays open, so it is emptied rather than deleted.)
    pub fn delete_index(&self, _dir: &Path) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.cache.clear();
        self.cache_dirty.store(false, Ordering::Relaxed);
    }

    pub fn is_dirty(&self) -> bool {
        self.cache_dirty.load(Ordering::Relaxed)
    }

    pub fn clear(&self) {
        self.cache.clear();
        self.cache_dirty.store(true, Ordering::Relaxed);
    }

    /// Cancels any running scan and starts a new one for `folders`. Up-to-date folders return instantly.
    pub fn start(&self, folders: Vec<PathBuf>) {
        let my_gen = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        if folders.is_empty() {
            return;
        }
        // A small worker pool takes folders from a shared queue, so one huge folder (Windows,
        // AppData, ...) no longer holds up every row behind it.
        let workers = WORKERS.min(folders.len());
        let queue = Arc::new(Mutex::new(folders.into_iter().collect::<VecDeque<_>>()));
        for _ in 0..workers {
            let queue = Arc::clone(&queue);
            let cache = Arc::clone(&self.cache);
            let generation = Arc::clone(&self.generation);
            let dirty = Arc::clone(&self.cache_dirty);
            let tx = self.tx.clone();
            let ctx = self.ctx.clone();
            self.live.fetch_add(1, Ordering::SeqCst);
            let guard = Live(Arc::clone(&self.live));
            std::thread::spawn(move || {
                let _guard = guard;
                lower_priority();
                loop {
                    if generation.load(Ordering::Relaxed) != my_gen {
                        return;
                    }
                    let Some(folder) = queue.lock().ok().and_then(|mut q| q.pop_front()) else { return };
                    let stamp = fs::metadata(&folder).and_then(|m| m.modified()).map(ns).unwrap_or(0);
                    let now = now_secs();
                    let fresh = cache.get(&folder).filter(|c| is_fresh(c, stamp, now));
                    let size = match fresh {
                        Some(c) => c.size,
                        None => match scan(&folder, stamp, &cache, &generation, my_gen, &dirty) {
                            Some(s) => s,
                            None => return, // cancelled
                        },
                    };
                    if tx.send(IndexMsg::Size(folder, size)).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            });
        }
    }

    // ---------------------------------------------------------------- persistence

    /// The database is written while scanning; this only makes those writes permanent (one fsync),
    /// in a background thread unless `wait` is set (use that on exit).
    pub fn save_if_dirty(&self, _dir: &Path, wait: bool) {
        if !self.cache_dirty.swap(false, Ordering::Relaxed) {
            return;
        }
        let store = Arc::clone(&self.cache);
        if wait {
            store.sync_to_disk();
        } else {
            std::thread::spawn(move || store.sync_to_disk());
        }
    }
}

fn is_fresh(c: &CachedFolder, stamp: u64, now: u64) -> bool {
    c.modified_ns == stamp && now.saturating_sub(c.scanned_at) < TTL_SECS
}

struct Frame {
    path: PathBuf,
    rd: fs::ReadDir,
    total: u64,
    stamp: u64,
}

enum Step {
    Add(u64),
    Descend(PathBuf, u64),
    Skip,
    End,
}

/// Iterative post-order walk. Returns None if cancelled. Every folder that is finished is stored.
fn scan(root: &Path, root_stamp: u64, cache: &Cache, generation: &AtomicU64, my_gen: u64, dirty: &AtomicBool) -> Option<u64> {
    let now = now_secs();
    let mut pending: Vec<(PathBuf, CachedFolder)> = Vec::new();
    let flush = |pending: &mut Vec<(PathBuf, CachedFolder)>| {
        if pending.is_empty() {
            return;
        }
        cache.put_many(std::mem::take(pending));
        dirty.store(true, Ordering::Relaxed);
    };

    let Ok(rd) = fs::read_dir(root) else { return Some(0) };
    let mut stack = vec![Frame { path: root.to_path_buf(), rd, total: 0, stamp: root_stamp }];
    let mut counter = 0u32;

    loop {
        if generation.load(Ordering::Relaxed) != my_gen {
            flush(&mut pending); // keep the completed sub-folders
            return None;
        }
        counter += 1;
        if counter % 4096 == 0 {
            flush(&mut pending);
            std::thread::sleep(Duration::from_millis(2)); // duty-cycle cap: leave CPU/disk for the UI
        }

        let step = {
            let top = stack.last_mut()?;
            match top.rd.next() {
                None => Step::End,
                Some(Err(_)) => Step::Skip,
                Some(Ok(de)) => match de.file_type() {
                    Ok(ft) if ft.is_symlink() => Step::Skip, // symlinks & junctions: avoid loops/double counting
                    Ok(ft) if ft.is_file() => Step::Add(de.metadata().map(|m| m.len()).unwrap_or(0)),
                    Ok(ft) if ft.is_dir() => {
                        let stamp = de.metadata().and_then(|m| m.modified()).map(ns).unwrap_or(0);
                        Step::Descend(de.path(), stamp)
                    }
                    _ => Step::Skip,
                },
            }
        };

        match step {
            Step::Skip => {}
            Step::Add(n) => {
                if let Some(top) = stack.last_mut() {
                    top.total = top.total.saturating_add(n);
                }
            }
            Step::Descend(path, stamp) => {
                let hit = cache.get(&path).filter(|c| is_fresh(c, stamp, now));
                if let Some(hit) = hit {
                    if let Some(top) = stack.last_mut() {
                        top.total = top.total.saturating_add(hit.size);
                    }
                } else if let Ok(rd) = fs::read_dir(&path) {
                    stack.push(Frame { path, rd, total: 0, stamp });
                }
            }
            Step::End => {
                let done = stack.pop()?;
                pending.push((done.path, CachedFolder { size: done.total, modified_ns: done.stamp, scanned_at: now }));
                match stack.last_mut() {
                    Some(parent) => parent.total = parent.total.saturating_add(done.total),
                    None => {
                        flush(&mut pending);
                        return Some(done.total);
                    }
                }
            }
        }
    }
}

#[cfg(windows)]
fn lower_priority() {
    // THREAD_PRIORITY_BELOW_NORMAL (-1): keeps the UI responsive without starving the scan.
    // (THREAD_MODE_BACKGROUND_BEGIN also drops I/O to "very low", which can make a scan crawl
    // whenever anything else touches the disk.)
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentThread() -> isize;
        fn SetThreadPriority(h: isize, p: i32) -> i32;
    }
    unsafe {
        SetThreadPriority(GetCurrentThread(), -1);
    }
}
#[cfg(not(windows))]
fn lower_priority() {}

// ----------------------------------------------------------------------------------------------
// File watcher: live refresh + cache invalidation

/// Most distinct changed paths remembered between two UI frames; more than that collapses into
/// a single "something changed" marker (the empty path).
const MAX_PENDING: usize = 2048;

pub struct DirWatcher {
    watcher: RecommendedWatcher,
    current: Option<PathBuf>,
    pending: Arc<Mutex<HashSet<PathBuf>>>,
}

impl DirWatcher {
    pub fn new(ctx: egui::Context) -> Option<Self> {
        // Events go into a bounded set that the UI drains once per frame. (An unbounded channel
        // grows without limit while the window is minimised or hidden and nobody reads it.)
        let pending: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
        let sink = Arc::clone(&pending);
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                if matches!(ev.kind, EventKind::Access(_)) || ev.paths.is_empty() {
                    return;
                }
                if let Ok(mut set) = sink.lock() {
                    for p in ev.paths {
                        if set.len() < MAX_PENDING {
                            set.insert(p);
                        } else {
                            set.insert(PathBuf::new());
                            break;
                        }
                    }
                }
                ctx.request_repaint_after(Duration::from_millis(300));
            }
        })
        .ok()?;
        Some(Self { watcher, current: None, pending })
    }

    /// Changed paths since the last call. An empty path means "too many to list".
    pub fn take(&self) -> Vec<PathBuf> {
        match self.pending.lock() {
            Ok(mut s) => std::mem::take(&mut *s).into_iter().collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn watch(&mut self, dir: &Path) {
        if self.current.as_deref() == Some(dir) {
            return;
        }
        if let Some(old) = self.current.take() {
            let _ = self.watcher.unwatch(&old);
        }
        // Drive roots see constant system churn; only watch their direct children.
        // Network shares are watched shallowly too: recursive watches over SMB/NFS are slow and unreliable.
        let mode = if dir.parent().is_none() || crate::drives::is_network_path(dir) {
            RecursiveMode::NonRecursive
        } else {
            RecursiveMode::Recursive
        };
        if self.watcher.watch(dir, mode).is_ok() {
            self.current = Some(dir.to_path_buf());
        }
    }
}
