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
//!  * a network share: the folder itself only; recursive watches over SMB/NFS are slow and unreliable;
//!  * without an indexer (`DirWatcher::ui_only`): only the listed folder, never recursive, and the
//!    watches of earlier folders are dropped; nothing needs deep size changes then.
//!
//! When a watch is dropped (limit reached) the folder it covered can no longer be trusted blindly,
//! so it is reported to the updater as "everything below here may have changed" and gets rescanned
//! the next time it is listed.

use super::live::{is_noisy, ChangeSink};
use super::priority::lower_priority;
use crate::waker::Waker;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{mpsc, Arc, Mutex},
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

    /// Does this group keep `dir` watched, as its own folder or through a recursive watch?
    fn watches_folder(&self, dir: &Path) -> bool {
        self.anchor == dir || self.covers(dir)
    }
}

/// The group to drop when there are too many: the least recently used one, but never the newest (the last) and
/// never one that keeps a shown folder watched. None when every other group is needed.
fn eviction_victim(groups: &[Group], shown: &[PathBuf]) -> Option<usize> {
    let last = groups.len().checked_sub(1)?;
    (0..last)
        .filter(|&i| !shown.iter().any(|d| groups[i].watches_folder(d)))
        .min_by_key(|&i| groups[i].used)
}

pub struct DirWatcher {
    /// Folders to watch go to a background thread: on Linux, a recursive watch means one
    /// `inotify_add_watch` per sub-folder, which can take seconds and must not block the UI.
    /// Each message is the complete set of folders the windows show.
    tx: mpsc::Sender<Vec<PathBuf>>,
    pending: Arc<Mutex<HashSet<PathBuf>>>,
}

/// The state that decides what is watched; owned by the watcher thread.
struct Inner {
    watcher: RecommendedWatcher,
    groups: Vec<Group>,
    current: Option<PathBuf>,
    /// The folders the windows show right now (the latest request): their watches are never dropped to stay
    /// within `MAX_GROUPS`, or a window would silently stop updating.
    shown: Vec<PathBuf>,
    sink: Option<Arc<ChangeSink>>,
}

impl DirWatcher {
    /// Refreshes the UI because of changes and reports folder/file size changes.
    pub fn new(waker: Waker, sink: Arc<ChangeSink>) -> Option<Self> {
        Self::build(waker, Some(sink))
    }

    /// Only refreshes the UI because of changes: no size reporting, no recursive watches.
    /// Used while folder sizes are turned off.
    pub fn ui_only(waker: Waker) -> Option<Self> {
        Self::build(waker, None)
    }

    fn build(waker: Waker, sink: Option<Arc<ChangeSink>>) -> Option<Self> {
        // Events go into a bounded set that the UI drains once per frame. (An unbounded channel
        // grows without limit while the window is minimised or hidden and nobody reads it.)
        let pending: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
        let ui_set = Arc::clone(&pending);
        let live = sink.clone();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else {
                // An error may mean lost events: stop trusting what the watches vouched for.
                if let Some(s) = &live {
                    s.revoke_trust();
                }
                return;
            };
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
                    waker.wake_after(Duration::from_millis(300));
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
            waker.wake_after(Duration::from_millis(300));
        })
            .ok()?;
        let mut inner = Inner { watcher, groups: Vec::new(), current: None, shown: Vec::new(), sink };
        let (tx, rx) = mpsc::channel::<Vec<PathBuf>>();
        std::thread::Builder::new()
            .name("dir-watch".into())
            .spawn(move || {
                lower_priority();
                // Only the newest request matters: quick navigation must not queue up several walks.
                while let Ok(mut dirs) = rx.recv() {
                    while let Ok(newer) = rx.try_recv() {
                        dirs = newer;
                    }
                    inner.shown = dirs.clone();
                    for dir in &dirs {
                        inner.watch(dir);
                    }
                }
            })
            .ok()?;
        Some(Self { tx, pending })
    }

    /// Changed paths since the last call. An empty path means "too many to list".
    pub fn take(&self) -> Vec<PathBuf> {
        match self.pending.lock() {
            Ok(mut s) => std::mem::take(&mut *s).into_iter().collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Call whenever a folder shown by any window changes; `dirs` is every window's folder, the one that
    /// changed last. Returns immediately; the watches are set up in the background. Each call refreshes the
    /// watches of all of them, so a folder one window keeps showing is not the least recently used one.
    pub fn watch(&self, dirs: Vec<PathBuf>) {
        let _ = self.tx.send(dirs);
    }
}

impl Inner {
    fn watch(&mut self, dir: &Path) {
        if self.current.as_deref() == Some(dir) {
            return;
        }
        // Without a sink nobody needs deep size changes, so every watch is shallow.
        let root = dir.parent().is_none();
        let network = crate::drives::is_network_path(dir);
        let shallow = self.sink.is_none() || root || network;

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

        let Some(group) = self.open_group(dir, shallow, root && !network) else { return };
        if let Some(s) = &self.sink {
            for (p, deep) in &group.watches {
                if *deep {
                    s.watch_deep(p); // from now on a folder scanned below it is vouched for
                }
            }
        }

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

        if self.sink.is_none() {
            // Only the folder on screen needs a watch; the rest would just cause needless refreshes.
            let (keep, old): (Vec<_>, Vec<_>) =
                std::mem::take(&mut self.groups).into_iter().partition(|g| g.anchor == dir);
            self.groups = keep;
            for g in &old {
                self.drop_group(g, false);
            }
        }

        // Too many remembered folders: drop the least recently used one (never the newest, and never one that
        // keeps a shown folder watched: with many windows open the limit may be exceeded by those).
        while self.groups.len() > MAX_GROUPS {
            let Some(i) = eviction_victim(&self.groups, &self.shown) else { break };
            let old = self.groups.remove(i);
            self.drop_group(&old, true);
        }
    }

    /// `with_children`: also watch the root's sub-folders recursively (a local drive root).
    fn open_group(&mut self, dir: &Path, shallow: bool, with_children: bool) -> Option<Group> {
        let mut watches: Vec<(PathBuf, bool)> = Vec::new();
        if shallow {
            self.watcher.watch(dir, RecursiveMode::NonRecursive).ok()?;
            watches.push((dir.to_path_buf(), false));
            // The recursive file watcher causes crashing on Linux, so only use it on WIndows.
            // (Also only with an indexer: the children are watched for size changes.)
            if with_children && cfg!(windows) && self.sink.is_some() {
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
        for (p, deep) in &g.watches {
            let _ = self.watcher.unwatch(p);
            if *deep {
                if let Some(s) = &self.sink {
                    s.unwatch_deep(p);
                    if lost_coverage {
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
#[cfg(test)]
mod tests {
    use super::*;

    fn group(anchor: &str, deep: bool) -> Group {
        Group { anchor: PathBuf::from(anchor), watches: vec![(PathBuf::from(anchor), deep)], used: Instant::now() }
    }

    #[test]
    fn a_group_watches_its_own_folder_and_everything_below_a_recursive_one() {
        let deep = group("C:/data", true);
        assert!(deep.watches_folder(Path::new("C:/data")));
        assert!(deep.watches_folder(Path::new("C:/data/sub/x")));
        assert!(!deep.watches_folder(Path::new("C:/other")));
        let shallow = group("C:/", false);
        assert!(shallow.watches_folder(Path::new("C:/")));
        assert!(!shallow.watches_folder(Path::new("C:/data")), "a shallow watch covers only its own folder");
    }

    #[test]
    fn the_oldest_group_goes_unless_a_window_still_shows_it() {
        let mut groups: Vec<Group> = ["C:/a", "C:/b", "C:/c", "C:/d"].iter().map(|a| group(a, true)).collect();
        for (i, g) in groups.iter_mut().enumerate() {
            g.used += Duration::from_millis(i as u64); // a is the oldest
        }
        assert_eq!(eviction_victim(&groups, &[]), Some(0));
        // a and b are on screen: the next oldest that is not goes (c); the newest (d) never does.
        let shown = [PathBuf::from("C:/a/inner"), PathBuf::from("C:/b")];
        assert_eq!(eviction_victim(&groups, &shown), Some(2));
        let all = [PathBuf::from("C:/a"), PathBuf::from("C:/b"), PathBuf::from("C:/c")];
        assert_eq!(eviction_victim(&groups, &all), None, "everything left is shown or newest: the limit is exceeded");
    }
}
