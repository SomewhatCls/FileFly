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
        if scan_slice.elapsed() >= Duration::from_millis(8) {
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
// Parallel walk: many threads, several trees at once.
//
// Every thread walks depth first through a stack of its own and hands half of it to the shared
// queue when another thread has run out of work. Depth first keeps the directory handle of a
// folder alive just long enough for its sub-folders to be opened relative to it (Windows), which
// is much cheaper than resolving the whole path for every folder again.

/// How a folder can be opened without its full path: relative to the still open handle of its
/// parent (Windows only).
#[cfg(windows)]
type Rel = Option<(Arc<win::Dir>, Box<[u16]>)>;
#[cfg(not(windows))]
type Rel = ();

/// Folders a thread keeps for itself; beyond this the oldest ones go to the shared queue.
const LOCAL_MAX: usize = 256;

/// Scan threads run at below-normal priority so that the UI stays responsive. With false they
/// compete with other programs on equal terms.
const BACKGROUND_PRIORITY: bool = true;

/// A folder handed to a queue shorter than this keeps its parent's handle, so that it can still be
/// opened relative to it. In a longer queue the handles would pile up, so they are released.
const QUEUE_KEEP_HANDLES: usize = 4096;

/// One tree that is being summed.
struct Root {
    path: PathBuf,
    #[cfg_attr(windows, allow(dead_code))]
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
    /// Parent handle and own name. Taken (and so released) when the folder is opened or when it
    /// is handed to the shared queue, so handles never pile up in the queue.
    rel: Mutex<Rel>,
}

impl Node {
    fn new(path: PathBuf, stamp: u64, parent: Option<Arc<Node>>, root: Arc<Root>) -> Node {
        Node {
            path,
            stamp,
            parent,
            root,
            pending: AtomicUsize::new(1),
            total: AtomicU64::new(0),
            failed: AtomicBool::new(false),
            rel: Mutex::new(Rel::default()),
        }
    }
}

/// What one worker thread keeps to itself.
struct Local {
    /// Folders waiting for this thread, the newest last (depth first).
    stack: Vec<Arc<Node>>,
    /// Read buffer of the directory calls (8-byte aligned), reused for every folder.
    #[cfg(windows)]
    buf: Vec<u64>,
}

type Finished = Vec<(PathBuf, CachedFolder)>;

/// Counters behind the timing lines that `scan_many` prints in debug builds.
#[derive(Default)]
struct Stats {
    folders: AtomicU64,
    files: AtomicU64,
    /// Thread time in nanoseconds: listing folders in total, of which index lookups and index
    /// stores, and waiting for work.
    list_ns: AtomicU64,
    get_ns: AtomicU64,
    store_ns: AtomicU64,
    idle_ns: AtomicU64,
    peak_queue: AtomicUsize,
    /// Folders handed from a thread's own stack to the shared queue.
    donated: AtomicU64,
    /// How folders were opened (Windows): relative to the parent, by path, or with the fallback.
    rel_opens: AtomicU64,
    path_opens: AtomicU64,
    fallbacks: AtomicU64,
    /// CPU time of the worker threads (Windows only), split into kernel and user mode.
    kernel_ns: AtomicU64,
    user_ns: AtomicU64,
    /// Time inside the Windows listing calls: open + first read, further reads, close, and the
    /// per-entry callback (which contains the index lookups).
    win_first_ns: AtomicU64,
    win_next_ns: AtomicU64,
    win_close_ns: AtomicU64,
    win_cb_ns: AtomicU64,
}

fn since(t: Instant) -> u64 {
    t.elapsed().as_nanos() as u64
}

struct Walk<'a> {
    queue: Mutex<VecDeque<Arc<Node>>>,
    wake: Condvar,
    /// Folders that exist but are not finished listing (queued, on a thread's stack, or being
    /// listed right now). 0 = nothing left to do.
    open: AtomicUsize,
    /// Threads that wait for the shared queue to get work.
    waiting: AtomicUsize,
    stop: AtomicBool,
    /// Wakes the thread controller (see `scan_many`) as soon as the walk is over.
    ctl: Condvar,
    ctl_lock: Mutex<()>,
    /// Worker threads started so far (the controller adds more while threads mostly wait).
    threads: AtomicUsize,
    cache: &'a Cache,
    generation: &'a AtomicU64,
    my_gen: u64,
    dirty: &'a AtomicBool,
    hot: &'a HotFiles,
    now: u64,
    began: Instant,
    stats: Stats,
    progress: &'a (dyn Fn(&Path, u64) + Sync),
    done: &'a (dyn Fn(&Path, u64) + Sync),
}

impl Walk<'_> {
    fn cancelled(&self) -> bool {
        self.stop.load(Ordering::Relaxed) || self.generation.load(Ordering::Relaxed) != self.my_gen
    }

    /// Puts a root into the shared queue.
    fn push(&self, n: Arc<Node>) {
        self.open.fetch_add(1, Ordering::SeqCst);
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.push_back(n);
        self.stats.peak_queue.fetch_max(q.len(), Ordering::Relaxed);
        drop(q);
        self.wake.notify_one();
    }

    /// The listing of one folder is over (its sub-folders already exist when this is called).
    fn close(&self) {
        if self.open.fetch_sub(1, Ordering::SeqCst) == 1 {
            // Last one: wake the idle workers so they can leave.
            let _g = self.queue.lock().unwrap_or_else(|e| e.into_inner());
            self.wake.notify_all();
            drop(_g);
            let _c = self.ctl_lock.lock().unwrap_or_else(|e| e.into_inner());
            self.ctl.notify_all();
        }
    }

    /// Next folder from the shared queue. None = everything is done or the walk was cancelled.
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
            self.waiting.fetch_add(1, Ordering::Relaxed);
            let idle = Instant::now();
            q = match self.wake.wait_timeout(q, Duration::from_millis(50)) {
                Ok((g, _)) => g,
                Err(e) => e.into_inner().0,
            };
            self.stats.idle_ns.fetch_add(since(idle), Ordering::Relaxed);
            self.waiting.fetch_sub(1, Ordering::Relaxed);
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
        let t = Instant::now();
        self.cache.put_many(std::mem::take(finished));
        self.stats.store_ns.fetch_add(since(t), Ordering::Relaxed);
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

    /// A sub-folder: taken from the index if it is up to date, otherwise it becomes a new node
    /// in `spawned`.
    fn sub_folder(
        &self,
        node: &Arc<Node>,
        path: PathBuf,
        stamp: u64,
        rel: Rel,
        sum: &mut u64,
        unreported: &mut u64,
        spawned: &mut Vec<Arc<Node>>,
    ) {
        let t = Instant::now();
        let hit = self.cache.get(&path).filter(|c| is_fresh(c, stamp, self.now));
        self.stats.get_ns.fetch_add(since(t), Ordering::Relaxed);
        match hit {
            Some(hit) => {
                *sum = sum.saturating_add(hit.size);
                *unreported = unreported.saturating_add(hit.size);
            }
            None => {
                node.pending.fetch_add(1, Ordering::SeqCst);
                self.open.fetch_add(1, Ordering::SeqCst);
                let mut child = Node::new(path, stamp, Some(Arc::clone(node)), Arc::clone(&node.root));
                *child.rel.get_mut().unwrap_or_else(|e| e.into_inner()) = rel;
                spawned.push(Arc::new(child));
            }
        }
    }

    /// Takes the sub-folders found by a listing onto the thread's own stack. If other threads
    /// are out of work, or the stack grows large, the oldest folders go to the shared queue.
    fn dispatch(&self, lc: &mut Local, spawned: Vec<Arc<Node>>) {
        if spawned.is_empty() {
            return;
        }
        lc.stack.extend(spawned);
        let len = lc.stack.len();
        let idle = self.waiting.load(Ordering::Relaxed);
        let give = if idle > 0 {
            len / 2
        } else if len > LOCAL_MAX {
            len - LOCAL_MAX / 2
        } else {
            0
        };
        if give == 0 {
            return;
        }
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        for n in lc.stack.drain(..give) {
            // In a long queue a folder may wait a long time: it must not hold its parent's handle.
            if q.len() >= QUEUE_KEEP_HANDLES {
                drop(std::mem::take(&mut *n.rel.lock().unwrap_or_else(|e| e.into_inner())));
            }
            q.push_back(n);
        }
        self.stats.peak_queue.fetch_max(q.len(), Ordering::Relaxed);
        drop(q);
        self.stats.donated.fetch_add(give as u64, Ordering::Relaxed);
        self.wake.notify_all();
    }

    /// Lists one folder: sums its files, takes fresh sub-folders from the index and creates nodes
    /// for the others. False = cancelled half-way (this folder is then not stored).
    fn list(&self, node: &Arc<Node>, finished: &mut Finished, lc: &mut Local) -> bool {
        let mut sum = 0u64; // files and fresh cached sub-folders of this folder
        let mut unreported = 0u64;
        let mut spawned: Vec<Arc<Node>> = Vec::new();
        match self.read_folder(node, &mut sum, &mut unreported, lc, &mut spawned) {
            None => {
                node.failed.store(true, Ordering::Relaxed);
                self.finish(Arc::clone(node), finished);
                return true;
            }
            Some(false) => return false,
            Some(true) => {}
        }
        node.total.fetch_add(sum, Ordering::SeqCst);
        self.bump(&node.root, unreported);
        self.finish(Arc::clone(node), finished);
        self.dispatch(lc, spawned);
        true
    }

    /// The directory read behind `list`. None = the folder cannot be listed, Some(false) = cancelled.
    #[cfg(not(windows))]
    fn read_folder(
        &self,
        node: &Arc<Node>,
        sum: &mut u64,
        unreported: &mut u64,
        _lc: &mut Local,
        spawned: &mut Vec<Arc<Node>>,
    ) -> Option<bool> {
        let rd = fs::read_dir(&node.path).ok()?;
        let exact = self.hot.pinned_name_in(&node.path);
        let hot_here = if self.hot.is_active() { self.hot.names_in(&node.path) } else { Vec::new() };
        let mut seen = 0u32;
        let mut files = 0u64;
        for entry in rd {
            seen = seen.wrapping_add(1);
            if seen % 1024 == 0 {
                if self.cancelled() {
                    return Some(false);
                }
                if seen % 4096 == 0 {
                    self.bump(&node.root, std::mem::take(unreported));
                }
            }
            let Ok(de) = entry else { continue };
            let Ok(ft) = de.file_type() else { continue };
            if ft.is_symlink() {
                continue; // symlinks & junctions: avoid loops/double counting
            }
            if ft.is_file() {
                let n = file_len_in(&de, exact.as_deref(), &hot_here);
                files += 1;
                *sum = sum.saturating_add(n);
                *unreported = unreported.saturating_add(n);
            } else if ft.is_dir() {
                let md = de.metadata().ok();
                let path = de.path();
                if super::live::is_pseudo(&path) || md.as_ref().is_some_and(|m| dev_of(m) != node.root.dev) {
                    continue;
                }
                let stamp = md.and_then(|m| m.modified().ok()).map(ns).unwrap_or(0);
                self.sub_folder(node, path, stamp, (), sum, unreported, spawned);
            }
        }
        self.stats.files.fetch_add(files, Ordering::Relaxed);
        Some(true)
    }

    /// Windows: the folder is opened relative to its parent's handle (or by path), and read with
    /// `NtQueryDirectoryFile` into the thread's reusable buffer. If it cannot be opened that way,
    /// `FindFirstFileExW` is tried before giving up. Same rules as everywhere: links and junctions
    /// are skipped, sizes come from the listing, except for the pinned file and hot files, which
    /// are asked directly.
    #[cfg(windows)]
    fn read_folder(
        &self,
        node: &Arc<Node>,
        sum: &mut u64,
        unreported: &mut u64,
        lc: &mut Local,
        spawned: &mut Vec<Arc<Node>>,
    ) -> Option<bool> {
        let exact = self.hot.pinned_name_in(&node.path);
        let hot_here = if self.hot.is_active() { self.hot.names_in(&node.path) } else { Vec::new() };
        let st = &self.stats;

        let t_open = Instant::now();
        let rel = std::mem::take(&mut *node.rel.lock().unwrap_or_else(|e| e.into_inner()));
        let was_rel = rel.is_some();
        let opened = match &rel {
            Some((parent, name)) => win::open_in(parent, name),
            None => win::open_path(&node.path),
        };
        drop(rel); // the parent's handle closes once all its sub-folders have been opened
        let dir = opened.map(Arc::new);
        let open_ns = since(t_open);
        match (&dir, was_rel) {
            (Some(_), true) => st.rel_opens.fetch_add(1, Ordering::Relaxed),
            (Some(_), false) => st.path_opens.fetch_add(1, Ordering::Relaxed),
            (None, _) => st.fallbacks.fetch_add(1, Ordering::Relaxed),
        };

        let mut seen = 0u32;
        let mut files = 0u64;
        let mut cancelled = false;
        let mut visit = |e: win::Ent<'_>| -> bool {
            seen = seen.wrapping_add(1);
            if seen % 1024 == 0 {
                if self.cancelled() {
                    cancelled = true;
                    return false;
                }
                if seen % 4096 == 0 {
                    self.bump(&node.root, std::mem::take(unreported));
                }
            }
            if e.is_dir {
                let stamp = win::systime(e.written).map(ns).unwrap_or(0);
                let rel: Rel = dir.as_ref().map(|d| (Arc::clone(d), Box::from(e.name)));
                self.sub_folder(node, node.path.join(win::name(&e)), stamp, rel, sum, unreported, spawned);
            } else {
                let mut n = e.len;
                files += 1;
                if exact.is_some() || !hot_here.is_empty() {
                    let name = win::name(&e);
                    if exact.as_deref().is_some_and(|x| same_name(&name, x)) || hot_here.iter().any(|h| same_name(&name, h)) {
                        n = fs::symlink_metadata(node.path.join(&name)).map(|m| m.len()).unwrap_or(n);
                    }
                }
                *sum = sum.saturating_add(n);
                *unreported = unreported.saturating_add(n);
            }
            true
        };
        let times = match &dir {
            Some(d) => win::each_in(d, &mut lc.buf, &mut visit),
            None => win::each(&node.path, &mut visit)?,
        };
        st.win_first_ns.fetch_add(times.first + open_ns, Ordering::Relaxed);
        st.win_next_ns.fetch_add(times.next, Ordering::Relaxed);
        st.win_close_ns.fetch_add(times.close, Ordering::Relaxed);
        st.win_cb_ns.fetch_add(times.callback, Ordering::Relaxed);
        st.files.fetch_add(files, Ordering::Relaxed);
        Some(!cancelled)
    }
}

/// `file_len` for a folder whose hot file names were fetched once (see `HotFiles::names_in`):
/// no path is built and no lock is taken per file.
#[cfg(not(windows))]
fn file_len_in(de: &fs::DirEntry, pinned: Option<&OsStr>, hot_here: &[OsString]) -> u64 {
    if pinned.is_some() || !hot_here.is_empty() {
        let name = de.file_name();
        if pinned.is_some_and(|n| same_name(&name, n)) || hot_here.iter().any(|n| same_name(&name, n)) {
            return exact_len(de);
        }
    }
    de.metadata().map(|m| m.len()).unwrap_or(0)
}

fn worker(w: &Walk<'_>) {
    if BACKGROUND_PRIORITY {
        lower_priority();
    }
    let mut finished: Finished = Vec::new();
    let mut lc = Local {
        stack: Vec::new(),
        #[cfg(windows)]
        buf: vec![0u64; 8192], // 64 KiB
    };
    loop {
        if w.cancelled() {
            w.stop.store(true, Ordering::Relaxed);
            w.wake.notify_all();
            break;
        }
        // Own stack first (depth first), then the shared queue.
        let node = match lc.stack.pop() {
            Some(n) => n,
            None => match w.next() {
                Some(n) => n,
                None => break,
            },
        };
        let t = Instant::now();
        let ok = w.list(&node, &mut finished, &mut lc);
        w.stats.list_ns.fetch_add(since(t), Ordering::Relaxed);
        w.stats.folders.fetch_add(1, Ordering::Relaxed);
        if !ok {
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
    #[cfg(windows)]
    {
        let (kernel, user) = win::thread_cpu();
        w.stats.kernel_ns.fetch_add(kernel, Ordering::Relaxed);
        w.stats.user_ns.fetch_add(user, Ordering::Relaxed);
    }
}

/// Walks all `roots` ((folder, mtime fingerprint) pairs) at once with `threads` threads, so even
/// a single huge tree is spread over all of them. Every finished folder is stored in the index.
/// `progress(root, bytes_so_far)` is called about ten times a second per root while it is being
/// walked; `done(root, size)` once when it is complete. Returns when everything is done or the
/// walk was cancelled (`generation` changed).
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
        waiting: AtomicUsize::new(0),
        stop: AtomicBool::new(false),
        ctl: Condvar::new(),
        ctl_lock: Mutex::new(()),
        threads: AtomicUsize::new(0),
        cache,
        generation,
        my_gen,
        dirty,
        hot,
        now: now_secs(),
        began: Instant::now(),
        stats: Stats::default(),
        progress,
        done,
    };
    for (path, stamp) in roots {
        let dev = fs::metadata(&path).map(|m| dev_of(&m)).unwrap_or(0);
        let root = Arc::new(Root { path: path.clone(), dev, running: AtomicU64::new(0), last_report: AtomicU64::new(0) });
        walk.push(Arc::new(Node::new(path, stamp, None, root)));
    }
    let base = threads.max(1);
    walk.threads.store(base, Ordering::Relaxed);
    std::thread::scope(|s| {
        let w = &walk;
        for _ in 0..base {
            s.spawn(move || worker(w));
        }
        // Windows: while the threads spend most of their time waiting instead of computing (a cold
        // disk), start more of them, a few at a time, up to four times the base number (at most 32).
        // A warm, CPU-bound scan is not affected: there the threads keep the cores busy.
        #[cfg(windows)]
        s.spawn(move || {
            let max = (base * 4).min(32).max(base);
            let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
            let mut started = base;
            let mut last = Instant::now();
            let mut last_cpu = win::process_cpu();
            let mut g = w.ctl_lock.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if w.cancelled() || w.open.load(Ordering::SeqCst) == 0 {
                    break;
                }
                g = match w.ctl.wait_timeout(g, Duration::from_millis(200)) {
                    Ok((g, _)) => g,
                    Err(e) => e.into_inner().0,
                };
                let wall = last.elapsed();
                if wall < Duration::from_millis(200) {
                    continue;
                }
                let cpu = win::process_cpu();
                let busy = cpu.saturating_sub(last_cpu) as f64 / wall.as_nanos() as f64; // cores' worth of CPU
                last = Instant::now();
                last_cpu = cpu;
                if started < max && busy < 0.6 * started.min(cores) as f64 {
                    let to = (started + 4).min(max);
                    for _ in started..to {
                        s.spawn(move || worker(w));
                    }
                    started = to;
                    w.threads.store(started, Ordering::Relaxed);
                }
            }
        });
    });
    if cfg!(debug_assertions) {
        let wall = walk.began.elapsed();
        let st = &walk.stats;
        let ms = |a: &AtomicU64| a.load(Ordering::Relaxed) / 1_000_000;
        let n = |a: &AtomicU64| a.load(Ordering::Relaxed);
        let started = walk.threads.load(Ordering::Relaxed);
        eprintln!("scan_many: {count} root(s), {base} thread(s) to start with, {started} at the end, {wall:?}");
        eprintln!(
            "  {} folders and {} files listed, queue peaked at {}, {} folders handed between threads",
            n(&st.folders),
            n(&st.files),
            st.peak_queue.load(Ordering::Relaxed),
            n(&st.donated)
        );
        eprintln!(
            "  opened relative to parent {}, by path {}, fallback {}",
            n(&st.rel_opens),
            n(&st.path_opens),
            n(&st.fallbacks)
        );
        eprintln!(
            "  thread time in ms: listing {} (index lookups {}, index stores {}), idle {}, available {}",
            ms(&st.list_ns),
            ms(&st.get_ns),
            ms(&st.store_ns),
            ms(&st.idle_ns),
            wall.as_millis() * started as u128
        );
        eprintln!("  cpu time in ms (all threads): kernel {}, user {}", ms(&st.kernel_ns), ms(&st.user_ns));
        eprintln!(
            "  windows calls in ms: open+first read {}, next reads {}, close {}, per-entry callback {}",
            ms(&st.win_first_ns),
            ms(&st.win_next_ns),
            ms(&st.win_close_ns),
            ms(&st.win_cb_ns)
        );
    }
}

/// Windows directory listing. `each_in` reads an open directory handle with `NtQueryDirectoryFile`
/// into a caller-supplied buffer; folders are opened with `NtOpenFile`, optionally relative to
/// their parent's handle. `each` is the fallback through `FindFirstFileExW`. `std::fs::read_dir`
/// uses the same API as the fallback but with the default buffer, with 8.3 short names looked up
/// for every entry and with a `DirEntry` (name and path allocations) per file; none of that is
/// needed to sum sizes.
#[cfg(windows)]
mod win {
    use std::{
        ffi::{c_void, OsString},
        os::windows::ffi::{OsStrExt, OsStringExt},
        path::Path,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    /// WIN32_FIND_DATAW
    #[repr(C)]
    struct FindData {
        attributes: u32,
        _created: FileTime,
        _accessed: FileTime,
        written: FileTime,
        size_high: u32,
        size_low: u32,
        /// The reparse tag if the entry is a reparse point.
        reparse_tag: u32,
        _reserved1: u32,
        name: [u16; 260],
        _short_name: [u16; 14],
    }

    #[repr(C)]
    struct UnicodeString {
        length: u16,
        maximum_length: u16,
        buffer: *mut u16,
    }

    #[repr(C)]
    struct ObjectAttributes {
        length: u32,
        root_directory: isize,
        object_name: *mut UnicodeString,
        attributes: u32,
        security_descriptor: *mut c_void,
        security_quality_of_service: *mut c_void,
    }

    #[repr(C)]
    struct IoStatusBlock {
        status: isize,
        information: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn FindFirstFileExW(
            file_name: *const u16,
            info_level: i32,
            find_data: *mut FindData,
            search_op: i32,
            search_filter: *const c_void,
            additional_flags: u32,
        ) -> isize;
        fn FindNextFileW(handle: isize, find_data: *mut FindData) -> i32;
        fn FindClose(handle: isize) -> i32;
        fn GetCurrentThread() -> isize;
        fn GetCurrentProcess() -> isize;
        fn GetProcessTimes(
            process: isize,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
        fn GetThreadTimes(
            thread: isize,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
    }

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtOpenFile(
            file_handle: *mut isize,
            desired_access: u32,
            object_attributes: *mut ObjectAttributes,
            io_status_block: *mut IoStatusBlock,
            share_access: u32,
            open_options: u32,
        ) -> i32;
        fn NtQueryDirectoryFile(
            file_handle: isize,
            event: isize,
            apc_routine: *mut c_void,
            apc_context: *mut c_void,
            io_status_block: *mut IoStatusBlock,
            file_information: *mut c_void,
            length: u32,
            file_information_class: i32,
            return_single_entry: u8,
            file_name: *mut UnicodeString,
            restart_scan: u8,
        ) -> i32;
        fn NtClose(handle: isize) -> i32;
    }

    const FIND_EX_INFO_BASIC: i32 = 1; // no short (8.3) names
    const FIND_EX_SEARCH_NAME_MATCH: i32 = 0;
    const FIND_FIRST_EX_LARGE_FETCH: u32 = 2; // bigger buffer, fewer system calls
    const ATTR_DIRECTORY: u32 = 0x10;
    const ATTR_REPARSE_POINT: u32 = 0x400;
    const TAG_SYMLINK: u32 = 0xA000_000C;
    const TAG_MOUNT_POINT: u32 = 0xA000_0003; // junctions

    const FILE_LIST_DIRECTORY: u32 = 0x1;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const SHARE_ALL: u32 = 0x7; // read | write | delete
    const FILE_DIRECTORY_FILE: u32 = 0x1;
    const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;
    const FILE_OPEN_FOR_BACKUP_INTENT: u32 = 0x4000;
    const OBJ_CASE_INSENSITIVE: u32 = 0x40;
    /// FILE_FULL_DIR_INFORMATION: times, EndOfFile, attributes, name; no short name.
    const FILE_FULL_DIRECTORY_INFORMATION: i32 = 2;
    /// Bytes of a FILE_FULL_DIR_INFORMATION entry before the name.
    const ENTRY_FIXED: usize = 68;

    pub struct Ent<'a> {
        /// UTF-16 name, no terminator.
        pub name: &'a [u16],
        pub is_dir: bool,
        pub len: u64,
        /// Last write time, 100 ns ticks since 1601 (only used for folders).
        pub written: u64,
    }

    pub fn name(e: &Ent<'_>) -> OsString {
        OsString::from_wide(e.name)
    }

    /// The last-write time as the `SystemTime` that `fs::Metadata::modified` would give.
    pub fn systime(written: u64) -> Option<SystemTime> {
        const TO_UNIX_EPOCH: u64 = 116_444_736_000_000_000; // 1601 -> 1970 in 100 ns ticks
        written.checked_sub(TO_UNIX_EPOCH).map(|t| UNIX_EPOCH + Duration::from_nanos(t.saturating_mul(100)))
    }

    /// CPU time of the calling thread so far in nanoseconds: (kernel mode, user mode).
    pub fn thread_cpu() -> (u64, u64) {
        let mut t = [0u32; 8]; // four FILETIMEs: creation, exit, kernel, user
        let p = t.as_mut_ptr() as *mut FileTime;
        // SAFETY: plain FFI; `t` holds room for the four FILETIME structures.
        let ok = unsafe { GetThreadTimes(GetCurrentThread(), p, p.add(1), p.add(2), p.add(3)) };
        if ok == 0 {
            return (0, 0);
        }
        let ticks = |lo: u32, hi: u32| (((hi as u64) << 32) | lo as u64).saturating_mul(100);
        (ticks(t[4], t[5]), ticks(t[6], t[7]))
    }

    /// CPU time of the whole process so far in nanoseconds (kernel and user mode together).
    pub fn process_cpu() -> u64 {
        let mut t = [0u32; 8]; // four FILETIMEs: creation, exit, kernel, user
        let p = t.as_mut_ptr() as *mut FileTime;
        // SAFETY: plain FFI; `t` holds room for the four FILETIME structures.
        let ok = unsafe { GetProcessTimes(GetCurrentProcess(), p, p.add(1), p.add(2), p.add(3)) };
        if ok == 0 {
            return 0;
        }
        let ticks = |lo: u32, hi: u32| ((hi as u64) << 32) | lo as u64;
        ticks(t[4], t[5]).saturating_add(ticks(t[6], t[7])).saturating_mul(100)
    }

    /// Time spent inside the system calls of one listing, in nanoseconds.
    #[derive(Default)]
    pub struct Times {
        pub first: u64,
        pub next: u64,
        pub close: u64,
        pub callback: u64,
    }

    /// An open directory handle, closed when dropped.
    pub struct Dir(isize);

    impl Drop for Dir {
        fn drop(&mut self) {
            // SAFETY: the handle was returned by NtOpenFile and is closed exactly once.
            unsafe { NtClose(self.0) };
        }
    }

    fn open_raw(root: Option<&Dir>, name: &[u16]) -> Option<Dir> {
        let len = u16::try_from(name.len() * 2).ok()?;
        if len == 0 {
            return None;
        }
        let mut us = UnicodeString { length: len, maximum_length: len, buffer: name.as_ptr() as *mut u16 };
        let mut oa = ObjectAttributes {
            length: std::mem::size_of::<ObjectAttributes>() as u32,
            root_directory: root.map_or(0, |d| d.0),
            object_name: &mut us,
            attributes: OBJ_CASE_INSENSITIVE,
            security_descriptor: std::ptr::null_mut(),
            security_quality_of_service: std::ptr::null_mut(),
        };
        let mut iosb = IoStatusBlock { status: 0, information: 0 };
        let mut h: isize = 0;
        // SAFETY: plain FFI; every pointer refers to a live local, `name` outlives the call.
        let status = unsafe {
            NtOpenFile(
                &mut h,
                FILE_LIST_DIRECTORY | SYNCHRONIZE,
                &mut oa,
                &mut iosb,
                SHARE_ALL,
                FILE_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_FOR_BACKUP_INTENT,
            )
        };
        if status < 0 { None } else { Some(Dir(h)) }
    }

    /// Opens the folder `name` inside the open folder `parent`.
    pub fn open_in(parent: &Dir, name: &[u16]) -> Option<Dir> {
        open_raw(Some(parent), name)
    }

    fn starts(w: &[u16], prefix: &str) -> bool {
        let p: Vec<u16> = prefix.encode_utf16().collect();
        w.starts_with(&p)
    }

    /// The path in the form `NtOpenFile` wants (`\??\C:\dir`). None for paths that are not
    /// absolute or have no drive; the caller then falls back to `FindFirstFileExW`.
    fn nt_path(p: &Path) -> Option<Vec<u16>> {
        let w: Vec<u16> =
            p.as_os_str().encode_wide().map(|c| if c == b'/' as u16 { b'\\' as u16 } else { c }).collect();
        let mut out: Vec<u16>;
        if starts(&w, r"\??\") {
            out = w;
        } else if starts(&w, r"\\?\") {
            out = r"\??\".encode_utf16().collect();
            out.extend_from_slice(&w[4..]);
        } else if starts(&w, r"\\") {
            out = r"\??\UNC\".encode_utf16().collect();
            out.extend_from_slice(&w[2..]);
        } else if w.get(1) == Some(&(b':' as u16)) {
            out = r"\??\".encode_utf16().collect();
            out.extend_from_slice(&w);
        } else {
            return None;
        }
        Some(out)
    }

    /// Opens a folder by its full path.
    pub fn open_path(p: &Path) -> Option<Dir> {
        open_raw(None, &nt_path(p)?)
    }

    fn u32_at(d: &[u8], o: usize) -> u32 {
        u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
    }

    fn u64_at(d: &[u8], o: usize) -> u64 {
        let mut b = [0u8; 8];
        b.copy_from_slice(&d[o..o + 8]);
        u64::from_le_bytes(b)
    }

    /// Calls `f` for every file and folder in the open folder `dir`, except `.`, `..`, symbolic
    /// links and junctions. `f` returns false to stop early. `buf` is the (8-byte aligned) read
    /// buffer.
    pub fn each_in(dir: &Dir, buf: &mut [u64], mut f: impl FnMut(Ent<'_>) -> bool) -> Times {
        let mut tm = Times::default();
        let cap = (buf.len() * 8).min(u32::MAX as usize);
        let base = buf.as_mut_ptr() as *mut u8;
        let mut restart = 1u8;
        'read: loop {
            let mut iosb = IoStatusBlock { status: 0, information: 0 };
            let t = Instant::now();
            // SAFETY: plain FFI; `base` points to `cap` writable bytes, `iosb` is a live local.
            let status = unsafe {
                NtQueryDirectoryFile(
                    dir.0,
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut iosb,
                    base as *mut c_void,
                    cap as u32,
                    FILE_FULL_DIRECTORY_INFORMATION,
                    0,
                    std::ptr::null_mut(),
                    restart,
                )
            };
            let spent = t.elapsed().as_nanos() as u64;
            if restart == 1 {
                tm.first += spent;
            } else {
                tm.next += spent;
            }
            if status < 0 {
                break; // STATUS_NO_MORE_FILES at the end, or an error
            }
            restart = 0;
            let used = iosb.information.min(cap);
            if used == 0 {
                break;
            }
            // SAFETY: the system just filled `used` bytes at `base`.
            let data = unsafe { std::slice::from_raw_parts(base as *const u8, used) };
            let mut off = 0usize;
            loop {
                if off + ENTRY_FIXED > data.len() {
                    break;
                }
                let next = u32_at(data, off) as usize;
                let name_len = u32_at(data, off + 60) as usize / 2;
                let name_start = off + ENTRY_FIXED;
                if name_start % 2 != 0 || name_start + name_len * 2 > data.len() {
                    break;
                }
                // SAFETY: the buffer is 8-byte aligned, entries start at multiples of 8 and the
                // name at +68, so the pointer is 2-byte aligned; the bounds were checked above.
                let name = unsafe { std::slice::from_raw_parts(data.as_ptr().add(name_start) as *const u16, name_len) };
                if !matches!(name, [46] | [46, 46]) {
                    let attrs = u32_at(data, off + 56);
                    let tag = u32_at(data, off + 64); // EaSize holds the reparse tag of links
                    let link = attrs & ATTR_REPARSE_POINT != 0 && (tag == TAG_SYMLINK || tag == TAG_MOUNT_POINT);
                    if !link {
                        let ent = Ent {
                            name,
                            is_dir: attrs & ATTR_DIRECTORY != 0,
                            len: u64_at(data, off + 40),
                            written: u64_at(data, off + 24),
                        };
                        let tc = Instant::now();
                        let go = f(ent);
                        tm.callback += tc.elapsed().as_nanos() as u64;
                        if !go {
                            break 'read;
                        }
                    }
                }
                if next == 0 {
                    break;
                }
                off += next;
            }
        }
        tm
    }

    fn wide_path(p: &Path) -> Vec<u16> {
        let mut w: Vec<u16> = p.as_os_str().encode_wide().collect();
        // Long paths only work in the \\?\ form (`std` does the same for its own calls).
        let verbatim: Vec<u16> = r"\\?\".encode_utf16().collect();
        if w.len() >= 240 && !w.starts_with(&verbatim) {
            if w.starts_with(&[b'\\' as u16, b'\\' as u16]) {
                let mut v: Vec<u16> = r"\\?\UNC\".encode_utf16().collect();
                v.extend_from_slice(&w[2..]);
                w = v;
            } else if w.get(1) == Some(&(b':' as u16)) {
                let mut v = verbatim;
                v.extend_from_slice(&w);
                w = v;
            }
        }
        w
    }

    /// Fallback listing through `FindFirstFileExW`, by path. Calls `f` for the same entries as
    /// `each_in`. None = the folder could not be opened.
    pub fn each(dir: &Path, mut f: impl FnMut(Ent<'_>) -> bool) -> Option<Times> {
        let mut pattern = wide_path(dir);
        if pattern.last() != Some(&(b'\\' as u16)) {
            pattern.push(b'\\' as u16);
        }
        pattern.push(b'*' as u16);
        pattern.push(0);
        // SAFETY: plain FFI; `data` is a valid WIN32_FIND_DATAW and `pattern` is NUL-terminated.
        let mut data: FindData = unsafe { std::mem::zeroed() };
        let mut tm = Times::default();
        let t0 = Instant::now();
        let h = unsafe {
            FindFirstFileExW(
                pattern.as_ptr(),
                FIND_EX_INFO_BASIC,
                &mut data,
                FIND_EX_SEARCH_NAME_MATCH,
                std::ptr::null(),
                FIND_FIRST_EX_LARGE_FETCH,
            )
        };
        tm.first = t0.elapsed().as_nanos() as u64;
        if h == -1 {
            return None;
        }
        loop {
            let n = data.name.iter().position(|&c| c == 0).unwrap_or(data.name.len());
            let name = &data.name[..n];
            if !matches!(name, [46] | [46, 46]) {
                let attrs = data.attributes;
                let link = attrs & ATTR_REPARSE_POINT != 0
                    && (data.reparse_tag == TAG_SYMLINK || data.reparse_tag == TAG_MOUNT_POINT);
                if !link {
                    let ent = Ent {
                        name,
                        is_dir: attrs & ATTR_DIRECTORY != 0,
                        len: ((data.size_high as u64) << 32) | data.size_low as u64,
                        written: ((data.written.high as u64) << 32) | data.written.low as u64,
                    };
                    let tc = Instant::now();
                    let go = f(ent);
                    tm.callback += tc.elapsed().as_nanos() as u64;
                    if !go {
                        break;
                    }
                }
            }
            let tn = Instant::now();
            let more = unsafe { FindNextFileW(h, &mut data) } != 0;
            tm.next += tn.elapsed().as_nanos() as u64;
            if !more {
                break;
            }
        }
        let tcl = Instant::now();
        unsafe { FindClose(h) };
        tm.close = tcl.elapsed().as_nanos() as u64;
        Some(tm)
    }
}
