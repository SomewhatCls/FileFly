//! Native Recycle Bin access (Windows + freedesktop Linux) through the `trash` crate.
//! On other platforms the stubs report "not supported" and the app falls back to the system file manager.

use crate::fsops::{format_date, format_size, DateFormat};
use crate::icons::icon_for_extension;
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, OnceLock,
    },
    time::{Duration, SystemTime},
};
use crate::prelude::*;

#[cfg(any(windows, all(unix, not(target_os = "macos"))))]
mod imp {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    pub type Item = trash::TrashItem;
    pub const SUPPORTED: bool = true;

    /// What the system knows about a trashed item.
    struct Probe {
        size: Option<u64>,
        entries: Option<u64>,
        is_dir: bool,
        /// Where a genuine trashed folder really is on disk (the indexer sums it from there).
        dir_path: Option<PathBuf>,
    }

    fn probe(item: &Item) -> Probe {
        // A file has a size; for a folder the system only reports how many entries it holds.
        // Its size is summed later by the indexer, from where the folder really is on disk.
        let (mut size, entries, mut is_dir) = match trash::os_limited::metadata(item) {
            Ok(md) => match md.size {
                trash::TrashItemSize::Bytes(b) => (Some(b), None, false),
                trash::TrashItemSize::Entries(n) => (None, Some(n as u64), true),
            },
            Err(_) => (None, None, false),
        };
        let mut dir_path = None;
        if is_dir {
            if let Some(p) = real_path(item) {
                match std::fs::metadata(&p) {
                    // Check for folder-likes which work like files, e.g. .zip.
                    Ok(md) if md.is_file() => {
                        size = Some(md.len());
                        is_dir = false;
                    }
                    // A genuine folder: the indexer sums it later
                    Ok(md) if md.is_dir() => dir_path = Some(p),
                    _ => {}
                }
            }
        }
        Probe { size, entries, is_dir, dir_path }
    }

    /// Identifies a trashed item in the size index. The deletion time guards against a reused id.
    pub fn key(item: &Item) -> String {
        format!("{}|{}", item.time_deleted, item.id.to_string_lossy())
    }

    pub fn load(fmt: DateFormat) -> Result<Vec<TrashRow>, String> {
        let items = trash::os_limited::list().map_err(|e| e.to_string())?;
        let mut out = Vec::with_capacity(items.len());
        let mut keys = HashSet::with_capacity(items.len());
        let mut shown = 0;
        for item in items {
            let Probe { size, entries, is_dir, dir_path } = probe(&item);
            if cfg!(debug_assertions) && is_dir && shown < 3 {
                eprintln!("trash folder: id {:?} -> {:?}", item.id, dir_path);
                shown += 1;
            }
            let original = item.original_path();
            let name = original.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let location = original.parent().map(|p| p.display().to_string()).unwrap_or_default();
            let deleted = (item.time_deleted >= 0).then(|| UNIX_EPOCH + Duration::from_secs(item.time_deleted as u64));
            let k = key(&item);
            index::learn(k.clone(), &original, size);
            keys.insert(k);
            out.push(make_row(item, name, location, deleted, size, entries, dir_path, is_dir, fmt));
        }
        index::finish(keys);
        Ok(out)
    }

    /// Brings the size index in line with the bin without loading rows: only items the index
    /// doesn't know yet are looked at (a trashed item never changes, so known ones need no work).
    pub fn refresh_index() -> Result<(), String> {
        let items = trash::os_limited::list().map_err(|e| e.to_string())?;
        let mut keys = HashSet::with_capacity(items.len());
        for item in &items {
            let k = key(item);
            if !index::knows(&k) {
                index::learn(k.clone(), &item.original_path(), probe(item).size);
            }
            keys.insert(k);
        }
        index::finish(keys);
        Ok(())
    }

    /// The folders whose changes mean "something was trashed, restored or purged".
    #[cfg(windows)]
    pub fn bin_dirs() -> Vec<PathBuf> {
        let mut out = Vec::new();
        for c in 'A'..='Z' {
            let root = PathBuf::from(format!("{c}:\\$Recycle.Bin"));
            let Ok(rd) = std::fs::read_dir(&root) else { continue };
            out.push(root);
            out.extend(rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
        }
        out
    }

    #[cfg(not(windows))]
    pub fn bin_dirs() -> Vec<PathBuf> {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
        base.map(|b| vec![b.join("Trash/info")]).unwrap_or_default()
    }

    /// Where the trashed item really is on disk (file or folder). None if the id doesn't lead there.
    fn real_path(item: &Item) -> Option<PathBuf> {
        #[cfg(windows)]
        let p = PathBuf::from(&item.id);
        #[cfg(not(windows))]
        let p = {
            let info = std::path::Path::new(&item.id);
            let name = info.file_name()?.to_str()?.strip_suffix(".trashinfo")?;
            info.parent()?.parent()?.join("files").join(name)
        };
        p.is_absolute().then_some(p)
    }

    /// Where the item was deleted from (and goes back to).
    pub fn original(item: &Item) -> PathBuf {
        item.original_path()
    }

    pub fn restore(items: Vec<Item>, progress: &OpProgress) -> Result<(), String> {
        progress.total_items.store(items.len() as _, AO::Relaxed);
        let mut failed = Vec::new();
        for item in items {
            let name = item.name.to_string_lossy().into_owned();
            progress.set_current(&name); // placeholder: whatever fills `current()`
            if let Err(e) = trash::os_limited::restore_all([item]) {
                failed.push(format!("{name}: {e}"));
            }
            progress.done_items.fetch_add(1, AO::Relaxed);
        }
        if failed.is_empty() { Ok(()) } else { Err(failed.join("\n")) }
    }

    pub fn purge(items: Vec<Item>) -> Result<(), String> {
        trash::os_limited::purge_all(items).map_err(|e| e.to_string())
    }
}

#[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
mod imp {
    use super::*;

    #[derive(Clone)]
    pub struct Item;
    pub const SUPPORTED: bool = false;

    pub fn load(_fmt: DateFormat) -> Result<Vec<TrashRow>, String> {
        Err("The Recycle Bin view is not supported on this platform.".into())
    }
    pub fn key(_: &Item) -> String {
        String::new()
    }
    pub fn refresh_index() -> Result<(), String> {
        Ok(())
    }
    pub fn bin_dirs() -> Vec<PathBuf> {
        Vec::new()
    }
    pub fn original(_: &Item) -> PathBuf {
        PathBuf::new()
    }
    pub fn restore(_items: Vec<Item>) -> Result<(), String> {
        Err("Not supported on this platform.".into())
    }
    pub fn purge(_items: Vec<Item>) -> Result<(), String> {
        Err("Not supported on this platform.".into())
    }
}

pub use imp::*;

pub struct TrashRow {
    pub item: Item,
    pub name: String,
    pub name_lc: String,
    pub location: String,
    pub location_lc: String,
    pub is_dir: bool,
    pub deleted: Option<SystemTime>,
    pub date_text: String,
    /// Bytes: a file's size, or a folder's once the indexer has summed it.
    pub size: Option<u64>,
    pub size_text: String,
    pub icon: &'static str,
    /// Where a trashed folder really is on disk (see `real_dir`).
    pub dir_path: Option<PathBuf>,
    /// The folder's size is still being summed, or is an old value that is being refreshed.
    pub size_pending: bool,
}

impl TrashRow {
    /// Sets a folder's size from the indexer. `pending` = a scan is still running.
    pub fn set_dir_size(&mut self, bytes: u64, pending: bool) {
        self.size = Some(bytes);
        self.size_text = format_size(bytes);
        self.size_pending = pending;
        if !pending {
            index::set_clean(key(&self.item), bytes);
        }
    }
}

#[allow(dead_code)]
fn make_row(
    item: Item,
    name: String,
    location: String,
    deleted: Option<SystemTime>,
    size: Option<u64>,
    entries: Option<u64>,
    dir_path: Option<PathBuf>,
    is_dir: bool,
    fmt: DateFormat,
) -> TrashRow {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    TrashRow {
        item,
        name_lc: name.to_lowercase(),
        location_lc: location.to_lowercase(),
        date_text: format_date(deleted, fmt, false),
        size_text: match (size, entries) {
            (Some(b), _) => format_size(b),
            (None, Some(n)) => format!("{n} item{}", if n == 1 { "" } else { "s" }),
            (None, None) => "—".to_owned(),
        },
        is_dir,
        icon: if is_dir { "folder" } else { icon_for_extension(&ext) },
        name,
        location,
        deleted,
        size,
        dir_path,
        size_pending: false,
    }
}

// ---------------------------------------------------------------------------------------------
// Size index: the Recycle Bin's fill size for the sidebar, without summing anything live
// ---------------------------------------------------------------------------------------------
//
// One entry per trashed item: its size and whether that size is "dirty" (a lower bound: a folder
// that was trashed while its size was still being summed). Items in the bin never change, so an
// entry only needs to be written when the item arrives, and again when its folder size becomes
// exact (`TrashRow::set_dir_size`, which the open Recycle Bin view feeds from the indexer).
// What is really in the bin is the truth: `refresh_index` drops entries that are gone and adds
// the ones that are missing (items trashed or restored by other programs, or while we were closed).

static WORKER: OnceLock<mpsc::Sender<()>> = OnceLock::new();
static SETTLED: AtomicBool = AtomicBool::new(false);
static ENABLED: AtomicBool = AtomicBool::new(false);

pub use index::init as init_index;

/// The size index follows the folder-size indexer: without one, folder sizes are never summed,
/// so nothing is tracked or shown. Turning it on syncs right away.
pub fn set_index_enabled(on: bool) {
    ENABLED.store(on, Ordering::SeqCst);
    if on {
        poke();
    }
}

pub fn index_enabled() -> bool {
    SUPPORTED && ENABLED.load(Ordering::SeqCst)
}

/// (total bytes, some of it is only a lower bound); `None` while unknown or switched off.
pub fn index_summary() -> Option<(u64, bool)> {
    index_enabled().then(index::summary).flatten()
}

fn poke() {
    if let Some(tx) = WORKER.get() {
        let _ = tx.send(());
    }
}

/// The folder is about to be trashed: remember its size from the indexer (`exact` = that scan is
/// finished), so the index doesn't start from zero. Folders without any known size need no call.
pub fn expect_trashed(path: PathBuf, size: u64, exact: bool) {
    index::hint(path, size, exact);
}

/// Held for the duration of a delete operation. When it is dropped (finished, failed or
/// cancelled, whichever way the operation ends) the index is synced and the hints of items that
/// never made it into the bin are forgotten.
pub struct DeletionGuard;

impl Drop for DeletionGuard {
    fn drop(&mut self) {
        SETTLED.store(true, Ordering::SeqCst);
        poke();
    }
}

/// Starts (once) the thread that keeps the index in line with the bin: it syncs right away and
/// whenever the bin's folders change (or a deletion settles), after the changes have been quiet
/// for a moment. `repaint` wakes the UI afterwards so the sidebar shows the new value.
pub fn start_index(repaint: impl Fn() + Send + 'static) {
    use notify::{RecursiveMode, Watcher};
    if !index_enabled() || WORKER.get().is_some() {
        return;
    }
    WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<()>();
        let _ = tx.send(());
        let wtx = tx.clone();
        std::thread::spawn(move || {
            // The watcher is only a "something changed" signal; the sync below finds out what.
            let mut watcher = notify::recommended_watcher(move |_: notify::Result<notify::Event>| {
                let _ = wtx.send(());
            })
            .ok();
            let mut watched = HashSet::new();
            while rx.recv().is_ok() {
                while rx.recv_timeout(Duration::from_millis(400)).is_ok() {}
                if let Some(w) = watcher.as_mut() {
                    for dir in bin_dirs() {
                        if !watched.contains(&dir) && w.watch(&dir, RecursiveMode::NonRecursive).is_ok() {
                            watched.insert(dir);
                        }
                    }
                }
                let settled = SETTLED.swap(false, Ordering::SeqCst);
                if index_enabled() {
                    let _ = refresh_index();
                }
                if settled {
                    index::clear_hints();
                }
                repaint();
            }
        });
        tx
    });
}

mod index {
    use std::{
        collections::{HashMap, HashSet},
        fs,
        path::{Path, PathBuf},
        sync::{
            atomic::{AtomicBool, Ordering},
            LazyLock, Mutex, MutexGuard,
        },
        time::Duration,
    };

    const FILE: &str = "trash_sizes.txt";
    const VERSION: &str = "v1";

    #[derive(Default)]
    struct Index {
        path: Option<PathBuf>,
        /// Item key -> (bytes, dirty).
        entries: HashMap<String, (u64, bool)>,
        /// Folders that are being trashed: original path -> (last known bytes, exact).
        hints: HashMap<PathBuf, (u64, bool)>,
        /// (total bytes, any entry dirty); `None` until the index is loaded or synced once.
        summary: Option<(u64, bool)>,
    }

    impl Index {
        fn changed(&mut self) {
            let total = self.entries.values().map(|e| e.0).sum();
            self.summary = Some((total, self.entries.values().any(|e| e.1)));
        }
    }

    static INDEX: LazyLock<Mutex<Index>> = LazyLock::new(Default::default);
    static SAVE_QUEUED: AtomicBool = AtomicBool::new(false);
    static WRITING: Mutex<()> = Mutex::new(());

    fn lock() -> MutexGuard<'static, Index> {
        INDEX.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Where the index file lives (the indexer's folder) and what it held last time.
    /// Call once at startup, before the sidebar is first drawn.
    pub fn init(dir: &Path) {
        let path = dir.join(FILE);
        let mut ix = lock();
        if ix.summary.is_none() {
            if let Some(text) = fs::read_to_string(&path).ok().filter(|t| t.starts_with(VERSION)) {
                ix.entries = text
                    .lines()
                    .skip(1)
                    .filter_map(|l| {
                        let mut f = l.splitn(3, '\t');
                        let (size, dirty) = (f.next()?.parse().ok()?, f.next()? == "1");
                        Some((f.next()?.to_owned(), (size, dirty)))
                    })
                    .collect();
                ix.changed();
            }
        }
        ix.path = Some(path);
    }

    /// (total bytes, some of it is only a lower bound).
    pub fn summary() -> Option<(u64, bool)> {
        lock().summary
    }

    pub fn hint(path: PathBuf, size: u64, exact: bool) {
        lock().hints.insert(path, (size, exact));
    }

    pub fn clear_hints() {
        lock().hints.clear();
    }

    pub fn knows(key: &str) -> bool {
        lock().entries.contains_key(key)
    }

    /// An item is in the bin. A known size is exact; otherwise a folder is dirty, with the size
    /// from its hint (if any) as the lower bound. Known folders keep what they have.
    pub fn learn(key: String, original: &Path, size: Option<u64>) {
        let mut ix = lock();
        if let Some(b) = size {
            ix.entries.insert(key, (b, false));
        } else if !ix.entries.contains_key(&key) {
            let e = match ix.hints.remove(original) {
                Some((b, exact)) => (b, !exact),
                None => {
                    if cfg!(debug_assertions) {
                        eprintln!("trash index: no size hint for {original:?} (hints: {:?})", ix.hints.keys().collect::<Vec<_>>());
                    }
                    (0, true)
                }
            };
            ix.entries.insert(key, e);
        }
    }

    /// A trashed folder's size has been summed completely.
    pub fn set_clean(key: String, bytes: u64) {
        if key.is_empty() {
            return;
        }
        let mut ix = lock();
        ix.entries.insert(key, (bytes, false));
        ix.changed();
        drop(ix);
        save();
    }

    /// `keys` is everything that is in the bin now.
    pub fn finish(keys: HashSet<String>) {
        let mut ix = lock();
        ix.entries.retain(|k, _| keys.contains(k));
        ix.changed();
        drop(ix);
        save();
    }

    /// Writes the index soon (many folders finishing at once make one write), in the background.
    fn save() {
        if SAVE_QUEUED.swap(true, Ordering::SeqCst) {
            return;
        }
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(500));
            let _w = WRITING.lock().unwrap_or_else(|e| e.into_inner());
            SAVE_QUEUED.store(false, Ordering::SeqCst);
            let (path, text) = {
                let ix = lock();
                let mut text = String::from(VERSION);
                for (key, (size, dirty)) in &ix.entries {
                    text.push_str(&format!("\n{size}\t{}\t{key}", u8::from(*dirty)));
                }
                (ix.path.clone(), text)
            };
            let Some(path) = path else { return };
            let tmp = path.with_extension("tmp");
            if fs::write(&tmp, text).and_then(|_| fs::rename(&tmp, &path)).is_err() {
                let _ = fs::remove_file(&tmp);
            }
        });
    }
}
