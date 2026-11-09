//! The folder walk: sums file sizes, reuses fresh cached sub-folders, stores what it finishes.

use super::{
    hot::{same_name, HotFiles},
    model::{is_fresh, ns, now_secs, CachedFolder},
    store::Cache,
};
use std::{
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
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
