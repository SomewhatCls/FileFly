//! Live updater: keeps folder sizes correct while files change.
//!
//! ```text
//!  file watcher ──paths──▶ ChangeSink ──(400 ms debounce)──▶ updater thread
//!                                                               │ 1. stat the changed files, exact sizes (hot.rs)
//!                                                               │ 2. drop the changed path and all ancestors from the index
//!                                                               │ 3. rescan only the listed folders that contain a change
//!                                                               ▼
//!                                                       IndexMsg::Size ──▶ UI
//! ```
//!
//! Cost control, so that it never slows the machine down:
//!  * events are coalesced in a set and handled in one batch per 400 ms;
//!  * a rescan only walks the folders *on the path* to the change, everything else is a cache hit;
//!  * the same folder is rescanned at most every 0.75 s (8 s while a flood of events is going on);
//!  * a flood (more than `STAT_LIMIT` paths in one batch) is not stat'ed file by file, it is
//!    handled per parent folder, and a watcher overflow becomes one rescan of the listing;
//!  * files of the index itself and Windows system noise (recycle bin, pagefile, ...) are ignored;
//!  * the thread runs at below-normal priority and sleeps when nothing happens.

use super::{
    hot::HotFiles,
    model::{ns, IndexMsg},
    priority::lower_priority,
    scan::scan,
    store::Cache,
};
use eframe::egui;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::Sender,
        Arc, Condvar, Mutex, MutexGuard,
    },
    time::{Duration, Instant},
};

/// Distinct changed paths remembered between two batches; more than that becomes "overflow".
const MAX_PATHS: usize = 4096;
/// Distinct subtree invalidations remembered between two batches.
const MAX_TREES: usize = 64;
/// Events are collected this long before a batch is processed.
const DEBOUNCE: Duration = Duration::from_millis(400);
/// Sleep time when nothing is hot and nothing waits for a rescan.
const IDLE_WAIT: Duration = Duration::from_secs(30);
/// Minimum time between two rescans of the same listed folder.
const MIN_GAP: Duration = Duration::from_millis(750);
/// Same, while a flood of events is going on.
const FLOOD_GAP: Duration = Duration::from_secs(8);
/// Batches with more paths than this are not stat'ed one by one.
const STAT_LIMIT: usize = 256;
/// Names (any path component, case-insensitive) that only ever produce noise.
const NOISY: [&str; 7] = [
    "$recycle.bin",
    "system volume information",
    "$sysreset",
    "$windows.~bt",
    "pagefile.sys",
    "hiberfil.sys",
    "swapfile.sys",
];

/// True for names that are never worth reacting to (also used to pick folders to watch).
pub fn is_noisy(p: &Path) -> bool {
    p.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        NOISY.iter().any(|n| s.eq_ignore_ascii_case(n))
    })
}

// ----------------------------------------------------------------------------------------------
// ChangeSink: where the watcher drops what it sees

#[derive(Default)]
pub struct Batch {
    paths: Vec<PathBuf>,
    trees: Vec<PathBuf>,
    overflow: bool,
}

#[derive(Default)]
struct SinkState {
    paths: HashSet<PathBuf>,
    trees: HashSet<PathBuf>,
    overflow: bool,
}

impl SinkState {
    fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.trees.is_empty() && !self.overflow
    }
}

/// Thread-safe collection point between the file watcher (producer) and the updater (consumer).
/// Get it from `Indexer::change_sink()` and hand it to `DirWatcher::with_sink`.
pub struct ChangeSink {
    state: Mutex<SinkState>,
    cv: Condvar,
    /// The folder of the index file: its own writes must not count as changes.
    index_dir: Mutex<Option<PathBuf>>,
    /// Bumped to retire an updater thread (new index folder, shutdown).
    epoch: AtomicU64,
}

impl ChangeSink {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(SinkState::default()),
            cv: Condvar::new(),
            index_dir: Mutex::new(None),
            epoch: AtomicU64::new(0),
        })
    }

    fn state(&self) -> MutexGuard<'_, SinkState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_ignored_dir(&self, dir: &Path) {
        *self.index_dir.lock().unwrap_or_else(|e| e.into_inner()) = Some(dir.to_path_buf());
    }

    /// False for paths that must not be reported (index file, system noise).
    pub fn accepts(&self, p: &Path) -> bool {
        if is_noisy(p) {
            return false;
        }
        match &*self.index_dir.lock().unwrap_or_else(|e| e.into_inner()) {
            Some(d) => !p.starts_with(d),
            None => true,
        }
    }

    /// Files or folders that changed (already filtered with `accepts`).
    pub fn push(&self, paths: impl IntoIterator<Item = PathBuf>) {
        let mut g = self.state();
        for p in paths {
            if g.paths.len() >= MAX_PATHS {
                g.overflow = true;
                break;
            }
            g.paths.insert(p);
        }
        drop(g);
        self.cv.notify_one();
    }

    /// Everything below `dir` may have changed (a watch was dropped, events were lost, ...).
    pub fn push_tree(&self, dir: PathBuf) {
        let mut g = self.state();
        if g.trees.len() < MAX_TREES {
            g.trees.insert(dir);
        } else {
            g.overflow = true;
        }
        drop(g);
        self.cv.notify_one();
    }

    /// The watcher lost events: any path might have changed.
    pub fn push_overflow(&self) {
        self.state().overflow = true;
        self.cv.notify_one();
    }

    /// Retires the running updater thread (if any) and returns the number for the next one.
    pub(super) fn new_epoch(&self) -> u64 {
        let mut g = self.state();
        *g = SinkState::default();
        let e = self.epoch.fetch_add(1, Ordering::SeqCst) + 1;
        drop(g);
        self.cv.notify_all();
        e
    }

    /// Waits for changes, at most `timeout`. After the first change it waits `DEBOUNCE` more so
    /// that a burst arrives as one batch. An empty batch means "timed out". None = retire.
    fn wait(&self, epoch: u64, timeout: Duration) -> Option<Batch> {
        let deadline = Instant::now() + timeout;
        let mut g = self.state();
        loop {
            if self.epoch.load(Ordering::SeqCst) != epoch {
                return None;
            }
            if !g.is_empty() {
                break;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Some(Batch::default());
            }
            g = match self.cv.wait_timeout(g, left) {
                Ok((g, _)) => g,
                Err(e) => e.into_inner().0,
            };
        }
        drop(g);
        std::thread::sleep(DEBOUNCE);
        if self.epoch.load(Ordering::SeqCst) != epoch {
            return None;
        }
        let s = std::mem::take(&mut *self.state());
        Some(Batch { paths: s.paths.into_iter().collect(), trees: s.trees.into_iter().collect(), overflow: s.overflow })
    }
}

// ----------------------------------------------------------------------------------------------
// The updater thread

/// Everything the updater needs, shared with the `Indexer`.
pub struct LiveCtx {
    pub cache: Cache,
    pub generation: Arc<AtomicU64>,
    pub dirty: Arc<AtomicBool>,
    /// The folders of the current listing (set by `Indexer::start`); only these are rescanned.
    pub tracked: Arc<Mutex<Arc<HashSet<PathBuf>>>>,
    pub hot: Arc<HotFiles>,
    pub sink: Arc<ChangeSink>,
    pub tx: Sender<IndexMsg>,
    pub ctx: egui::Context,
}

pub fn spawn(lc: LiveCtx, epoch: u64) {
    let _ = std::thread::Builder::new().name("index-live".into()).spawn(move || run(lc, epoch));
}

/// Exact size of a file (asks the file itself, not the directory listing), None if it is gone.
fn file_size(p: &Path) -> Option<u64> {
    fs::symlink_metadata(p).ok().filter(|m| m.is_file()).map(|m| m.len())
}

/// Looks at one reported path and says whether the totals above it need to be recomputed.
fn classify(hot: &HotFiles, p: PathBuf, now: Instant, out: &mut Vec<PathBuf>) {
    match fs::symlink_metadata(&p) {
        Ok(m) if m.is_file() => {
            // Same size as last time = an attribute-only event: nothing to do.
            if hot.touch(&p, m.len(), now) {
                out.push(p);
            }
        }
        Ok(m) if m.file_type().is_symlink() => {} // the scan skips links, so they never count
        Ok(_) => out.push(p),                     // a folder was created, renamed, changed
        Err(_) => {
            hot.remove(&p); // deleted or renamed away
            out.push(p);
        }
    }
}

/// The folder the listing shows (the parent of the listed folders).
fn listing_dir(tracked: &HashSet<PathBuf>) -> Option<PathBuf> {
    tracked.iter().next().and_then(|p| p.parent()).map(Path::to_path_buf)
}

fn next_wakeup(hot: &HotFiles, queue: &HashSet<PathBuf>, allowed: &HashMap<PathBuf, Instant>, now: Instant) -> Duration {
    let mut t = IDLE_WAIT;
    if let Some(due) = hot.next_due() {
        t = t.min(due.saturating_duration_since(now));
    }
    for q in queue {
        let at = allowed.get(q).copied().unwrap_or(now);
        t = t.min(at.saturating_duration_since(now));
    }
    t.max(Duration::from_millis(50))
}

fn run(lc: LiveCtx, epoch: u64) {
    lower_priority();
    // Listed folders that contain a change and wait for their rescan.
    let mut queue: HashSet<PathBuf> = HashSet::new();
    // When a listed folder may be rescanned again.
    let mut allowed: HashMap<PathBuf, Instant> = HashMap::new();
    let mut gap = MIN_GAP;

    loop {
        let wait = next_wakeup(&lc.hot, &queue, &allowed, Instant::now());
        let Some(batch) = lc.sink.wait(epoch, wait) else { return };
        let now = Instant::now();
        let tracked: Arc<HashSet<PathBuf>> = lc.tracked.lock().unwrap_or_else(|e| e.into_inner()).clone();
        queue.retain(|t| tracked.contains(t));
        allowed.retain(|_, at| *at > now);

        // 1. Work out which paths changed.
        let flood = batch.overflow || batch.paths.len() > STAT_LIMIT;
        if !batch.paths.is_empty() || batch.overflow {
            gap = if flood { FLOOD_GAP } else { MIN_GAP };
        }
        let mut trees = batch.trees;
        if batch.overflow {
            if let Some(dir) = listing_dir(&tracked) {
                trees.push(dir);
            }
        }
        let mut changed: Vec<PathBuf> = Vec::new();
        if batch.paths.len() <= STAT_LIMIT {
            for p in batch.paths {
                classify(&lc.hot, p, now, &mut changed);
            }
        } else {
            // Too many to look at one by one: invalidate per parent folder.
            let parents: HashSet<PathBuf> = batch.paths.iter().filter_map(|p| p.parent().map(Path::to_path_buf)).collect();
            changed.extend(parents);
        }
        // Files that are being written without producing events.
        for p in lc.hot.due(now) {
            let size = file_size(&p);
            if lc.hot.record(&p, size, now) {
                changed.push(p);
            }
        }

        // 2. Invalidate them and everything above, and note which listed folders are affected.
        if !changed.is_empty() || !trees.is_empty() {
            for p in &changed {
                lc.cache.remove_with_ancestors(p);
                for a in p.ancestors() {
                    if tracked.contains(a) {
                        queue.insert(a.to_path_buf());
                        break;
                    }
                }
            }
            for t in &trees {
                lc.cache.remove_tree(t);
                for tr in tracked.iter() {
                    if tr.starts_with(t) || t.starts_with(tr) {
                        queue.insert(tr.clone());
                    }
                }
            }
            lc.dirty.store(true, Ordering::Relaxed);
        }

        // 3. Rescan the affected listed folders whose turn has come. Only the folders on the path
        //    to the change are walked again; their untouched sub-folders are cache hits.
        let g = lc.generation.load(Ordering::Relaxed);
        let ready: Vec<PathBuf> =
            queue.iter().filter(|t| allowed.get(*t).map_or(true, |at| *at <= now)).cloned().collect();
        for t in ready {
            if lc.generation.load(Ordering::Relaxed) != g {
                break; // a new listing took over and rescans on its own
            }
            queue.remove(&t);
            let stamp = fs::metadata(&t).and_then(|m| m.modified()).map(ns).unwrap_or(0);
            match scan(&t, stamp, &lc.cache, &lc.generation, g, &lc.dirty, &lc.hot) {
                Some(size) => {
                    allowed.insert(t.clone(), Instant::now() + gap);
                    if lc.tx.send(IndexMsg::Size(t, size)).is_err() {
                        return; // the UI is gone
                    }
                    lc.ctx.request_repaint();
                }
                None => {
                    queue.insert(t);
                    break;
                }
            }
        }
    }
}
