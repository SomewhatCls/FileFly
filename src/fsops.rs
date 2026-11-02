//! File-system helpers and formatting that don't touch the UI.
//!
//! # Where temporary files live (and how a crash is cleaned up)
//!
//! | What                                   | Where                                                    | Name                                  |
//! |----------------------------------------|----------------------------------------------------------|---------------------------------------|
//! | Copy, or Cut across drives (`copy_item_with`) | next to the destination                                  | `.<name>.part-<pid>-<n>`              |
//! | Extract a zip (`extract_zip`)          | next to the destination                                  | `.<name>.part-<pid>-<n>`              |
//! | Compress to zip (`compress_items`)     | next to the destination                                  | `.<name>.part-<pid>-<n>.zip`          |
//! | "Replace" parks the old item           | next to the destination                                  | `.<name>.replaced-<pid>-<n>`          |
//! | Browsing a zip like a folder           | `<temp dir>/FileFly-archives-<pid>/`                     | (`.in-use` lock file inside)          |
//! | Journal of the temp items above        | `<temp dir>/FileFly-journal-<pid>-<stamp>.log`           | one `+`/`-` line per temp item        |
//!
//! A Cut on the same drive is a plain rename and creates no temp file at all.
//!
//! The temp items sit next to their destination (anywhere on any drive), so they can't be found by
//! scanning a folder. Instead every one is written to the journal *before* it is created and struck
//! out again once it is gone. At startup `sweep_leftover_temps` reads the journals of dead runs
//! (a journal whose lock can be taken belongs to a process that no longer exists; a running second
//! window holds its lock) and, for each entry that is still pending:
//!   * `part`: deletes the half-written copy / archive,
//!   * `replaced`: puts the original back if the destination is missing, otherwise deletes it
//!     (the replacement was finished, because it only ever appears by an atomic rename).
//!
//! Only paths named in a journal are touched, and only if their name still has exactly the shape
//! listed above and carries the pid of the run that wrote the journal. Cut sources, the user's own
//! files and anything else are never looked at.

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    fs, io,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering as AO},
    time::{Duration, SystemTime},
};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DateFormat {
    #[default]
    European, // 31.12.2025 14:05
    Iso,      // 2025-12-31 14:05
    American, // 12/31/2025 2:05 PM
}

impl DateFormat {
    pub fn label(self) -> &'static str {
        match self {
            DateFormat::European => "31.12.2025 14:05",
            DateFormat::Iso => "2025-12-31 14:05",
            DateFormat::American => "12/31/2025 2:05 PM",
        }
    }
    fn pattern(self, seconds: bool) -> &'static str {
        match (self, seconds) {
            (DateFormat::European, false) => "%d.%m.%Y %H:%M",
            (DateFormat::European, true) => "%d.%m.%Y %H:%M:%S",
            (DateFormat::Iso, false) => "%Y-%m-%d %H:%M",
            (DateFormat::Iso, true) => "%Y-%m-%d %H:%M:%S",
            (DateFormat::American, false) => "%m/%d/%Y %-I:%M %p",
            (DateFormat::American, true) => "%m/%d/%Y %-I:%M:%S %p",
        }
    }
}

pub fn format_date(time: Option<SystemTime>, fmt: DateFormat, seconds: bool) -> String {
    match time {
        Some(t) => DateTime::<Local>::from(t).format(fmt.pattern(seconds)).to_string(),
        None => "—".to_owned(),
    }
}

pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{size:.1} {}", UNITS[unit]) }
}

pub fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "report10" sorts after "report2".
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = ai.peek().copied().filter(char::is_ascii_digit) {
                    na.push(c);
                    ai.next();
                }
                let mut nb = String::new();
                while let Some(c) = bi.peek().copied().filter(char::is_ascii_digit) {
                    nb.push(c);
                    bi.next();
                }
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let o = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
                ai.next();
                bi.next();
            }
        }
    }
}

#[cfg(windows)]
pub fn attributes(md: &fs::Metadata) -> u32 {
    use std::os::windows::fs::MetadataExt;
    md.file_attributes()
}
#[cfg(not(windows))]
pub fn attributes(_md: &fs::Metadata) -> u32 {
    0
}

/// Uses the attributes already delivered by `read_dir` (no extra syscall per entry, and correct for
/// junctions such as "Application Data", which `fs::metadata` can't follow).
pub fn is_hidden_entry(name: &str, md: &fs::Metadata) -> bool {
    attributes(md) & (0x2 | 0x4) != 0 || name.starts_with('.')
}

pub fn remove_existing(path: &Path) -> std::io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) }
}

/// Removes the half-written temp item of a failed or cancelled operation, and makes sure its disk
/// space comes back even when the name can't be deleted right away. A just-closed file is often held
/// for a moment by antivirus, the search indexer or a sync client (OneDrive, Dropbox); a plain
/// delete then fails, and because the temp name starts with a dot the leftover is invisible in
/// listings while its bytes stay allocated. So: empty the file first (frees the space even if the
/// delete is refused), retry the delete a few times, and if it is still locked keep retrying in the
/// background (the journal covers the case that the app is closed before that succeeds).
fn remove_temp(path: &Path) {
    if let Ok(md) = fs::symlink_metadata(path) {
        if md.is_file() {
            if let Ok(f) = fs::OpenOptions::new().write(true).open(path) {
                let _ = f.set_len(0);
            }
        }
    }
    for attempt in 0..6u64 {
        match remove_existing(path) {
            Ok(()) => return,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return,
            Err(_) => std::thread::sleep(Duration::from_millis(100 * (attempt + 1))),
        }
    }
    let p = path.to_path_buf();
    std::thread::spawn(move || {
        for _ in 0..60 {
            std::thread::sleep(Duration::from_secs(2));
            if remove_existing(&p).is_ok() || fs::symlink_metadata(&p).is_err() {
                journal_settle(&p);
                return;
            }
        }
    });
}

pub fn unique_copy_name(destination: &Path) -> PathBuf {
    if !destination.exists() {
        return destination.to_path_buf();
    }
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    // Built from OsStrings, so a name that is not valid Unicode is kept byte for byte.
    let stem = destination.file_stem().map(|s| s.to_os_string()).unwrap_or_else(|| "Copy".into());
    let ext = destination.extension().map(|e| e.to_os_string());
    for n in 1u64.. {
        let mut name = stem.clone();
        name.push(format!(" ({n})"));
        if let Some(e) = &ext {
            name.push(".");
            name.push(e);
        }
        let candidate = parent.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

/// A free name next to `path` for something that is only temporary: the leading dot hides it from
/// listings, the pid keeps two running instances apart, the counter avoids clobbering a leftover.
fn free_sibling(path: &Path, tag: &str) -> PathBuf {
    let name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    for n in 0u32.. {
        let mut file = std::ffi::OsString::from(".");
        file.push(&name);
        file.push(format!(".{tag}-{}-{n}", std::process::id()));
        let candidate = path.with_file_name(file);
        if fs::symlink_metadata(&candidate).is_err() {
            return candidate;
        }
    }
    unreachable!()
}

/// Where "Replace" parks the existing item until the new one is safely in place. If the app dies
/// in between, the original is still sitting next to the destination under this name.
pub fn backup_name(path: &Path) -> PathBuf {
    let aside = free_sibling(path, "replaced");
    journal_add("replaced", &aside, path);
    aside
}

/// Call once the parked original is gone again (deleted after a successful paste, or renamed back
/// after a failed one, or the parking rename itself failed). Does nothing while it still exists, so
/// the next start can still deal with it.
pub fn backup_settled(aside: &Path) {
    journal_settle(aside);
}

/// Final rename of a finished temp item. Antivirus/indexers sometimes hold a just-written file or
/// folder for a moment, which shows up as "access denied"; a few short retries ride that out.
fn rename_retry(from: &Path, to: &Path) -> io::Result<()> {
    let mut attempt = 0u64;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if attempt < 5 && (e.kind() == io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(32)) => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(60 * attempt));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Live state of a long file operation, shared between the worker thread (which updates it) and the
/// progress dialog (which reads it). Plain atomics, so neither side ever waits for the other.
#[derive(Default)]
pub struct OpProgress {
    /// Bytes to copy in total / copied so far. Moves on one drive are renames and add no bytes.
    pub total_bytes: AtomicU64,
    pub done_bytes: AtomicU64,
    /// Top-level items to handle / finished so far.
    pub total_items: AtomicU64,
    pub done_items: AtomicU64,
    /// The worker is still measuring what there is to do.
    pub preparing: AtomicBool,
    /// Set by the dialog; the worker stops at the next chunk and cleans up its temp item.
    pub cancel: AtomicBool,
    /// Set by the dialog; the worker waits (at chunk boundaries) until it is cleared.
    pub paused: AtomicBool,
    current: std::sync::Mutex<String>,
    /// Items being worked on right now (id, name). The dialog lists them, each with its own cancel button.
    running: std::sync::Mutex<Vec<(u64, String)>>,
    /// Running items the user cancelled one by one: only they stop, the rest of the operation carries on.
    skipped: std::sync::Mutex<std::collections::HashSet<u64>>,
    any_skipped: AtomicBool,
}

thread_local! {
    /// The item the current worker thread is busy with (0: none), and what that item has added to the
    /// shared byte counters so far, so a cancelled item can take its share back out again.
    static ITEM_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static ITEM_DONE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static ITEM_TOTAL: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

const CANCELLED: &str = "cancelled";

/// True for the error a cancelled operation stops with (not something to show the user).
pub fn is_cancelled(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::Interrupted && e.to_string() == CANCELLED
}

impl OpProgress {
    pub fn set_current(&self, name: &str) {
        *self.current.lock().unwrap_or_else(|e| e.into_inner()) = name.to_owned();
    }

    pub fn current(&self) -> String {
        self.current.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(AO::Relaxed)
    }

    /// Worker thread: starts item `id` (ids are unique within one operation, never 0).
    pub fn begin_item(&self, id: u64, name: &str) {
        ITEM_ID.with(|c| c.set(id));
        ITEM_DONE.with(|c| c.set(0));
        ITEM_TOTAL.with(|c| c.set(0));
        self.running.lock().unwrap_or_else(|e| e.into_inner()).push((id, name.to_owned()));
    }

    /// Worker thread: ends the item started with `begin_item`; returns the bytes it had added to
    /// (done, total) while it ran.
    pub fn end_item(&self, id: u64) -> (u64, u64) {
        self.running.lock().unwrap_or_else(|e| e.into_inner()).retain(|r| r.0 != id);
        self.skipped.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        ITEM_ID.with(|c| c.set(0));
        (ITEM_DONE.with(|c| c.replace(0)), ITEM_TOTAL.with(|c| c.replace(0)))
    }

    /// The items being worked on right now, in the order they started.
    pub fn running_items(&self) -> Vec<(u64, String)> {
        self.running.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Dialog: stop just this item (its partial result is removed); everything else carries on.
    pub fn skip_item(&self, id: u64) {
        self.skipped.lock().unwrap_or_else(|e| e.into_inner()).insert(id);
        self.any_skipped.store(true, AO::Relaxed);
    }

    fn this_item_skipped(&self) -> bool {
        let id = ITEM_ID.with(|c| c.get());
        id != 0
            && self.any_skipped.load(AO::Relaxed)
            && self.skipped.lock().unwrap_or_else(|e| e.into_inner()).contains(&id)
    }

    /// Takes back bytes a cancelled item had contributed (`done`) or announced (`total`).
    pub fn forgo(&self, done: u64, total: u64) {
        let _ = self.done_bytes.try_update(AO::Relaxed, AO::Relaxed, |v| Some(v.saturating_sub(done)));
        let _ = self.total_bytes.try_update(AO::Relaxed, AO::Relaxed, |v| Some(v.saturating_sub(total)));
    }

    fn add_done(&self, n: u64) {
        self.done_bytes.fetch_add(n, AO::Relaxed);
        ITEM_DONE.with(|c| c.set(c.get() + n));
    }

    fn add_total(&self, n: u64) {
        self.total_bytes.fetch_add(n, AO::Relaxed);
        ITEM_TOTAL.with(|c| c.set(c.get() + n));
    }

    /// Called between chunks / items: waits while paused, fails once the operation (or just this
    /// item) is cancelled.
    fn checkpoint(&self) -> io::Result<()> {
        while self.paused.load(AO::Relaxed) && !self.cancelled() && !self.this_item_skipped() {
            std::thread::sleep(Duration::from_millis(50));
        }
        if self.cancelled() || self.this_item_skipped() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, CANCELLED));
        }
        Ok(())
    }
}

fn file_label(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Bytes in `path` (a file or a whole folder tree), walked the same way `copy_tree` walks it.
/// Unreadable parts count as 0 (the copy itself will report them); only a cancel is an error.
pub fn tree_bytes(path: &Path, prog: &OpProgress) -> io::Result<u64> {
    fn walk(path: &Path, depth: usize, prog: &OpProgress) -> io::Result<u64> {
        prog.checkpoint()?;
        let Ok(md) = fs::metadata(path) else { return Ok(0) };
        if !md.is_dir() {
            return Ok(md.len());
        }
        if depth > 128 {
            return Ok(0);
        }
        let Ok(rd) = fs::read_dir(path) else { return Ok(0) };
        let mut sum = 0u64;
        for item in rd.flatten() {
            sum += walk(&item.path(), depth + 1, prog)?;
        }
        Ok(sum)
    }
    walk(path, 0, prog)
}

/// Files up to this size go through `fs::copy` (the OS copy routine: keeps every attribute and is
/// fastest); bigger ones are copied in chunks so the progress dialog can follow them and a
/// pause/cancel takes effect within a moment.
const FAST_COPY_LIMIT: u64 = 8 * 1024 * 1024;
const COPY_CHUNK: usize = 4 * 1024 * 1024;

fn copy_file(from: &Path, to: &Path, prog: &OpProgress) -> io::Result<()> {
    prog.checkpoint()?;
    let md = fs::metadata(from)?;
    if md.len() <= FAST_COPY_LIMIT {
        fs::copy(from, to)?;
        prog.add_done(md.len());
        return Ok(());
    }
    let mut src = fs::File::open(from)?;
    let mut dst = fs::File::create(to)?;
    let mut buf = vec![0u8; COPY_CHUNK.min(md.len() as usize).max(64 * 1024)];
    loop {
        prog.checkpoint()?;
        let n = match src.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        dst.write_all(&buf[..n])?;
        prog.add_done(n as u64);
    }
    if let Ok(t) = md.modified() {
        let _ = dst.set_modified(t);
    }
    drop(dst);
    let _ = fs::set_permissions(to, md.permissions());
    Ok(())
}

/// Recursive copy into `destination` (always a temp folder, so partial results are never visible).
/// The depth limit turns a junction/symlink loop into an error instead of endless recursion.
fn copy_tree(source: &Path, destination: &Path, depth: usize, prog: &OpProgress) -> io::Result<()> {
    if depth > 128 {
        return Err(io::Error::other("folders are nested too deeply (possible link loop)"));
    }
    fs::create_dir_all(destination)?;
    for item in fs::read_dir(source)? {
        let item = item?;
        let (from, to) = (item.path(), destination.join(item.file_name()));
        if from.is_dir() {
            copy_tree(&from, &to, depth + 1, prog)?;
        } else {
            prog.set_current(&item.file_name().to_string_lossy());
            copy_file(&from, &to, prog)?;
        }
    }
    Ok(())
}

/// Copies to a temporary name next to the destination and renames it into place only when the
/// whole copy has succeeded, so the real name never points at a half-written file or folder.
/// Never overwrites: an existing destination is an error (callers move it aside first).
///
/// Reports to (and obeys pause/cancel of) `prog`.
pub fn copy_item_with(source: &Path, destination: &Path, prog: &OpProgress) -> io::Result<()> {
    if fs::symlink_metadata(destination).is_ok() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "the destination already exists"));
    }
    let is_dir = source.is_dir();
    if is_dir && destination.starts_with(source) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "cannot copy a folder into itself"));
    }
    prog.set_current(&file_label(source));
    let tmp = free_sibling(destination, "part");
    journal_add("part", &tmp, destination);
    let copied = if is_dir {
        copy_tree(source, &tmp, 0, prog)
    } else {
        copy_file(source, &tmp, prog)
    };
    let result = copied.and_then(|_| rename_retry(&tmp, destination));
    if result.is_err() {
        remove_temp(&tmp); // don't leave the partial copy behind
    }
    journal_settle(&tmp);
    result
}

// ---------------------------------------------------------------------------------------------
// System clipboard: Copy/Cut publish the selection as a file list, Paste reads it back, so it
// works with Explorer/Finder/file managers and other apps. Same shape as the app's own
// `clipboard: Option<(Vec<PathBuf>, bool)>` (paths, is_cut).
// ---------------------------------------------------------------------------------------------

/// Windows marks a cut with this format (DROPEFFECT_MOVE = 2); no format means copy.
#[cfg(windows)]
const WIN_DROP_EFFECT: &str = "Preferred DropEffect";
/// GNOME/Nautilus-style file managers: first line is "cut" or "copy", then one URI per line.
#[cfg(all(unix, not(target_os = "macos")))]
const GNOME_FILES: &str = "x-special/gnome-copied-files";
/// KDE/Dolphin marks a cut with "1" in this format.
#[cfg(all(unix, not(target_os = "macos")))]
const KDE_CUT: &str = "application/x-kde-cutselection";

/// `canonicalize` yields `\\?\C:\...` on Windows, which many apps can't paste; drop the prefix.
fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => p.to_path_buf(),
    }
}

/// Publishes the items on the system clipboard (with the "cut" marker if `cut`). Nothing is copied
/// or moved here; whichever app pastes does that. macOS has no native file cut, so there a cut
/// is published as a plain copy (the source stays in place, nothing can be lost).
pub fn set_clipboard(paths: &[PathBuf], cut: bool) -> io::Result<()> {
    use clipboard_rs::{Clipboard, ClipboardContent, ClipboardContext};
    let files = paths
        .iter()
        .map(|p| fs::canonicalize(p).map(|c| strip_verbatim(&c).to_string_lossy().into_owned()))
        .collect::<io::Result<Vec<String>>>()?;
    // The text copy exists so egui sees a non-empty clipboard and emits Event::Paste on Ctrl+V;
    // file managers ignore it and use the file list.
    let mut content = vec![ClipboardContent::Files(files.clone()), ClipboardContent::Text(files.join("\n"))];
    if cut {
        #[cfg(windows)]
        content.push(ClipboardContent::Other(WIN_DROP_EFFECT.into(), 2u32.to_le_bytes().to_vec()));
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let mut list = String::from("cut");
            for f in &files {
                if let Ok(u) = url::Url::from_file_path(f) {
                    list.push('\n');
                    list.push_str(u.as_str());
                }
            }
            content.push(ClipboardContent::Other(GNOME_FILES.into(), list.into_bytes()));
            content.push(ClipboardContent::Other(KDE_CUT.into(), b"1".to_vec()));
        }
    }
    let ctx = ClipboardContext::new().map_err(|e| io::Error::other(e.to_string()))?;
    ctx.set(content).map_err(|e| io::Error::other(e.to_string()))
}

/// The files on the system clipboard (from this app or any other) and whether they were cut.
/// `None` if the clipboard holds something else, or only items that no longer exist (which also
/// covers a finished cut-and-paste, since the sources are gone by then).
pub fn get_clipboard() -> Option<(Vec<PathBuf>, bool)> {
    use clipboard_rs::{Clipboard, ClipboardContext};
    let ctx = ClipboardContext::new().ok()?;
    let paths: Vec<PathBuf> = ctx
        .get_files()
        .ok()?
        .into_iter()
        .filter_map(|s| {
            if s.starts_with("file://") {
                url::Url::parse(&s).ok()?.to_file_path().ok()
            } else {
                Some(PathBuf::from(s))
            }
        })
        .filter(|p| fs::symlink_metadata(p).is_ok())
        .collect();
    if paths.is_empty() {
        return None;
    }
    #[cfg(windows)]
    let cut = match ctx.get_buffer(WIN_DROP_EFFECT) {
        Ok(b) if b.len() >= 4 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) & 2 != 0,
        _ => false,
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let cut = ctx.get_buffer(GNOME_FILES).map(|b| b.starts_with(b"cut")).unwrap_or(false)
        || ctx.get_buffer(KDE_CUT).map(|b| b.starts_with(b"1")).unwrap_or(false);
    #[cfg(target_os = "macos")]
    let cut = false;
    Some((paths, cut))
}

fn is_cross_device(e: &io::Error) -> bool {
    // ERROR_NOT_SAME_DEVICE on Windows, EXDEV elsewhere
    e.raw_os_error() == Some(if cfg!(windows) { 17 } else { 18 })
}

/// Rename when possible (atomic on one drive); copy + delete only when the rename failed because
/// the destination is on another drive. Any other failure (item in use, access denied, ...) is
/// reported as is, without half-moving anything. Never replaces an existing destination.
///
/// Reports to (and obeys pause/cancel of) `prog`. A rename adds no bytes; when the move has to become
/// a copy, its size is added to the total only then.
pub fn move_item_with(source: &Path, destination: &Path, prog: &OpProgress) -> io::Result<()> {
    if fs::symlink_metadata(destination).is_ok() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "the destination already exists"));
    }
    prog.checkpoint()?;
    prog.set_current(&file_label(source));
    match fs::rename(source, destination) {
        Ok(()) => Ok(()),
        Err(e) if is_cross_device(&e) => {
            let bytes = tree_bytes(source, prog)?;
            prog.add_total(bytes);
            copy_item_with(source, destination, prog)?; // source stays untouched until this fully succeeded
            remove_existing(source)
        }
        Err(e) => Err(e),
    }
}

pub fn open_recycle_bin() -> std::io::Result<()> {
    #[cfg(windows)]
    {
        return std::process::Command::new("explorer.exe").arg("shell:RecycleBinFolder").spawn().map(|_| ());
    }
    #[cfg(target_os = "macos")]
    {
        let trash = dirs::home_dir()
            .map(|h| h.join(".Trash"))
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "home directory not found"))?;
        return open::that(trash).map_err(std::io::Error::other);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        return open::that("trash:///").map_err(std::io::Error::other);
    }
    #[allow(unreachable_code)]
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "Recycle Bin is not available here"))
}

/// Opens a terminal window in `dir` (Windows Terminal if installed, else the classic console).
pub fn open_terminal(dir: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        if std::process::Command::new("wt.exe").arg("-d").arg(dir).spawn().is_ok() {
            return Ok(());
        }
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        return std::process::Command::new("cmd.exe")
            .current_dir(dir)
            .creation_flags(CREATE_NEW_CONSOLE)
            .spawn()
            .map(|_| ());
    }
    #[cfg(target_os = "macos")]
    {
        return std::process::Command::new("open").args(["-a", "Terminal"]).arg(dir).spawn().map(|_| ());
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        for term in ["kgx", "gnome-terminal", "konsole", "xfce4-terminal", "alacritty", "xterm"] {
            if std::process::Command::new(term).current_dir(dir).spawn().is_ok() {
                return Ok(());
            }
        }
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no terminal emulator found"));
    }
    #[allow(unreachable_code)]
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "not supported here"))
}

pub fn find_peazip() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        for var in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(p) = std::env::var_os(var) {
                let c = PathBuf::from(p).join("PeaZip").join("peazip.exe");
                if c.is_file() {
                    return Some(c);
                }
            }
        }
        None
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("PATH")
            .and_then(|paths| std::env::split_paths(&paths).map(|d| d.join("peazip")).find(|c| c.is_file()))
    }
}

pub fn open_with_peazip(exe: &Path, path: &Path) -> std::io::Result<()> {
    let mut c = std::process::Command::new(exe);
    c.arg(if path.is_dir() { "-ext2browsepath" } else { "-ext2browse" });
    c.arg(path).spawn().map(|_| ())
}

pub fn add_to_peazip(exe: &Path, path: &Path) -> std::io::Result<()> {
    std::process::Command::new(exe).arg("-add2archive").arg(path).spawn().map(|_| ())
}

#[derive(Default, Clone)]
pub struct FolderStats {
    pub files: u64,
    pub folders: u64,
    pub bytes: u64,
}

/// Iterative walk (no recursion depth issues); `cancelled` lets the Properties dialog abort it.
pub fn folder_stats(root: &Path, cancelled: &dyn Fn() -> bool) -> FolderStats {
    let mut stats = FolderStats::default();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if cancelled() {
            break;
        }
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for item in rd.flatten() {
            let Ok(ft) = item.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                stats.folders += 1;
                stack.push(item.path());
            } else {
                stats.files += 1;
                stats.bytes += item.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    stats
}

// ---------------------------------------------------------------------------------------------
// ZIP archives: compress / extract with the tools the operating system ships (no zip library),
// and "open a .zip like a folder" by unpacking it into a private cache that the UI presents
// under the zip's own path (C:\data\photos.zip\2024\...).
//
//   Windows  tar.exe (bsdtar, part of Windows 10+), PowerShell Compress-Archive/Expand-Archive
//   macOS    ditto (the engine behind Finder's "Compress" and double-click extraction)
//   Linux    zip / unzip, then bsdtar, then 7z, whichever is installed
// ---------------------------------------------------------------------------------------------

use std::ffi::{OsStr, OsString};
use std::process::{Command, Stdio};

const OK0: &[i32] = &[0];
/// unzip exits with 1 for "finished, with warnings".
#[cfg(all(unix, not(target_os = "macos")))]
const OK01: &[i32] = &[0, 1];

const NO_ZIP_TOOL: &str = if cfg!(windows) {
    "no ZIP tool found (tar.exe or PowerShell is required)"
} else if cfg!(target_os = "macos") {
    "the system tool \"ditto\" was not found"
} else {
    "no ZIP tool found; install \"zip\" and \"unzip\" (or bsdtar / 7-Zip)"
};

pub fn has_zip_ext(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip"))
}

fn tool(program: impl AsRef<OsStr>) -> Command {
    let mut c = Command::new(program);
    c.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console window flashing up
    }
    c
}

/// Runs a tool to completion. `ErrorKind::NotFound` means the program is not installed (the caller
/// then tries the next candidate); any other error carries the tool's own message.
fn run_tool(mut cmd: Command, ok_codes: &[i32]) -> io::Result<()> {
    let out = cmd.output()?;
    if out.status.success() || out.status.code().is_some_and(|c| ok_codes.contains(&c)) {
        return Ok(());
    }
    let raw = if out.stderr.iter().any(|b| !b.is_ascii_whitespace()) { &out.stderr } else { &out.stdout };
    let text = String::from_utf8_lossy(raw).into_owned();
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).take(3).collect();
    Err(io::Error::other(if lines.is_empty() {
        format!("the ZIP tool failed ({})", out.status)
    } else {
        lines.join(" ")
    }))
}

fn run_first_available(tools: Vec<(Command, &'static [i32])>) -> io::Result<()> {
    for (cmd, ok) in tools {
        match run_tool(cmd, ok) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            done => return done,
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, NO_ZIP_TOOL))
}

/// A name that is safe to hand to a command line tool (a leading "-" would be read as an option).
fn arg_safe(name: &OsStr) -> OsString {
    if name.to_string_lossy().starts_with('-') {
        let mut s = OsString::from("./");
        s.push(name);
        s
    } else {
        name.to_os_string()
    }
}

#[cfg(windows)]
fn system_tar() -> PathBuf {
    // Full path on purpose: a "tar" from Git/MSYS earlier in PATH cannot write zip files.
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32")
        .join("tar.exe")
}

#[cfg(windows)]
fn ps_quote(p: &Path) -> String {
    format!("'{}'", p.to_string_lossy().replace('\'', "''"))
}

#[allow(unused_variables, unused_mut)]
fn compress_tools(parent: &Path, items: &[PathBuf], names: &[OsString], out: &Path) -> Vec<(Command, &'static [i32])> {
    let mut tools: Vec<(Command, &'static [i32])> = Vec::new();
    #[cfg(windows)]
    {
        let mut c = tool(system_tar());
        c.arg("-a").arg("-c").arg("-f").arg(out).arg("-C").arg(parent).args(names);
        tools.push((c, OK0));
        let list = items.iter().map(|p| ps_quote(p)).collect::<Vec<_>>().join(",");
        let mut c = tool("powershell.exe");
        c.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command"]).arg(format!(
            "$ErrorActionPreference='Stop'; Compress-Archive -LiteralPath {list} -DestinationPath {}",
            ps_quote(out)
        ));
        tools.push((c, OK0));
    }
    #[cfg(target_os = "macos")]
    {
        let mut c = tool("ditto");
        c.args(["-c", "-k", "--sequesterRsrc", "--keepParent"]).args(items).arg(out);
        tools.push((c, OK0));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut c = tool("zip");
        c.current_dir(parent).args(["-r", "-q", "-y"]).arg(out).args(names);
        tools.push((c, OK0));
        let mut c = tool("bsdtar");
        c.arg("-a").arg("-c").arg("-f").arg(out).arg("-C").arg(parent).args(names);
        tools.push((c, OK0));
        for sevenzip in ["7z", "7zz"] {
            let mut c = tool(sevenzip);
            c.current_dir(parent).args(["a", "-tzip", "-bd", "-y"]).arg(out).args(names);
            tools.push((c, OK0));
        }
    }
    tools
}

#[allow(unused_variables, unused_mut)]
fn extract_tools(zip: &Path, dest: &Path) -> Vec<(Command, &'static [i32])> {
    let mut tools: Vec<(Command, &'static [i32])> = Vec::new();
    #[cfg(windows)]
    {
        let mut c = tool(system_tar());
        c.arg("-x").arg("-f").arg(zip).arg("-C").arg(dest);
        tools.push((c, OK0));
        let mut c = tool("powershell.exe");
        c.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command"]).arg(format!(
            "$ErrorActionPreference='Stop'; Expand-Archive -LiteralPath {} -DestinationPath {}",
            ps_quote(zip),
            ps_quote(dest)
        ));
        tools.push((c, OK0));
    }
    #[cfg(target_os = "macos")]
    {
        let mut c = tool("ditto");
        c.arg("-x").arg("-k").arg(zip).arg(dest);
        tools.push((c, OK0));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut c = tool("unzip");
        c.arg("-q").arg(zip).arg("-d").arg(dest);
        tools.push((c, OK01));
        let mut c = tool("bsdtar");
        c.arg("-x").arg("-f").arg(zip).arg("-C").arg(dest);
        tools.push((c, OK0));
        for sevenzip in ["7z", "7zz"] {
            let mut o = OsString::from("-o");
            o.push(dest);
            let mut c = tool(sevenzip);
            c.args(["x", "-y", "-bd"]).arg(o).arg(zip);
            tools.push((c, OK0));
        }
    }
    tools
}

/// Hidden temporary name next to `dest` that still ends in ".zip" (the tools pick the format and
/// refuse to write from the extension).
fn zip_temp(dest: &Path) -> PathBuf {
    let mut s = free_sibling(dest, "part").into_os_string();
    s.push(".zip");
    PathBuf::from(s)
}

/// Zips `items` (all in one folder) into the new file `dest`. The archive is built under a temporary
/// name and renamed into place when finished, so `dest` never holds a half-written zip.
/// Never overwrites: an existing `dest` is an error.
pub fn compress_items(items: &[PathBuf], dest: &Path) -> io::Result<()> {
    let invalid = |m: &str| io::Error::new(io::ErrorKind::InvalidInput, m.to_owned());
    let parent = items.first().and_then(|p| p.parent()).ok_or_else(|| invalid("nothing to compress"))?;
    let mut names = Vec::with_capacity(items.len());
    for p in items {
        if p.parent() != Some(parent) {
            return Err(invalid("the items are not all in the same folder"));
        }
        names.push(arg_safe(p.file_name().ok_or_else(|| invalid("an item has no name"))?));
    }
    if fs::symlink_metadata(dest).is_ok() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "the destination already exists"));
    }
    let tmp = zip_temp(dest);
    journal_add("part", &tmp, dest);
    let result = run_first_available(compress_tools(parent, items, &names, &tmp)).and_then(|_| {
        if tmp.is_file() {
            rename_retry(&tmp, dest)
        } else {
            Err(io::Error::other("the ZIP tool did not create an archive"))
        }
    });
    if result.is_err() {
        remove_temp(&tmp); // don't leave the partial archive behind
    }
    journal_settle(&tmp);
    result
}

/// Unpacks `zip` into the new folder `dest` (same temp-then-rename safety as `compress_items`).
/// Never merges into or overwrites an existing `dest`.
pub fn extract_zip(zip: &Path, dest: &Path) -> io::Result<()> {
    if fs::symlink_metadata(dest).is_ok() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "the destination already exists"));
    }
    let tmp = free_sibling(dest, "part");
    journal_add("part", &tmp, dest);
    if let Err(e) = fs::create_dir(&tmp) {
        journal_settle(&tmp);
        return Err(e);
    }
    let result = run_first_available(extract_tools(zip, &tmp)).and_then(|_| rename_retry(&tmp, dest));
    if result.is_err() {
        let _ = remove_existing(&tmp);
    }
    journal_settle(&tmp);
    result
}

// ---------------------------------------------------------------------------------------------
// Journal of temporary items, so that a crash or kill can be cleaned up on the next start (see the
// table at the top of this file).
// ---------------------------------------------------------------------------------------------

const JOURNAL_PREFIX: &str = "FileFly-journal-";

struct Journal {
    /// Opened lazily on the first temp item; the exclusive lock on it lives exactly as long as this
    /// process, which is how a later start can tell a dead run from a running one.
    file: Option<fs::File>,
    path: Option<PathBuf>,
    /// Temp items that exist (or may exist) right now.
    pending: Vec<PathBuf>,
}

static JOURNAL: std::sync::Mutex<Journal> = std::sync::Mutex::new(Journal { file: None, path: None, pending: Vec::new() });

fn journal() -> std::sync::MutexGuard<'static, Journal> {
    JOURNAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn line_safe(p: &Path) -> Option<&str> {
    p.to_str().filter(|s| !s.contains(['\t', '\n', '\r']))
}

/// Records `tmp` (about to be created) and the final path it is meant for. Best effort: when the
/// journal can't be written the operation itself still runs, it just isn't crash-cleaned.
fn journal_add(tag: &str, tmp: &Path, dest: &Path) {
    let (Some(t), Some(d)) = (line_safe(tmp), line_safe(dest)) else { return };
    let mut j = journal();
    if j.file.is_none() {
        let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let path = std::env::temp_dir().join(format!("{JOURNAL_PREFIX}{}-{stamp}.log", std::process::id()));
        let Ok(f) = fs::OpenOptions::new().read(true).write(true).create_new(true).open(&path) else { return };
        if f.try_lock().is_err() {
            let _ = fs::remove_file(&path);
            return;
        }
        j.file = Some(f);
        j.path = Some(path);
    }
    let Some(f) = j.file.as_mut() else { return };
    if f.write_all(format!("+\t{tag}\t{t}\t{d}\n").as_bytes()).is_ok() {
        j.pending.push(tmp.to_path_buf());
    }
}

/// Strikes `tmp` out of the journal, but only when it is really gone from the disk.
fn journal_settle(tmp: &Path) {
    if fs::symlink_metadata(tmp).is_ok() {
        return;
    }
    let mut j = journal();
    let Some(i) = j.pending.iter().position(|p| p == tmp) else { return };
    j.pending.swap_remove(i);
    if let (Some(t), Some(f)) = (line_safe(tmp), j.file.as_mut()) {
        let _ = f.write_all(format!("-\t{t}\n").as_bytes());
    }
}

/// Called on a clean exit: the journal is deleted when nothing is pending. If the window is closed
/// in the middle of a copy, it stays so that the next start removes the half-written item.
pub fn finish_journal() {
    let mut j = journal();
    if !j.pending.is_empty() {
        return;
    }
    drop(j.file.take()); // (release the handle before deleting the file)
    if let Some(path) = j.path.take() {
        let _ = fs::remove_file(path);
    }
}

/// `<dot><name>.<tag>-<pid>-<n>` (optionally followed by ".zip" for archives) with the pid of the
/// run that created it: the exact shape `free_sibling` produces, nothing else.
fn is_our_temp(path: &Path, tag: &str, pid: u32) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return false };
    if !name.starts_with('.') {
        return false;
    }
    let name = name.strip_suffix(".zip").unwrap_or(name);
    let Some(i) = name.rfind(&format!(".{tag}-")) else { return false };
    let mut parts = name[i + tag.len() + 2..].split('-');
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    match (parts.next(), parts.next(), parts.next()) {
        (Some(a), Some(b), None) => a == pid.to_string() && digits(b),
        _ => false,
    }
}

/// Returns true when the entry is dealt with (cleaned, or nothing there, or not ours to touch).
fn clean_leftover(tag: &str, tmp: &Path, dest: &Path, pid: u32) -> bool {
    if !is_our_temp(tmp, tag, pid) || fs::symlink_metadata(tmp).is_err() {
        return true;
    }
    match tag {
        "part" => remove_existing(tmp).is_ok(),
        "replaced" => {
            if fs::symlink_metadata(dest).is_err() {
                fs::rename(tmp, dest).is_ok() // the paste never completed: the original goes back
            } else {
                remove_existing(tmp).is_ok() // the replacement is in place: the old one is garbage
            }
        }
        _ => true,
    }
}

/// Cleans up what crashed or killed runs left behind. Blocking (can take a while for big copies):
/// call it from a worker thread at startup.
pub fn sweep_leftover_temps() {
    let Ok(rd) = fs::read_dir(std::env::temp_dir()) else { return };
    for item in rd.flatten() {
        let name = item.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix(JOURNAL_PREFIX).and_then(|r| r.strip_suffix(".log")) else { continue };
        let Some(pid) = rest.split('-').next().and_then(|p| p.parse::<u32>().ok()) else { continue };
        let path = item.path();
        let Ok(mut f) = fs::OpenOptions::new().read(true).write(true).open(&path) else { continue };
        if f.try_lock().is_err() {
            continue; // locked: that run is still alive (our own journal lands here too)
        }
        let mut text = String::new();
        if f.read_to_string(&mut text).is_err() {
            continue;
        }
        drop(f);

        let mut open: Vec<(String, String, String)> = Vec::new();
        for line in text.lines() {
            let mut c = line.split('\t');
            match (c.next(), c.next(), c.next(), c.next()) {
                (Some("+"), Some(tag), Some(tmp), Some(dest)) => open.push((tag.into(), tmp.into(), dest.into())),
                (Some("-"), Some(tmp), None, None) => open.retain(|e| e.1 != tmp),
                _ => {}
            }
        }
        let failed: Vec<_> = open
            .into_iter()
            .filter(|(tag, tmp, dest)| !clean_leftover(tag, Path::new(tmp), Path::new(dest), pid))
            .collect();
        if failed.is_empty() {
            let _ = fs::remove_file(&path);
        } else {
            // (e.g. the item is still locked by antivirus) keep just those for the next start
            let keep: String = failed.iter().map(|(a, b, c)| format!("+\t{a}\t{b}\t{c}\n")).collect();
            let _ = fs::write(&path, keep);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Browsing a zip like a folder. The archive is unpacked once into a per-process cache folder; tabs
// navigate in that real folder, while everything the user sees (address bar, breadcrumbs, tab
// title, "Up") is translated back to "<zip path>\inner\folder" by `display_path`.
// ---------------------------------------------------------------------------------------------

struct Mount {
    /// The zip file (canonical path; may itself live inside another mount's cache folder).
    zip: PathBuf,
    /// Where it was unpacked.
    root: PathBuf,
    /// Size and modification time of the zip when it was unpacked; a changed zip is unpacked anew.
    len: u64,
    modified: Option<SystemTime>,
}

static MOUNTS: std::sync::Mutex<Vec<Mount>> = std::sync::Mutex::new(Vec::new());
static MOUNT_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
const CACHE_PREFIX: &str = "FileFly-archives-";

fn mounts() -> std::sync::MutexGuard<'static, Vec<Mount>> {
    MOUNTS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Held (exclusively locked) for as long as this process lives; see `sweep_old_archive_caches`.
static CACHE_LOCK: std::sync::Mutex<Option<fs::File>> = std::sync::Mutex::new(None);
const CACHE_LOCK_NAME: &str = ".in-use";

fn hold_cache_lock() {
    let mut g = CACHE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        if let Ok(f) = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(cache_root().join(CACHE_LOCK_NAME)) {
            if f.try_lock().is_ok() {
                *g = Some(f);
            }
        }
    }
}

fn cache_root() -> PathBuf {
    std::env::temp_dir().join(format!("{CACHE_PREFIX}{}", std::process::id()))
}

fn join_rel(base: &Path, rel: &Path) -> PathBuf {
    if rel.as_os_str().is_empty() { base.to_path_buf() } else { base.join(rel) }
}

/// The unpacked folder of `zip` if it was unpacked before and the zip has not changed since.
fn fresh_mount_root(zip: &Path) -> Option<PathBuf> {
    let md = fs::metadata(zip).ok()?;
    let modified = md.modified().ok();
    let ms = mounts();
    let found = ms
        .iter()
        .rev()
        .find(|m| m.zip == zip && m.len == md.len() && m.modified == modified && m.root.is_dir())
        .map(|m| m.root.clone());
    found
}

pub enum ArchivePath {
    /// A normal path (possibly translated into an already unpacked archive).
    Real(PathBuf),
    /// The path points into (or at) a zip that still has to be unpacked with `mount_archive`;
    /// `rel` is the part inside the zip.
    Extract { zip: PathBuf, rel: PathBuf },
}

/// The first zip *file* on the way down `path` and the remainder of the path after it.
fn find_zip_in(path: &Path) -> Option<(PathBuf, PathBuf)> {
    let comps: Vec<_> = path.components().collect();
    let mut prefix = PathBuf::new();
    for (i, c) in comps.iter().enumerate() {
        prefix.push(c.as_os_str());
        if has_zip_ext(&prefix) && prefix.is_file() {
            let rel: PathBuf = comps[i + 1..].iter().collect();
            return Some((prefix, rel));
        }
    }
    None
}

/// Turns a path that may run through zip files ("C:\a\b.zip\docs") into a real one. Archives that
/// are not unpacked yet are reported instead of being unpacked here (unpacking takes a while).
pub fn resolve_archive_path(path: &Path) -> ArchivePath {
    let mut path = path.to_path_buf();
    for _ in 0..8 {
        // (zips inside zips: after one translation the new path may run through another zip)
        let Some((prefix, rel)) = find_zip_in(&path) else {
            return ArchivePath::Real(path);
        };
        let zip = dunce::canonicalize(&prefix).unwrap_or(prefix);
        match fresh_mount_root(&zip) {
            Some(root) => path = join_rel(&root, &rel),
            None => return ArchivePath::Extract { zip, rel },
        }
    }
    ArchivePath::Real(path)
}

/// True if `path` lies inside the temporary unpacked copy of a zip.
pub fn is_archive_path(path: &Path) -> bool {
    archive_of(path).is_some()
}

/// Unpacks `zip` (or reuses the unpacking from earlier) and returns the folder it was unpacked to.
/// Blocking: call it from a worker thread.
pub fn mount_archive(zip: &Path) -> io::Result<PathBuf> {
    if let Some(root) = fresh_mount_root(zip) {
        return Ok(root);
    }
    let md = fs::metadata(zip)?;
    let base = cache_root().join(MOUNT_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed).to_string());
    fs::create_dir_all(&base)?;
    hold_cache_lock();
    // Canonical, so it compares equal to what the folder tabs get from `canonicalize` (8.3 names on
    // Windows, /var -> /private/var on macOS).
    let base = dunce::canonicalize(&base)?;
    let stem = zip.file_stem().map(|s| s.to_os_string()).unwrap_or_else(|| "archive".into());
    let root = base.join(stem);
    if let Err(e) = extract_zip(zip, &root) {
        let _ = fs::remove_dir_all(&base);
        return Err(e);
    }
    mounts().push(Mount { zip: zip.to_path_buf(), root: root.clone(), len: md.len(), modified: md.modified().ok() });
    Ok(root)
}

/// The path the user should see for `real`: inside an unpacked archive that is
/// "<zip path>\<inner path>", anywhere else the path itself.
pub fn display_path(real: &Path) -> PathBuf {
    let ms = mounts();
    if ms.is_empty() {
        return real.to_path_buf();
    }
    let mut p = real.to_path_buf();
    for _ in 0..8 {
        let Some(m) = ms.iter().filter(|m| p.starts_with(&m.root)).max_by_key(|m| m.root.as_os_str().len()) else {
            break;
        };
        let rel = p.strip_prefix(&m.root).map(Path::to_path_buf).unwrap_or_default();
        p = join_rel(&m.zip, &rel);
    }
    p
}

/// The zip file whose unpacked contents `path` is part of, if any.
pub fn archive_of(path: &Path) -> Option<PathBuf> {
    let ms = mounts();
    let found = ms
        .iter()
        .filter(|m| path.starts_with(&m.root))
        .max_by_key(|m| m.root.as_os_str().len())
        .map(|m| m.zip.clone());
    found
}

/// A real folder next to the outermost zip that `real` is inside of (`real` itself when it is not
/// inside an archive). Used for what has to be written to disk: sessions, "Extract all".
pub fn folder_outside(real: &Path) -> PathBuf {
    if archive_of(real).is_none() {
        return real.to_path_buf();
    }
    let mut p = display_path(real);
    while !p.is_dir() {
        match p.parent() {
            Some(q) => p = q.to_path_buf(),
            None => break,
        }
    }
    p
}

/// Deletes everything unpacked by this process (called on exit).
pub fn remove_archive_cache() {
    drop(CACHE_LOCK.lock().unwrap_or_else(|e| e.into_inner()).take());
    let _ = fs::remove_dir_all(cache_root());
}

/// Removes caches that a crashed or killed instance left in the temp folder. A cache whose `.in-use`
/// lock can be taken belongs to a dead run and goes right away; one that is still locked belongs to
/// another running FileFly window and is left alone. Caches without a lock file (from older
/// versions) are only removed once they are a week old.
pub fn sweep_old_archive_caches() {
    let Ok(rd) = fs::read_dir(std::env::temp_dir()) else { return };
    let ours = cache_root();
    for item in rd.flatten() {
        if !item.file_name().to_string_lossy().starts_with(CACHE_PREFIX) || item.path() == ours {
            continue;
        }
        let dir = item.path();
        let stale = match fs::OpenOptions::new().read(true).write(true).open(dir.join(CACHE_LOCK_NAME)) {
            Ok(lock) => lock.try_lock().is_ok(), // (dropped at the end of this arm's scope: before the delete below)
            Err(_) => item
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > Duration::from_secs(7 * 24 * 3600)),
        };
        if stale {
            let _ = fs::remove_dir_all(dir);
        }
    }
}
