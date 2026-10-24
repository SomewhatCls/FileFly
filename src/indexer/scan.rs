//! The folder walk: sums file sizes, reuses fresh cached sub-folders, stores what it finishes.

use super::{
    hot::HotFiles,
    model::{is_fresh, ns, now_secs, CachedFolder},
    store::Cache,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};

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

/// Size of one file. A file that changed a moment ago ("hot", see `hot.rs`) is asked directly,
/// because the size that came with the directory listing may be stale; everything else uses the
/// listing, which costs no extra system call.
fn file_len(de: &fs::DirEntry, hot: &HotFiles) -> u64 {
    if hot.is_active() {
        let p = de.path();
        if hot.contains(&p) {
            return fs::symlink_metadata(&p).map(|m| m.len()).unwrap_or(0);
        }
    }
    de.metadata().map(|m| m.len()).unwrap_or(0)
}

/// Iterative post-order walk. Returns None if cancelled. Every folder that is finished is stored.
pub fn scan(
    root: &Path,
    root_stamp: u64,
    cache: &Cache,
    generation: &AtomicU64,
    my_gen: u64,
    dirty: &AtomicBool,
    hot: &HotFiles,
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
    let mut stack = vec![Frame { path: root.to_path_buf(), rd, total: 0, stamp: root_stamp }];
    let mut counter = 0u32;

    loop {
        if generation.load(Ordering::Relaxed) != my_gen {
            flush(&mut pending); // keep the completed sub-folders
            return None;
        }
        counter += 1;
        if counter % 4096 == 0 {
            // limit rewrites
            if pending.len() >= 4000 {
                flush(&mut pending);
            }
            // Prevents full hardware usage.
            std::thread::sleep(Duration::from_millis(2));
        }

        let step = {
            let top = stack.last_mut()?;
            match top.rd.next() {
                None => Step::End,
                Some(Err(_)) => Step::Skip,
                Some(Ok(de)) => match de.file_type() {
                    Ok(ft) if ft.is_symlink() => Step::Skip, // symlinks & junctions: avoid loops/double counting
                    Ok(ft) if ft.is_file() => Step::Add(file_len(&de, hot)),
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
