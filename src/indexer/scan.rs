//! The folder walk: sums file sizes, reuses fresh cached sub-folders, stores what it finishes.
//!
//! Every thread walks depth first through a stack of its own and hands half of it to the shared
//! queue when another thread has run out of work. Depth first keeps the directory handle of a
//! folder alive just long enough for its sub-folders to be opened relative to it (Windows), which
//! is much cheaper than resolving the whole path for every folder again.

use super::{
    hot::{same_name, HotDirs, HotFiles},
    model::{is_fresh, now_secs, stamp_of, wait_for, Cancel, CachedFolder, MutexExt},
    priority::lower_priority,
    store::{finish_key, key_state, Cache},
};
#[cfg(not(windows))]
use super::store::extend_name;
#[cfg(windows)]
use super::store::extend_wide;
use std::{
    collections::VecDeque,
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Condvar, Mutex,
    },
    time::{Duration, Instant},
};

/// A folder with no sub-folders, less than this much data and fewer than `SMALL_LEAF_FILES` files is
/// not stored in the index: listing it again costs about as much as looking it up, and such folders
/// are roughly half of all folders. The results stay exact either way. A folder with many files is
/// stored however small it is, because listing it again is not cheap. The folders of the current
/// listing (the roots of a scan) are always stored, so that their old sizes can be shown at once.
const SMALL_LEAF_BYTES: u64 = 1 << 20;
const SMALL_LEAF_FILES: usize = 64;

/// Folders a thread keeps for itself; beyond this the oldest ones go to the shared queue.
const LOCAL_MAX: usize = 256;

/// A folder handed to a queue shorter than this keeps its parent's handle, so that it can still be
/// opened relative to it. In a longer queue the handles would pile up, so they are released.
const QUEUE_KEEP_HANDLES: usize = 4096;

/// How a folder can be opened without its full path: relative to the still open handle of its
/// parent (Windows only).
#[cfg(windows)]
type Rel = Option<(Arc<win::Dir>, Box<[u16]>)>;
#[cfg(not(windows))]
type Rel = ();

/// Device of a folder; the walk does not cross into other file systems (Unix; on Windows mount
/// points are reparse points and skipped as links).
#[cfg(unix)]
fn dev_of(m: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    m.dev()
}

/// Size of a file asked from the file itself (a handle-based stat) instead of the directory
/// listing, which may be stale for a file that is being written. `fallback` if that fails.
fn exact_len(p: &Path, fallback: u64) -> u64 {
    fs::symlink_metadata(p).map(|m| m.len()).unwrap_or(fallback)
}

/// Does the file `name` have to be read exactly: it is the pinned file or a hot one (see `hot.rs`)?
fn needs_exact(name: &OsStr, pinned: Option<&OsStr>, hot_here: &[OsString]) -> bool {
    pinned.is_some_and(|p| same_name(name, p)) || hot_here.iter().any(|h| same_name(name, h))
}

/// A folder to scan: where, the fingerprint of its mtime, and since when a recursive watch has been
/// covering it (`ChangeSink::watched_since`), which decides how long its cached sub-folders are trusted.
pub struct RootSpec {
    pub path: PathBuf,
    pub stamp: u32,
    pub watched_since: Option<u64>,
}

/// One tree that is being summed.
struct Root {
    path: PathBuf,
    /// Inherited by every folder below the root (see `is_fresh`).
    watched_since: Option<u64>,
    #[cfg(unix)]
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
    /// Running hash of `path` (see `store::key_state`); a child extends it with its own name.
    state: u64,
    stamp: u32,
    parent: Option<Arc<Node>>,
    root: Arc<Root>,
    /// 1 for the folder's own listing + 1 per sub-folder that is not finished yet.
    pending: AtomicUsize,
    total: AtomicU64,
    /// The folder could not be listed: its total is passed up but not stored in the index.
    failed: AtomicBool,
    /// The folder has sub-folders, and this many files (see `SMALL_LEAF_BYTES`).
    has_dirs: AtomicBool,
    files: AtomicUsize,
}

impl Node {
    fn new(path: PathBuf, state: u64, stamp: u32, parent: Option<Arc<Node>>, root: Arc<Root>) -> Node {
        Node {
            path,
            state,
            stamp,
            parent,
            root,
            pending: AtomicUsize::new(1),
            total: AtomicU64::new(0),
            failed: AtomicBool::new(false),
            has_dirs: AtomicBool::new(false),
            files: AtomicUsize::new(0),
        }
    }
}

/// A folder waiting to be listed, with the handle of its parent (if it still holds one). The handle
/// is released when the folder is handed to a long shared queue, so handles never pile up.
struct Job {
    node: Arc<Node>,
    rel: Rel,
}

/// What listing one folder produces.
#[derive(Default)]
struct Acc {
    /// Files and fresh cached sub-folders of the folder.
    sum: u64,
    /// The part of `sum` that has not been reported to the live display yet.
    unreported: u64,
    /// Sub-folders that are not in the index (or not fresh) and still have to be walked.
    spawned: Vec<Job>,
}

/// What one worker thread keeps to itself.
struct Local {
    /// Folders waiting for this thread, the newest last (depth first).
    stack: Vec<Job>,
    /// Read buffer of the directory calls (8-byte aligned), reused for every folder.
    #[cfg(windows)]
    buf: Vec<u64>,
}

type Finished = Vec<(u64, CachedFolder)>;

struct Walk<'a> {
    queue: Mutex<VecDeque<Job>>,
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
    cache: &'a Cache,
    cancel: &'a Cancel,
    /// The pinned file and the hot files, copied once when the walk starts (see `HotFiles`).
    pinned: Option<(PathBuf, OsString)>,
    hot_dirs: Arc<HotDirs>,
    now: u64,
    began: Instant,
    progress: &'a (dyn Fn(&Path, u64) + Sync),
    done: &'a (dyn Fn(&Path, u64) + Sync),
}

impl Walk<'_> {
    fn cancelled(&self) -> bool {
        self.stop.load(Ordering::Relaxed) || self.cancel.is_set()
    }

    /// Name of the pinned file, if it lives directly in `dir`.
    fn pinned_in(&self, dir: &Path) -> Option<&OsStr> {
        let (d, n) = self.pinned.as_ref()?;
        same_name(d.as_os_str(), dir.as_os_str()).then_some(n.as_os_str())
    }

    /// Names of the hot files that live directly in `dir` (almost always none).
    fn hot_in(&self, dir: &Path) -> &[OsString] {
        if self.hot_dirs.is_empty() {
            return &[];
        }
        match self.hot_dirs.get(dir) {
            Some(v) => v.as_slice(),
            None => &[],
        }
    }

    /// Puts a root into the shared queue.
    fn push(&self, node: Arc<Node>) {
        self.open.fetch_add(1, Ordering::SeqCst);
        self.queue.lk().push_back(Job { node, rel: Rel::default() });
        self.wake.notify_one();
    }

    /// The listing of one folder is over (its sub-folders already exist when this is called).
    fn close(&self) {
        if self.open.fetch_sub(1, Ordering::SeqCst) == 1 {
            // Last one: wake the idle workers and the controller so they can leave.
            drop(self.queue.lk());
            self.wake.notify_all();
            drop(self.ctl_lock.lk());
            self.ctl.notify_all();
        }
    }

    /// Next folder from the shared queue. None = everything is done or the walk was cancelled.
    fn next(&self) -> Option<Job> {
        let mut q = self.queue.lk();
        loop {
            if self.cancelled() {
                self.stop.store(true, Ordering::Relaxed);
                self.wake.notify_all();
                return None;
            }
            if let Some(j) = q.pop_front() {
                return Some(j);
            }
            if self.open.load(Ordering::SeqCst) == 0 {
                return None;
            }
            self.waiting.fetch_add(1, Ordering::Relaxed);
            q = wait_for(&self.wake, q, Duration::from_millis(50));
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

    /// Called for every directory entry: now and then looks for a cancel and reports progress.
    /// False = cancelled.
    fn tick(&self, seen: &mut u32, root: &Root, unreported: &mut u64) -> bool {
        *seen = seen.wrapping_add(1);
        if *seen % 1024 == 0 {
            if self.cancelled() {
                return false;
            }
            if *seen % 4096 == 0 {
                self.bump(root, std::mem::take(unreported));
            }
        }
        true
    }

    fn store(&self, finished: &mut Finished) {
        if !finished.is_empty() {
            self.cache.put_many(std::mem::take(finished));
        }
    }

    /// Marks the listing of `node` as done and walks up the tree while that finishes folders.
    fn finish(&self, mut node: Arc<Node>, finished: &mut Finished) {
        loop {
            if node.pending.fetch_sub(1, Ordering::SeqCst) != 1 {
                return;
            }
            let total = node.total.load(Ordering::SeqCst);
            let keep = node.parent.is_none()
                || node.has_dirs.load(Ordering::Relaxed)
                || total >= SMALL_LEAF_BYTES
                || node.files.load(Ordering::Relaxed) >= SMALL_LEAF_FILES;
            if keep && !node.failed.load(Ordering::Relaxed) {
                let rec = CachedFolder { size: total, stamp: node.stamp, scanned: self.now as u32 };
                finished.push((finish_key(node.state), rec));
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

    /// A sub-folder of `node` whose path state is `state`: taken from the index if it is up to
    /// date, otherwise it becomes a new job in `acc.spawned`. The path (`path`) and the parent's
    /// handle (`rel`) are only built for the folders that are not taken from the index.
    fn sub_folder(
        &self,
        node: &Arc<Node>,
        state: u64,
        stamp: u32,
        path: &dyn Fn() -> PathBuf,
        rel: impl FnOnce() -> Rel,
        acc: &mut Acc,
    ) {
        node.has_dirs.store(true, Ordering::Relaxed);
        match self.cache.get_key(finish_key(state), path).filter(|c| is_fresh(c, stamp, self.now, node.root.watched_since)) {
            Some(hit) => {
                acc.sum += hit.size;
                acc.unreported += hit.size;
            }
            None => {
                node.pending.fetch_add(1, Ordering::SeqCst);
                self.open.fetch_add(1, Ordering::SeqCst);
                let child = Node::new(path(), state, stamp, Some(Arc::clone(node)), Arc::clone(&node.root));
                acc.spawned.push(Job { node: Arc::new(child), rel: rel() });
            }
        }
    }

    /// Takes the sub-folders found by a listing onto the thread's own stack. If other threads
    /// are out of work, or the stack grows large, the oldest folders go to the shared queue.
    fn dispatch(&self, lc: &mut Local, spawned: Vec<Job>) {
        if spawned.is_empty() {
            return;
        }
        lc.stack.extend(spawned);
        let len = lc.stack.len();
        let give = if self.waiting.load(Ordering::Relaxed) > 0 {
            len / 2
        } else if len > LOCAL_MAX {
            len - LOCAL_MAX / 2
        } else {
            0
        };
        if give == 0 {
            return;
        }
        let mut q = self.queue.lk();
        for mut job in lc.stack.drain(..give) {
            // In a long queue a folder may wait a long time: it must not hold its parent's handle.
            if q.len() >= QUEUE_KEEP_HANDLES {
                job.rel = Rel::default();
            }
            q.push_back(job);
        }
        drop(q);
        self.wake.notify_all();
    }

    /// Lists one folder: sums its files, takes fresh sub-folders from the index and creates jobs
    /// for the others. False = cancelled half-way (this folder is then not stored).
    fn list(&self, job: Job, finished: &mut Finished, lc: &mut Local) -> bool {
        let Job { node, rel } = job;
        let mut acc = Acc::default();
        match self.read_folder(&node, rel, &mut acc, lc) {
            None => {
                node.failed.store(true, Ordering::Relaxed);
                self.finish(node, finished);
                return true;
            }
            Some(false) => return false,
            Some(true) => {}
        }
        node.total.fetch_add(acc.sum, Ordering::SeqCst);
        self.bump(&node.root, acc.unreported);
        self.finish(node, finished);
        self.dispatch(lc, acc.spawned);
        true
    }

    /// The directory read behind `list`. None = the folder cannot be listed, Some(false) = cancelled.
    /// Links and junctions are skipped (loops, double counting); sizes come from the listing, except
    /// for the pinned file and hot files, which are asked directly.
    #[cfg(not(windows))]
    fn read_folder(&self, node: &Arc<Node>, _rel: Rel, acc: &mut Acc, _lc: &mut Local) -> Option<bool> {
        let rd = fs::read_dir(&node.path).ok()?;
        let exact = self.pinned_in(&node.path);
        let hot_here = self.hot_in(&node.path);
        let mut seen = 0u32;
        let mut files = 0usize;
        for entry in rd {
            if !self.tick(&mut seen, &node.root, &mut acc.unreported) {
                return Some(false);
            }
            let Ok(de) = entry else { continue };
            let Ok(ft) = de.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_file() {
                let mut n = de.metadata().map(|m| m.len()).unwrap_or(0);
                files += 1;
                if exact.is_some() || !hot_here.is_empty() {
                    let name = de.file_name();
                    if needs_exact(&name, exact, hot_here) {
                        n = exact_len(&node.path.join(&name), n);
                    }
                }
                acc.sum += n;
                acc.unreported += n;
            } else if ft.is_dir() {
                let md = de.metadata().ok();
                let name = de.file_name();
                if is_part_name(&name) { continue; }
                let path = node.path.join(&name);
                if super::live::is_pseudo(&path) || md.as_ref().is_some_and(|m| dev_of(m) != node.root.dev) {
                    continue;
                }
                let stamp = md.and_then(|m| m.modified().ok()).map(stamp_of).unwrap_or(0);
                let state = extend_name(node.state, &name);
                self.sub_folder(node, state, stamp, &|| path.clone(), || (), acc);
            }
        }
        node.files.store(files, Ordering::Relaxed);
        Some(true)
    }

    /// Windows: the folder is opened relative to its parent's handle (or by path) and read with
    /// `NtQueryDirectoryFile` into the thread's reusable buffer. Same rules as everywhere else.
    /// A sub-folder that is found in the index costs no allocation at all: its key is the parent's
    /// path state extended with the name from the listing.
    #[cfg(windows)]
    fn read_folder(&self, node: &Arc<Node>, rel: Rel, acc: &mut Acc, lc: &mut Local) -> Option<bool> {
        let exact = self.pinned_in(&node.path);
        let hot_here = self.hot_in(&node.path);
        let opened = match &rel {
            Some((parent, name)) => win::open_in(parent, name),
            None => win::open_path(&node.path),
        };
        drop(rel); // the parent's handle closes once all its sub-folders have been opened
        let dir = Arc::new(opened?);

        let mut seen = 0u32;
        let mut files = 0usize;
        let mut cancelled = false;
        let mut visit = |e: win::Ent<'_>| -> bool {
            if !self.tick(&mut seen, &node.root, &mut acc.unreported) {
                cancelled = true;
                return false;
            }
            if e.is_dir {
                if e.name.first() == Some(&(b'.' as u16)) && crate::fsops::is_part_name(&win::name(&e)) { return true; }
                let stamp = win::systime(e.written).map(stamp_of).unwrap_or(0);
                let state = extend_wide(node.state, e.name);
                self.sub_folder(
                    node,
                    state,
                    stamp,
                    &|| node.path.join(win::name(&e)),
                    || Some((Arc::clone(&dir), Box::from(e.name))),
                    acc,
                );
            } else {
                let mut n = e.len;
                files += 1;
                if exact.is_some() || !hot_here.is_empty() {
                    let name = win::name(&e);
                    if needs_exact(&name, exact, hot_here) {
                        n = exact_len(&node.path.join(&name), n);
                    }
                }
                acc.sum += n;
                acc.unreported += n;
            }
            true
        };
        win::each_in(&dir, &mut lc.buf, &mut visit);
        node.files.store(files, Ordering::Relaxed);
        Some(!cancelled)
    }
}

fn worker(w: &Walk<'_>) {
    lower_priority();
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
        let Some(job) = lc.stack.pop().or_else(|| w.next()) else { break };
        if !w.list(job, &mut finished, &mut lc) {
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

/// Walks all `roots` at once with `threads` threads, so even
/// a single huge tree is spread over all of them. Every finished folder is stored in the index.
/// `progress(root, bytes_so_far)` is called about ten times a second per root while it is being
/// walked; `done(root, size)` once when it is complete. Returns when everything is done or the
/// walk was cancelled.
pub fn scan_many(
    roots: Vec<RootSpec>,
    cache: &Cache,
    cancel: &Cancel,
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
        cache,
        cancel,
        pinned: hot.pinned(),
        hot_dirs: hot.snapshot(),
        now: now_secs(),
        began: Instant::now(),
        progress,
        done,
    };
    for RootSpec { path, stamp, watched_since } in roots {
        let root = Arc::new(Root {
            path: path.clone(),
            watched_since,
            #[cfg(unix)]
            dev: fs::metadata(&path).map(|m| dev_of(&m)).unwrap_or(0),
            running: AtomicU64::new(0),
            last_report: AtomicU64::new(0),
        });
        let state = key_state(&path);
        walk.push(Arc::new(Node::new(path, state, stamp, None, root)));
    }
    let base = threads.max(1);
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
            if base <= 1 {
                return; // a single-thread walk (see `scan_one`) stays single-threaded
            }
            let max = (base * 4).min(32).max(base);
            let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
            let mut started = base;
            let mut last = Instant::now();
            let mut last_cpu = win::process_cpu();
            let mut g = w.ctl_lock.lk();
            loop {
                if w.cancelled() || w.open.load(Ordering::SeqCst) == 0 {
                    break;
                }
                g = wait_for(&w.ctl, g, Duration::from_millis(200));
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
                }
            }
        });
    });
    if cfg!(debug_assertions) {
        eprintln!("scan_many: {count} root(s), {:?}", walk.began.elapsed());
    }
}

/// Walks one folder on one thread (the live updater's rescans). None = cancelled.
pub fn scan_one(
    root: &Path,
    stamp: u32,
    watched_since: Option<u64>,
    cache: &Cache,
    cancel: &Cancel,
    hot: &HotFiles,
) -> Option<u64> {
    let result = AtomicU64::new(0);
    let finished = AtomicBool::new(false);
    let done = |_: &Path, n: u64| {
        result.store(n, Ordering::Relaxed);
        finished.store(true, Ordering::Relaxed);
    };
    let spec = RootSpec { path: root.to_path_buf(), stamp, watched_since };
    scan_many(vec![spec], cache, cancel, hot, 1, &|_, _| {}, &done);
    finished.load(Ordering::Relaxed).then(|| result.load(Ordering::Relaxed))
}

/// Windows directory listing: folders are opened with `NtOpenFile`, optionally relative to their
/// parent's handle, and read with `NtQueryDirectoryFile` into a caller-supplied buffer. Compared
/// with `fs::read_dir` this skips the 8.3 short names, the per-file name and path allocations and
/// the full path resolution for every folder; none of that is needed to sum sizes.
#[cfg(windows)]
mod win {
    use std::{
        ffi::{c_void, OsString},
        os::windows::ffi::{OsStrExt, OsStringExt},
        path::Path,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
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
        fn GetCurrentProcess() -> isize;
        fn GetProcessTimes(
            process: isize,
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

    const ATTR_DIRECTORY: u32 = 0x10;
    const ATTR_REPARSE_POINT: u32 = 0x400;
    /// `std` treats every reparse tag with this bit as a link (symbolic links, junctions, WSL links,
    /// ...): `fs::read_dir` + `file_type().is_symlink()` skipped all of them, so this must too.
    const TAG_NAME_SURROGATE: u32 = 0x2000_0000;

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

    /// The path in the form `NtOpenFile` wants (`\??\C:\dir`). None for a path that is not absolute
    /// (the folder then counts as one that cannot be listed).
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
    pub fn each_in(dir: &Dir, buf: &mut [u64], mut f: impl FnMut(Ent<'_>) -> bool) {
        let cap = (buf.len() * 8).min(u32::MAX as usize);
        let base = buf.as_mut_ptr() as *mut u8;
        let mut restart = 1u8;
        loop {
            let mut iosb = IoStatusBlock { status: 0, information: 0 };
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
            if status < 0 {
                return; // STATUS_NO_MORE_FILES at the end, or an error
            }
            restart = 0;
            let used = iosb.information.min(cap);
            if used == 0 {
                return;
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
                    let link = attrs & ATTR_REPARSE_POINT != 0 && tag & TAG_NAME_SURROGATE != 0;
                    if !link {
                        let ent = Ent {
                            name,
                            is_dir: attrs & ATTR_DIRECTORY != 0,
                            len: u64_at(data, off + 40),
                            written: u64_at(data, off + 24),
                        };
                        if !f(ent) {
                            return;
                        }
                    }
                }
                if next == 0 {
                    break;
                }
                off += next;
            }
        }
    }
}
