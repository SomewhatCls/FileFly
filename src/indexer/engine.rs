//! `Indexer`: the object the rest of the app talks to. It owns the store, the scan workers and the
//! live updater, and keeps the public API of the old single-file indexer.

use super::{
    hot::HotFiles,
    live::{self, ChangeSink, LiveCtx},
    model::{is_fresh, now_secs, stamp_of, Cancel, IndexMsg, MutexExt, PRUNE_SECS, WORKERS},
    scan::{scan_many, RootSpec},
    store::{self, Cache, Store},
};
use crate::waker::Waker;
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::Sender,
        Arc, Mutex,
    },
    time::{Duration, SystemTime},
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
fn open_store(dir: &Path, sink: &Arc<ChangeSink>, hot: &HotFiles) -> Cache {
    for old in OLD_FILES {
        let _ = fs::remove_file(dir.join(old));
    }
    let store = Arc::new(Store::open(dir));
    store::spawn_flusher(&store);
    // The index file is open for as long as we run, so its listed size is never refreshed: always
    // read it exactly, and when it changes size, invalidate the folders above it right away.
    hot.pin(&dir.join(store::DB_FILE));
    let s = Arc::clone(sink);
    store.set_on_resize(move || s.index_resized());
    // Read the index file once, sequentially: that fills the system's file cache and the store's
    // lookup filter, after which a folder that was never stored costs no disk read in a scan.
    let warmer = Arc::clone(&store);
    std::thread::spawn(move || {
        super::priority::lower_priority();
        warmer.warm();
    });
    let pruner = Arc::clone(&store);
    std::thread::spawn(move || pruner.prune(now_secs().saturating_sub(PRUNE_SECS)));
    store
}

pub struct Indexer {
    cache: Cache,
    /// Folder the index file lives in.
    dir: PathBuf,
    generation: Arc<AtomicU64>,
    /// Whether the app window has focus (see `set_focused`).
    focused: Arc<AtomicBool>,
    live: Arc<AtomicUsize>,
    tx: Sender<IndexMsg>,
    waker: Waker,
    /// Recently changed files (exact sizes, see `hot.rs`).
    hot: Arc<HotFiles>,
    /// Where the file watcher reports changes (see `live.rs`).
    sink: Arc<ChangeSink>,
    /// The folders of the current listing; the live updater keeps these up to date.
    tracked: Arc<Mutex<Arc<HashSet<PathBuf>>>>,
}

impl Indexer {
    /// `dir` is where the index file lives.
    pub fn new(waker: Waker, tx: Sender<IndexMsg>, dir: &Path) -> Self {
        let sink = ChangeSink::new();
        sink.set_ignored_dir(dir);
        let hot = Arc::new(HotFiles::default());
        let ix = Self {
            cache: open_store(dir, &sink, &hot),
            dir: dir.to_path_buf(),
            generation: Arc::new(AtomicU64::new(0)),
            focused: Arc::new(AtomicBool::new(true)),
            live: Arc::new(AtomicUsize::new(0)),
            tx,
            waker,
            hot,
            sink,
            tracked: Arc::new(Mutex::new(Arc::new(HashSet::new()))),
        };
        ix.invalidate_above_index();
        ix.spawn_live();
        ix
    }

    /// The folders above the index file were summed when the file may have had another size (it only
    /// changes size while the app runs, and a change right before exit may not have been processed),
    /// so they are dropped once whenever an index is opened. Later size changes are reported by
    /// the store itself (see `open_store`).
    fn invalidate_above_index(&self) {
        self.cache.remove_with_ancestors(&self.dir.join(store::DB_FILE));
    }

    /// (Re)starts the live updater for the current store. Any earlier one retires itself.
    fn spawn_live(&self) {
        let epoch = self.sink.new_epoch();
        live::spawn(
            LiveCtx {
                cache: Arc::clone(&self.cache),
                generation: Arc::clone(&self.generation),
                tracked: Arc::clone(&self.tracked),
                hot: Arc::clone(&self.hot),
                sink: Arc::clone(&self.sink),
                tx: self.tx.clone(),
                waker: self.waker.clone(),
            },
            epoch,
        );
    }

    /// The collection point for file-system events. Create the watcher with
    /// `DirWatcher::new(waker, indexer.change_sink())` to get live size updates.
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
        self.cache = open_store(dir, &self.sink, &self.hot);
        self.dir = dir.to_path_buf();
        self.sink.set_ignored_dir(dir);
        self.invalidate_above_index();
        self.spawn_live(); // the old updater still holds the old store; this retires it
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

    /// `lookup`, plus whether the entry is up to date right now: the same test a listing applies
    /// before it trusts a cached size (no rescan needed, so no spinner). Costs one `stat`.
    pub fn lookup_fresh(&self, path: &Path) -> Option<(u64, bool)> {
        let c = self.cache.get(path)?;
        let stamp = fs::metadata(path).and_then(|m| m.modified()).map(stamp_of).unwrap_or(0);
        Some((c.size, is_fresh(&c, stamp, now_secs(), self.sink.watched_since(path))))
    }

    /// Drop the path and every ancestor: their totals are no longer correct.
    pub fn invalidate(&self, path: &Path) {
        self.cache.remove_with_ancestors(path);
    }

    /// Drop `dir`, everything below it and all its ancestors (used when too many changes happened
    /// to list them individually).
    pub fn invalidate_tree(&self, dir: &Path) {
        self.cache.remove_tree(dir);
    }

    /// Tells the indexer whether the window has focus. Rescans that are only due because the
    /// entry outlived its TTL wait for focus, so they never start while the user is elsewhere.
    pub fn set_focused(&self, focused: bool) {
        self.focused.store(focused, Ordering::Relaxed);
    }

    /// True while a scan started by `start()` still has a live worker thread.
    pub fn is_running(&self) -> bool {
        self.live.load(Ordering::SeqCst) > 0
    }

    /// Forgets everything: stops a running scan and empties the index (RAM and file).
    /// (The file stays open, so it is reset rather than deleted.)
    pub fn clear(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.cache.clear();
    }

    pub fn is_dirty(&self) -> bool {
        self.cache.dirty.load(Ordering::Relaxed)
    }

    /// Cancels any running scan and starts a new one for `folders`. Up-to-date folders return instantly.
    /// `folders` also become the set the live updater keeps correct while files change. Each comes with
    /// its last-modified time (`Metadata::modified`) where the caller already has it from its listing,
    /// which saves a `stat` per folder before the scan can begin.
    pub fn start_with(&self, folders: Vec<(PathBuf, Option<SystemTime>)>) {
        let my_gen = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        *self.tracked.lk() = Arc::new(folders.iter().map(|(p, _)| p.clone()).collect());
        if folders.is_empty() {
            return;
        }
        // One shared walk: all threads work on all listed folders at once, and on the sub-folders
        // of each, so a single huge folder (Windows, AppData, ...) is spread over them too.
        let cache = Arc::clone(&self.cache);
        let cancel = Cancel::new(&self.generation, my_gen);
        let hot = Arc::clone(&self.hot);
        let focused = Arc::clone(&self.focused);
        let sink = Arc::clone(&self.sink);
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        self.live.fetch_add(1, Ordering::SeqCst);
        let guard = Live(Arc::clone(&self.live));
        std::thread::spawn(move || {
            let _guard = guard;
            let now = now_secs();
            // False = the UI is gone.
            let send = |msg: IndexMsg| {
                let alive = tx.send(msg).is_ok();
                if alive {
                    waker.wake();
                }
                alive
            };
            // Up-to-date folders are answered right away, the rest goes into the walk.
            let mut todo = Vec::new();
            // Unchanged folders whose entry merely aged past the TTL: they keep showing their size
            // and are re-scanned once the window has focus.
            let mut aged = Vec::new();
            for (folder, mtime) in folders {
                if cancel.is_set() {
                    return;
                }
                let stamp = mtime
                    .or_else(|| fs::metadata(&folder).and_then(|m| m.modified()).ok())
                    .map(stamp_of)
                    .unwrap_or(0);
                let watched_since = sink.watched_since(&folder);
                let alive = match cache.get(&folder) {
                    Some(c) if is_fresh(&c, stamp, now, watched_since) => send(IndexMsg::Size(folder, c.size)),
                    Some(c) if c.stamp == stamp => {
                        let alive = send(IndexMsg::Size(folder.clone(), c.size));
                        aged.push(RootSpec { path: folder, stamp, watched_since });
                        alive
                    }
                    // Not up to date: the row keeps the old size, with a spinner, until the walk
                    // has the new one.
                    Some(c) => {
                        let alive = send(IndexMsg::Partial(folder.clone(), c.size));
                        todo.push(RootSpec { path: folder, stamp, watched_since });
                        alive
                    }
                    None => {
                        todo.push(RootSpec { path: folder, stamp, watched_since });
                        true
                    }
                };
                if !alive {
                    return;
                }
            }
            // live running totals for the rows while the walk is still going
            let progress = |p: &Path, n: u64| {
                send(IndexMsg::Partial(p.to_path_buf(), n));
            };
            let done = |p: &Path, n: u64| {
                send(IndexMsg::Size(p.to_path_buf(), n));
            };
            if !todo.is_empty() {
                scan_many(todo, &cache, &cancel, &hot, WORKERS, &progress, &done);
            }
            if aged.is_empty() {
                return;
            }
            // Once started a scan may run in the background; only the start waits for focus.
            while !focused.load(Ordering::Relaxed) {
                if cancel.is_set() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            // The watches may have changed while this waited for focus.
            for r in &mut aged {
                r.watched_since = sink.watched_since(&r.path);
            }
            scan_many(aged, &cache, &cancel, &hot, WORKERS, &progress, &done);
        });
    }

    // ---------------------------------------------------------------- persistence

    /// Results are buffered in RAM and written in batches while scanning; this writes what is
    /// left and makes it permanent (fsync), in a background thread unless `wait` is set
    /// (use that on exit).
    pub fn save_if_dirty(&self, wait: bool) {
        if !self.cache.dirty.swap(false, Ordering::Relaxed) {
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
