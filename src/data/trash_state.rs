//! State and helpers of the Recycle Bin view.

use crate::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrashSort {
    Name,
    Location,
    Deleted,
    Size,
}

pub(crate) struct TrashState {
    pub(crate) rows: Vec<trash_bin::TrashRow>,
    /// Which row shows which trashed folder (the indexer reports sizes by path).
    pub(crate) dir_rows: HashMap<PathBuf, usize>,
    pub(crate) view: Vec<usize>,
    pub(crate) selected: HashSet<usize>,
    pub(crate) anchor: Option<usize>,
    pub(crate) loading: bool,
    pub(crate) seq: u64,
    pub(crate) error: Option<String>,
    pub(crate) sort_col: TrashSort,
    pub(crate) sort_asc: bool,
    pub(crate) needs_sort: bool,
    pub(crate) scroll_to: Option<usize>,
}

impl TrashState {
    pub(crate) fn new() -> Self {
        Self {
            rows: Vec::new(),
            dir_rows: HashMap::new(),
            view: Vec::new(),
            selected: HashSet::new(),
            anchor: None,
            loading: true,
            seq: 0,
            error: None,
            sort_col: TrashSort::Deleted,
            sort_asc: false,
            needs_sort: false,
            scroll_to: None,
        }
    }
}

pub(crate) struct ConfirmPurge {
    pub(crate) items: Vec<trash_bin::Item>,
    pub(crate) text: String,
    /// Keeps `items` marked as busy while the question is open (so they can't be restored in the
    /// meantime). On "Delete", move it into the job that purges them, so they stay busy until it ends.
    pub(crate) guard: BusyGuard,
}

/// A restore waiting for the user to decide what to do with each item whose place is taken.
pub(crate) struct PendingRestore {
    /// Items that need no decision (or were already decided).
    pub(crate) ready: Vec<(trash_bin::Item, trash_bin::Clash)>,
    /// (item, is a folder) whose original path is taken; the front one is being asked about.
    pub(crate) todo: VecDeque<(trash_bin::Item, bool)>,
    /// "Do this for all remaining conflicts" was ticked.
    pub(crate) apply_all: bool,
    /// Keeps all the items busy while the question is open; moves into the job that restores them.
    pub(crate) guard: BusyGuard,
}

/// Marks Recycle Bin items as being worked on (`Explorer::trash_busy`) for as long as it lives: they
/// can't be restored or deleted a second time. Dropping it releases them, however the job ends
/// (finished, failed, cancelled, or panicked).
pub(crate) struct BusyGuard {
    set: Arc<std::sync::Mutex<HashSet<std::ffi::OsString>>>,
    ids: Vec<std::ffi::OsString>,
}

impl BusyGuard {
    pub(crate) fn new(set: Arc<std::sync::Mutex<HashSet<std::ffi::OsString>>>, ids: Vec<std::ffi::OsString>) -> Self {
        set.lock().unwrap_or_else(|e| e.into_inner()).extend(ids.iter().cloned());
        Self { set, ids }
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        let mut set = self.set.lock().unwrap_or_else(|e| e.into_inner());
        for id in &self.ids {
            set.remove(id);
        }
    }
}

pub(crate) fn trash_move(tr: &mut TrashState, delta: i32) {
    if tr.view.is_empty() {
        return;
    }
    let cur = tr.anchor.filter(|&a| a < tr.view.len());
    let next = match cur {
        Some(c) => (c as i32 + delta).clamp(0, tr.view.len() as i32 - 1) as usize,
        None => 0,
    };
    tr.selected.clear();
    tr.selected.insert(tr.view[next]);
    tr.anchor = Some(next);
    tr.scroll_to = Some(next);
}

pub(crate) fn trash_edge(tr: &mut TrashState, last: bool) {
    if tr.view.is_empty() {
        return;
    }
    let i = if last { tr.view.len() - 1 } else { 0 };
    tr.selected.clear();
    tr.selected.insert(tr.view[i]);
    tr.anchor = Some(i);
    tr.scroll_to = Some(i);
}

pub(crate) fn rebuild_trash_view(tr: &mut TrashState, search: &str) {
    let q = search.trim().to_lowercase();
    let rows = &tr.rows;
    tr.view.clear();
    tr.view.extend((0..rows.len()).filter(|&i| {
        q.is_empty() || rows[i].name_lc.contains(&q) || rows[i].location_lc.contains(&q)
    }));
    let (col, asc) = (tr.sort_col, tr.sort_asc);
    tr.view.sort_by(|&a, &b| {
        let (ra, rb) = (&rows[a], &rows[b]);
        if col == TrashSort::Size {
            match rb.is_dir.cmp(&ra.is_dir) {
                Ordering::Equal => {}
                o => return o,
            }
        }
        let o = match col {
            TrashSort::Name => natural_cmp(&ra.name_lc, &rb.name_lc),
            TrashSort::Location => ra
                .location_lc
                .cmp(&rb.location_lc)
                .then_with(|| natural_cmp(&ra.name_lc, &rb.name_lc)),
            TrashSort::Deleted => ra.deleted.cmp(&rb.deleted),
            TrashSort::Size => ra
                .size
                .cmp(&rb.size)
                .then_with(|| natural_cmp(&ra.name_lc, &rb.name_lc)),
        };
        if asc {
            o
        } else {
            o.reverse()
        }
    });
    tr.needs_sort = false;
}

/// Rebuilds `dir_rows` after the rows were replaced.
pub(crate) fn index_trash_dirs(tr: &mut TrashState) {
    tr.dir_rows = tr.rows.iter().enumerate().filter_map(|(i, r)| r.dir_path.clone().map(|p| (p, i))).collect();
}

/// Whether any of `paths` is, contains, or lies inside one of the `locked` ones (what running
/// operations are working on, see `Explorer::locked_paths`).
pub(crate) fn touches_locked(locked: &[PathBuf], paths: &[PathBuf]) -> bool {
    paths.iter().any(|p| locked.iter().any(|l| paths_related(p, l)))
}
