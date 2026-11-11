//! The folder walk: sums file sizes, reuses fresh cached sub-folders, stores what it finishes.

use super::{
    hot::{same_name, HotFiles},
    model::{is_fresh, ns, now_secs, CachedFolder},
    priority::lower_priority,
    store::Cache,
};
use std::{
    collections::VecDeque,
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Condvar, Mutex,
    },
    time::Duration,
};
use std::time::Instant;

struct Frame {
    path: PathBuf,
    rd: fs::ReadDir,
    total: u64,
    stamp: u64,
    /// Name of a file in this folder that must be read exactly (the index file), if any.
    exact: Option<OsString>,
}

enum Step {
    Add(u64),
    Descend(PathBuf, u64),
    Skip,
    End,
}

/// Size of one file. A file that changed a moment ago ("hot", see `hot.rs`) is asked directly,
/// because the size that came with the directory listing may be stale; everything else uses the
/// listing, which costs no extra system call.
fn file_len(de: &fs::DirEntry, hot: &HotFiles, pinned: Option<&OsStr>) -> u64 {
    if let Some(n) = pinned {
        if same_name(&de.file_name(), n) {
            return exact_len(de);
        }
    }
    if hot.is_active() && hot.contains(&de.path()) {
        return exact_len(de);
    }
    de.metadata().map(|m| m.len()).unwrap_or(0)
}

/// Asks the file itself (a handle-based stat); falls back to the listing if that is refused.
fn exact_len(de: &fs::DirEntry) -> u64 {
    fs::symlink_metadata(de.path()).or_else(|_| de.metadata()).map(|m| m.len()).unwrap_or(0)
}

/// Helper function to identify the device a file is on.
#[cfg(unix)]
fn dev_of(m: &fs::Metadata) -> u64 { use std::os::unix::fs::MetadataExt; m.dev() }
#[cfg(not(unix))]
fn dev_of(_: &fs::Metadata) -> u64 { 0 }

/// `scan_with_progress` without progress reports.
pub fn scan(
    root: &Path,
    root_stamp: u64,
    cache: &Cache,
    generation: &AtomicU64,
    my_gen: u64,
    dirty: &AtomicBool,
    hot: &HotFiles,
) -> Option<u64> {
    scan_with_progress(root, root_stamp, cache, generation, my_gen, dirty, hot, &|_| {})
}

/// Iterative post-order walk. Returns None if cancelled. Every folder that is finished is stored.
/// `progress` receives the running total under `root` about ten times a second while walking.
pub fn scan_with_progress(
    root: &Path,
    root_stamp: u64,
    cache: &Cache,
    generation: &AtomicU64,
    my_gen: u64,
    dirty: &AtomicBool,
    hot: &HotFiles,
    progress: &dyn Fn(u64),
) -> Option<u64> {
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
    let mut stack = vec![Frame { path: root.to_path_buf(), rd, total: 0, stamp: root_stamp, exact: hot.pinned_name_in(root) }];
    let mut counter = 0u32;
    let root_dev = fs::metadata(root).map(|m| dev_of(&m)).unwrap_or(0);
    // Time since last scan pause to limit system impact.
    let mut scan_slice = Instant::now();
    // Everything counted so far under `root` (for the live display) and when it was last reported.
    let mut running = 0u64;
    let mut last_report = Instant::now();

    loop {
        if generation.load(Ordering::Relaxed) != my_gen {
            flush(&mut pending); // keep the completed sub-folders
            return None;
        }

        // Limiting the system load with intermittent sleeps.
        if counter % 4096 == 0 && scan_slice.elapsed() >= Duration::from_millis(8) {
            std::thread::sleep(Duration::from_millis(4));
            scan_slice = Instant::now();
            if last_report.elapsed() >= Duration::from_millis(100) {
                progress(running);
                last_report = Instant::now();
            }
            if pending.len() >= 4000 {
                flush(&mut pending);
            }
        }

        let step = {
            let top = stack.last_mut()?;
            match top.rd.next() {
                None => Step::End,
                Some(Err(_)) => Step::Skip,
                Some(Ok(de)) => match de.file_type() {
                    Ok(ft) if ft.is_symlink() => Step::Skip, // symlinks & junctions: avoid loops/double counting
                    Ok(ft) if ft.is_file() => Step::Add(file_len(&de, hot, top.exact.as_deref())),
                    Ok(ft) if ft.is_dir() => {
                        let md = de.metadata().ok();
                        let path = de.path();
                        if super::live::is_pseudo(&path)
                            || md.as_ref().is_some_and(|m| dev_of(m) != root_dev)
                        {
                            Step::Skip
                        } else {
                            let stamp = md.and_then(|m| m.modified().ok()).map(ns).unwrap_or(0);
                            Step::Descend(path, stamp)
                        }
                    }
                    _ => Step::Skip,
                },
            }
        };

        match step {
            Step::Skip => {}
            Step::Add(n) => {
                running = running.saturating_add(n);
                if let Some(top) = stack.last_mut() {
                    top.total = top.total.saturating_add(n);
                }
            }
            Step::Descend(path, stamp) => {
                let hit = cache.get(&path).filter(|c| is_fresh(c, stamp, now));
                if let Some(hit) = hit {
                    running = running.saturating_add(hit.size);
                    if let Some(top) = stack.last_mut() {
                        top.total = top.total.saturating_add(hit.size);
                    }
                } else if let Ok(rd) = fs::read_dir(&path) {
                    let exact = hot.pinned_name_in(&path);
                    stack.push(Frame { path, rd, total: 0, stamp, exact });
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

// ----------------------------------------------------------------------------------------------
// Parallel walk: many threads, one shared queue of folders, several trees at once

/// One tree that is being summed.
struct Root {
    path: PathBuf,
    dev: u64,
    /// Everything counted so far under this root (for the live display).
    running: AtomicU64,
    /// Milliseconds after the start of the walk at which `running` was last reported.
    last_report: AtomicU64,
}

/// One folder of the walk. A folder is finished when its own listing is done *and* all its
/// sub-folders are finished; whoever finishes it last adds its total to the parent.
struct Node {
    path: PathBuf,
    stamp: u64,
    parent: Option<Arc<Node>>,
    root: Arc<Root>,
    /// 1 for the folder's own listing + 1 per sub-folder that is not finished yet.
    pending: AtomicUsize,
    total: AtomicU64,
    /// The folder could not be listed: its total is passed up but not stored in the index.
    failed: AtomicBool,
}

struct Walk<'a> {
    queue: Mutex<VecDeque<Arc<Node>>>,
    wake: Condvar,
    /// Folders that are queued or being listed right now. 0 = nothing left to do.
    open: AtomicUsize,
    stop: AtomicBool,
    cache: &'a Cache,
    generation: &'a AtomicU64,
    my_gen: u64,
    dirty: &'a AtomicBool,
    hot: &'a HotFiles,
    now: u64,
    began: Instant,
    progress: &'a (dyn Fn(&Path, u64) + Sync),
    done: &'a (dyn Fn(&Path, u64) + Sync),
}

type Finished = Vec<(PathBuf, CachedFolder)>;

impl Walk<'_> {
    fn cancelled(&self) -> bool {
        self.stop.load(Ordering::Relaxed) || self.generation.load(Ordering::Relaxed) != self.my_gen
    }

    fn push(&self, n: Arc<Node>) {
        self.open.fetch_add(1, Ordering::SeqCst);
        self.queue.lock().unwrap_or_else(|e| e.into_inner()).push_back(n);
        self.wake.notify_one();
    }

    /// The listing of one folder is over (its sub-folders were queued before this is called).
    fn close(&self) {
        if self.open.fetch_sub(1, Ordering::SeqCst) == 1 {
            // Last one: wake the idle workers so they can leave.
            let _g = self.queue.lock().unwrap_or_else(|e| e.into_inner());
            self.wake.notify_all();
        }
    }

    /// Next folder to list. None = everything is done or the walk was cancelled.
    /// (Breadth first, so every listed root makes progress and small ones finish early.)
    fn next(&self) -> Option<Arc<Node>> {
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if self.cancelled() {
                self.stop.store(true, Ordering::Relaxed);
                self.wake.notify_all();
                return None;
            }
            if let Some(n) = q.pop_front() {
                return Some(n);
            }
            if self.open.load(Ordering::SeqCst) == 0 {
                return None;
            }
            q = match self.wake.wait_timeout(q, Duration::from_millis(50)) {
                Ok((g, _)) => g,
                Err(e) => e.into_inner().0,
            };
        }
    }

    /// Adds to the running total of a root and reports it now and then (about ten times a second).
    fn bump(&self, root: &Root, delta: u64) {
        if delta != 0 {
            root.running.fetch_add(delta, Ordering::Relaxed);
        }
        let ms = self.began.elapsed().as_millis() as u64;
        let last = root.last_report.load(Ordering::Relaxed);
        if ms >= last + 100
            && root.last_report.compare_exchange(last, ms, Ordering::Relaxed, Ordering::Relaxed).is_ok()
        {
            (self.progress)(&root.path, root.running.load(Ordering::Relaxed));
        }
    }

    fn store(&self, finished: &mut Finished) {
        if finished.is_empty() {
            return;
        }
        self.cache.put_many(std::mem::take(finished));
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Marks the listing of `node` as done and walks up the tree while that finishes folders.
    fn finish(&self, mut node: Arc<Node>, finished: &mut Finished) {
        loop {
            if node.pending.fetch_sub(1, Ordering::SeqCst) != 1 {
                return;
            }
            let total = node.total.load(Ordering::SeqCst);
            if !node.failed.load(Ordering::Relaxed) {
                finished.push((node.path.clone(), CachedFolder { size: total, modified_ns: node.stamp, scanned_at: self.now }));
            }
            match node.parent.clone() {
                Some(p) => {
                    p.total.fetch_add(total, Ordering::SeqCst);
                    node = p;
                }
                None => {
                    self.store(finished); // the root is in the index before it is announced
                    (self.done)(&node.path, total);
                    return;
                }
            }
        }
    }

    /// Lists one folder: sums its files, takes fresh sub-folders from the index and queues the
    /// others. False = cancelled half-way (this folder is then not stored).
    fn list(&self, node: &Arc<Node>, finished: &mut Finished) -> bool {
        let Ok(rd) = fs::read_dir(&node.path) else {
            node.failed.store(true, Ordering::Relaxed);
            self.finish(Arc::clone(node), finished);
            return true;
        };
        let exact = self.hot.pinned_name_in(&node.path);
        let mut sum = 0u64; // files and fresh cached sub-folders of this folder
        let mut unreported = 0u64;
        let mut seen = 0u32;
        for entry in rd {
            seen = seen.wrapping_add(1);
            if seen % 1024 == 0 {
                if self.cancelled() {
                    return false;
                }
                if seen % 4096 == 0 {
                    self.bump(&node.root, std::mem::take(&mut unreported));
                }
            }
            let Ok(de) = entry else { continue };
            let Ok(ft) = de.file_type() else { continue };
            if ft.is_symlink() {
                continue; // symlinks & junctions: avoid loops/double counting
            }
            if ft.is_file() {
                let n = file_len(&de, self.hot, exact.as_deref());
                sum = sum.saturating_add(n);
                unreported = unreported.saturating_add(n);
            } else if ft.is_dir() {
                let md = de.metadata().ok();
                let path = de.path();
                if super::live::is_pseudo(&path) || md.as_ref().is_some_and(|m| dev_of(m) != node.root.dev) {
                    continue;
                }
                let stamp = md.and_then(|m| m.modified().ok()).map(ns).unwrap_or(0);
                match self.cache.get(&path).filter(|c| is_fresh(c, stamp, self.now)) {
                    Some(hit) => {
                        sum = sum.saturating_add(hit.size);
                        unreported = unreported.saturating_add(hit.size);
                    }
                    None => {
                        node.pending.fetch_add(1, Ordering::SeqCst);
                        self.push(Arc::new(Node {
                            path,
                            stamp,
                            parent: Some(Arc::clone(node)),
                            root: Arc::clone(&node.root),
                            pending: AtomicUsize::new(1),
                            total: AtomicU64::new(0),
                            failed: AtomicBool::new(false),
                        }));
                    }
                }
            }
        }
        node.total.fetch_add(sum, Ordering::SeqCst);
        self.bump(&node.root, unreported);
        self.finish(Arc::clone(node), finished);
        true
    }
}

fn worker(w: &Walk<'_>) {
    lower_priority();
    let mut finished: Finished = Vec::new();
    while let Some(node) = w.next() {
        if !w.list(&node, &mut finished) {
            w.stop.store(true, Ordering::Relaxed); // cancelled
            w.wake.notify_all();
            break;
        }
        w.close();
        if finished.len() >= 4000 {
            w.store(&mut finished);
        }
    }
    w.store(&mut finished); // keep the completed folders, also after a cancel
}

/// Walks all `roots` ((folder, mtime fingerprint) pairs) at once with `threads` threads that
/// share one queue of folders, so even a single huge tree is spread over all of them. Every
/// finished folder is stored in the index. `progress(root, bytes_so_far)` is called about ten
/// times a second per root while it is being walked; `done(root, size)` once when it is complete.
/// Returns when everything is done or the walk was cancelled (`generation` changed).
pub fn scan_many(
    roots: Vec<(PathBuf, u64)>,
    cache: &Cache,
    generation: &AtomicU64,
    my_gen: u64,
    dirty: &AtomicBool,
    hot: &HotFiles,
    threads: usize,
    progress: &(dyn Fn(&Path, u64) + Sync),
    done: &(dyn Fn(&Path, u64) + Sync),
) {
    if roots.is_empty() {
        return;
    }
    let count = roots.len();
    let walk = Walk {
        queue: Mutex::new(VecDeque::new()),
        wake: Condvar::new(),
        open: AtomicUsize::new(0),
        stop: AtomicBool::new(false),
        cache,
        generation,
        my_gen,
        dirty,
        hot,
        now: now_secs(),
        began: Instant::now(),
        progress,
        done,
    };
    for (path, stamp) in roots {
        let dev = fs::metadata(&path).map(|m| dev_of(&m)).unwrap_or(0);
        let root = Arc::new(Root { path: path.clone(), dev, running: AtomicU64::new(0), last_report: AtomicU64::new(0) });
        walk.push(Arc::new(Node {
            path,
            stamp,
            parent: None,
            root,
            pending: AtomicUsize::new(1),
            total: AtomicU64::new(0),
            failed: AtomicBool::new(false),
        }));
    }
    std::thread::scope(|s| {
        for _ in 0..threads.max(1) {
            s.spawn(|| worker(&walk));
        }
    });
    if cfg!(debug_assertions) {
        eprintln!("scan_many: {count} folder(s), {} thread(s), {:?}", threads.max(1), walk.began.elapsed());
    }
}
