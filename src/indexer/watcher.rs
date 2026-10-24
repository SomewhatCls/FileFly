//! File watcher: tells the UI which paths changed, and feeds the live updater (`live.rs`).
//!
//! What is watched (this is what makes size changes visible *deep* inside listed folders):
//!  * a normal folder: recursively, so a change at any depth is seen;
//!  * navigating into a sub-folder of a watched folder costs nothing, it is already covered;
//!  * navigating somewhere else keeps the earlier watches (up to `MAX_GROUPS`, least recently
//!    used dropped first), so going back up does not mean "unknown changes since I left";
//!  * a drive root: the root itself, plus each of its direct child folders recursively
//!    (up to `MAX_CHILD_WATCHES`). Watching a whole drive recursively would drown in system churn,
//!    but the listing only sizes the children, so those are what matter. `Windows`, the recycle
//!    bin and similar noise are skipped (their sizes stay on the TTL fallback);
//!  * a network share: the folder itself only; recursive watches over SMB/NFS are slow and unreliable.
//!
//! When a watch is dropped (limit reached) the folder it covered can no longer be trusted blindly,
//! so it is reported to the updater as "everything below here may have changed" and gets rescanned
//! the next time it is listed.

use super::live::{is_noisy, ChangeSink};
use eframe::egui;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// Most distinct changed paths remembered between two UI frames; more than that collapses into
/// a single "something changed" marker (the empty path).
const MAX_PENDING: usize = 2048;
/// Folders (and their watches) kept after navigating away.
const MAX_GROUPS: usize = 8;
/// Child folders of a drive root that get their own recursive watch.
const MAX_CHILD_WATCHES: usize = 48;

/// The watches that belong to one visited folder.
struct Group {
    anchor: PathBuf,
    /// (path, recursive)
    watches: Vec<(PathBuf, bool)>,
    used: Instant,
}

impl Group {
    /// Is `dir` inside one of this group's recursive watches?
    fn covers(&self, dir: &Path) -> bool {
        self.watches.iter().any(|(p, deep)| *deep && dir.starts_with(p))
    }
}

pub struct DirWatcher {
    watcher: RecommendedWatcher,
    groups: Vec<Group>,
    current: Option<PathBuf>,
    pending: Arc<Mutex<HashSet<PathBuf>>>,
    sink: Option<Arc<ChangeSink>>,
}

impl DirWatcher {
    /// Refreshes the UI because of changes and reports folder/file size changes.
    pub fn new(ctx: egui::Context, sink: Arc<ChangeSink>) -> Option<Self> {
        Self::build(ctx, Some(sink))
    }

    fn build(ctx: egui::Context, sink: Option<Arc<ChangeSink>>) -> Option<Self> {
        // Events go into a bounded set that the UI drains once per frame. (An unbounded channel
        // grows without limit while the window is minimised or hidden and nobody reads it.)
        let pending: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
        let ui_set = Arc::clone(&pending);
        let live = sink.clone();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else { return };
            if matches!(ev.kind, EventKind::Access(_)) {
                return;
            }
            if ev.paths.is_empty() {
                // notify reports lost events (buffer overflow) as a path-less `Other` event.
                if matches!(ev.kind, EventKind::Other) {
                    if let Some(s) = &live {
                        s.push_overflow();
                    }
                    if let Ok(mut set) = ui_set.lock() {
                        set.insert(PathBuf::new());
                    }
                    ctx.request_repaint_after(Duration::from_millis(300));
                }
                return;
            }
            // The updater ignores the index file and system noise; the UI sees everything.
            if let Some(s) = &live {
                let fwd: Vec<PathBuf> = ev.paths.iter().filter(|p| s.accepts(p)).cloned().collect();
                if !fwd.is_empty() {
                    s.push(fwd);
                }
            }
            if let Ok(mut set) = ui_set.lock() {
                for p in ev.paths {
                    if set.len() < MAX_PENDING {
                        set.insert(p);
                    } else {
                        set.insert(PathBuf::new());
                        break;
                    }
                }
            }
            ctx.request_repaint_after(Duration::from_millis(300));
        })
        .ok()?;
        Some(Self { watcher, groups: Vec::new(), current: None, pending, sink })
    }

    /// Changed paths since the last call. An empty path means "too many to list".
    pub fn take(&self) -> Vec<PathBuf> {
        match self.pending.lock() {
            Ok(mut s) => std::mem::take(&mut *s).into_iter().collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Call whenever the listed folder changes.
    pub fn watch(&mut self, dir: &Path) {
        if self.current.as_deref() == Some(dir) {
            return;
        }
        let shallow = dir.parent().is_none() || crate::drives::is_network_path(dir);

        if !shallow {
            // Already inside a recursive watch from an earlier visit? Then there is nothing to do.
            if let Some(g) = self.groups.iter_mut().find(|g| g.covers(dir)) {
                g.used = Instant::now();
                self.current = Some(dir.to_path_buf());
                return;
            }
        } else if let Some(i) = self.groups.iter().position(|g| g.anchor == dir) {
            // Revisited drive root / share: rebuild, so folders created since then get watched too.
            let old = self.groups.remove(i);
            self.drop_group(&old, false);
        }

        let Some(group) = self.open_group(dir, shallow) else { return };

        if !shallow {
            // The new recursive watch covers every older watch inside it.
            let mut i = 0;
            while i < self.groups.len() {
                if self.groups[i].anchor.starts_with(dir) {
                    let old = self.groups.remove(i);
                    self.drop_group(&old, false);
                } else {
                    i += 1;
                }
            }
        }
        self.groups.push(group);
        self.current = Some(dir.to_path_buf());

        // Too many remembered folders: drop the least recently used one (never the newest).
        while self.groups.len() > MAX_GROUPS {
            let last = self.groups.len() - 1;
            let Some(i) = (0..last).min_by_key(|&i| self.groups[i].used) else { break };
            let old = self.groups.remove(i);
            self.drop_group(&old, true);
        }
    }

    fn open_group(&mut self, dir: &Path, shallow: bool) -> Option<Group> {
        let mut watches: Vec<(PathBuf, bool)> = Vec::new();
        if shallow {
            self.watcher.watch(dir, RecursiveMode::NonRecursive).ok()?;
            watches.push((dir.to_path_buf(), false));
            // The recursive file watcher causes crashing on Linux, so only use it on WIndows.
            if dir.parent().is_none() && !crate::drives::is_network_path(dir) && cfg!(windows) {
                for child in child_dirs(dir) {
                    if watches.len() > MAX_CHILD_WATCHES {
                        break;
                    }
                    if self.watcher.watch(&child, RecursiveMode::Recursive).is_ok() {
                        watches.push((child, true));
                    }
                }
            }
        } else {
            self.watcher.watch(dir, RecursiveMode::Recursive).ok()?;
            watches.push((dir.to_path_buf(), true));
        }
        Some(Group { anchor: dir.to_path_buf(), watches, used: Instant::now() })
    }

    /// Stops the watches of `g`. With `lost_coverage` the folders they covered are reported as
    /// "may have changed", because from now on nobody is looking at them.
    fn drop_group(&mut self, g: &Group, lost_coverage: bool) {
        for (p, _) in &g.watches {
            let _ = self.watcher.unwatch(p);
        }
        if lost_coverage {
            if let Some(s) = &self.sink {
                for (p, deep) in &g.watches {
                    if *deep {
                        s.push_tree(p.clone());
                    }
                }
            }
        }
    }
}

/// Direct child folders worth watching (no links, no system noise, no `Windows`).
fn child_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    rd.flatten()
        .filter(|de| de.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|de| de.path())
        .filter(|p| !is_noisy(p) && !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("windows")))
        .collect()
}
