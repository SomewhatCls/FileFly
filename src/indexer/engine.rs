//! `Indexer`: the object the rest of the app talks to. It owns the store, the scan workers and the
//! live updater, and keeps the public API of the old single-file indexer.

use super::{
    hot::HotFiles,
    live::{self, ChangeSink, LiveCtx},
    model::{entries_for_mb, is_fresh, ns, now_secs, IndexMsg, PRUNE_SECS, WORKERS},
    priority::lower_priority,
    scan::scan,
    store::{self, Cache, Store},
};
use eframe::egui;
use std::{
    collections::{HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::Sender,
        Arc, Mutex,
    },
};

/// Files of the earlier index formats; removed at startup (including the oversized redb file).
const OLD_FILES: [&str; 3] = ["folder_sizes.json", "folder_sizes.bin", "folder_sizes.redb"];

/// Counts a live scan worker: incremented before the thread is spawned, decremented when the
/// thread ends for any reason (finished, cancelled, channel closed, panic).
struct Live(Arc<AtomicUsize>);
impl Drop for Live {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Opens the index in `dir`, clears out leftovers of older formats and starts the background
/// pruning of entries that haven't been refreshed for `PRUNE_SECS`.
fn open_store(dir: &Path, max_entries: usize) -> Cache {
    for old in OLD_FILES {
        let _ = fs::remove_file(dir.join(old));
    }
    let store = Arc::new(Store::open(dir, max_entries));
    let pruner = Arc::clone(&store);
    std::thread::spawn(move || pruner.prune(now_secs().saturating_sub(PRUNE_SECS)));
    store
}

pub struct Indexer {
    cache: Cache,
    /// Folder the index file lives in.
    dir: PathBuf,
    generation: Arc<AtomicU64>,
    cache_dirty: Arc<AtomicBool>,
    live: Arc<AtomicUsize>,
    tx: Sender<IndexMsg>,
    ctx: egui::Context,
    /// Recently changed files (exact sizes, see `hot.rs`).
    hot: Arc<HotFiles>,
    /// Where the file watcher reports changes (see `live.rs`).
    sink: Arc<ChangeSink>,
    /// The folders of the current listing; the live updater keeps these up to date.
    tracked: Arc<Mutex<Arc<HashSet<PathBuf>>>>,
}

impl Indexer {
    /// `dir` is where the index file lives. `limit_mb` is the RAM cache limit (0 = unlimited).
    pub fn new(ctx: egui::Context, tx: Sender<IndexMsg>, dir: &Path, limit_mb: usize) -> Self {
        let max_entries = if limit_mb == 0 { usize::MAX } else { entries_for_mb(limit_mb) };
        let sink = ChangeSink::new();
        sink.set_ignored_dir(dir);
        let ix = Self {
            cache: open_store(dir, max_entries),
            dir: dir.to_path_buf(),
            generation: Arc::new(AtomicU64::new(0)),
            cache_dirty: Arc::new(AtomicBool::new(false)),
            live: Arc::new(AtomicUsize::new(0)),
            tx,
            ctx,
            hot: Arc::new(HotFiles::default()),
            sink,
            tracked: Arc::new(Mutex::new(Arc::new(HashSet::new()))),
        };
        ix.spawn_live();
        ix
    }

    /// (Re)starts the live updater for the current store. Any earlier one retires itself.
    fn spawn_live(&self) {
        let epoch = self.sink.new_epoch();
        live::spawn(
            LiveCtx {
                cache: Arc::clone(&self.cache),
                generation: Arc::clone(&self.generation),
                dirty: Arc::clone(&self.cache_dirty),
                tracked: Arc::clone(&self.tracked),
                hot: Arc::clone(&self.hot),
                sink: Arc::clone(&self.sink),
                tx: self.tx.clone(),
                ctx: self.ctx.clone(),
            },
            epoch,
        );
    }

    /// The collection point for file-system events. Create the watcher with
    /// `DirWatcher::with_sink(ctx, indexer.change_sink())` to get live size updates.
    pub fn change_sink(&self) -> Arc<ChangeSink> {
        Arc::clone(&self.sink)
    }

    /// Switches to the index in `dir` (created if it doesn't exist there yet). This is what
    /// replaces the old `load(dir)` when the user picks another index folder. The old folder's
    /// file is left where it is.
    pub fn set_dir(&mut self, dir: &Path) {
        if dir == self.dir {
            return;
        }
        self.generation.fetch_add(1, Ordering::SeqCst); // stop scans that still write to the old index
        self.cache.sync(); // make the old index's last writes permanent
        let max_entries = self.cache.max_ram.load(Ordering::Relaxed);
        self.cache = open_store(dir, max_entries);
        self.dir = dir.to_path_buf();
        self.cache_dirty.store(false, Ordering::Relaxed);
        self.sink.set_ignored_dir(dir);
        self.spawn_live(); // the old updater still holds the old store; this retires it
    }

    /// Changes the RAM cache limit and trims RAM if it is now over the limit.
    pub fn set_limit_mb(&self, mb: usize, unlimited_cache: bool) {
        let n = if unlimited_cache { usize::MAX } else { entries_for_mb(mb) };
        self.cache.max_ram.store(n, Ordering::Relaxed);
        self.cache.shrink();
        eprintln!("Cache limit set to {} MB", if unlimited_cache { usize::MAX } else { mb });
    }

    /// Number of indexed folders (all of them, not just the ones currently in RAM; approximate).
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Cached size regardless of age (shown immediately; fresh value follows).
    /// A folder that is not in RAM costs one 4 KiB read from the file, so call this once per
    /// folder (when a listing loads), not for every row on every frame.
    pub fn lookup(&self, path: &Path) -> Option<u64> {
        self.cache.get(path).map(|c| c.size)
    }

    /// Drop the path and every ancestor: their totals are no longer correct.
    pub fn invalidate(&self, path: &Path) {
        self.cache.remove_with_ancestors(path);
        self.cache_dirty.store(true, Ordering::Relaxed);
        self.kick_flush();
    }

    /// Drop `dir`, everything below it and all its ancestors (used when too many changes happened
    /// to list them individually).
    pub fn invalidate_tree(&self, dir: &Path) {
        self.cache.remove_tree(dir);
        self.cache_dirty.store(true, Ordering::Relaxed);
        self.kick_flush();
    }

    /// Writes the buffered changes in a background thread once enough have piled up. (Scan
    /// threads do this themselves; this covers invalidations coming from the UI thread.)
    fn kick_flush(&self) {
        if self.cache.pending_len() >= store::FLUSH_AT && !self.cache.bg_flush.swap(true, Ordering::SeqCst) {
            let s = Arc::clone(&self.cache);
            std::thread::spawn(move || {
                s.flush();
                s.bg_flush.store(false, Ordering::SeqCst);
            });
        }
    }

    /// True while a scan started by `start()` still has a live worker thread.
    pub fn is_running(&self) -> bool {
        self.live.load(Ordering::SeqCst) > 0
    }

    /// Forgets everything: stops a running scan and empties the index (RAM and file).
    /// (The file stays open, so it is reset rather than deleted.)
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
    /// `folders` also become the set the live updater keeps correct while files change.
    pub fn start(&self, folders: Vec<PathBuf>) {
        let my_gen = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        *self.tracked.lock().unwrap_or_else(|e| e.into_inner()) = Arc::new(folders.iter().cloned().collect());
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
            let hot = Arc::clone(&self.hot);
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
                        None => match scan(&folder, stamp, &cache, &generation, my_gen, &dirty, &hot) {
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

    /// Results are buffered in RAM and written in batches while scanning; this writes what is
    /// left and makes it permanent (fsync), in a background thread unless `wait` is set
    /// (use that on exit).
    pub fn save_if_dirty(&self, _dir: &Path, wait: bool) {
        if !self.cache_dirty.swap(false, Ordering::Relaxed) {
            return;
        }
        let store = Arc::clone(&self.cache);
        if wait {
            store.sync();
        } else {
            std::thread::spawn(move || store.sync());
        }
    }
}

impl Drop for Indexer {
    fn drop(&mut self) {
        self.generation.fetch_add(1, Ordering::SeqCst); // cancel scans
        self.sink.new_epoch(); // retire the live updater
    }
}
