//! Copy/move (paste) worker logic and conflict-free naming.

use crate::prelude::*;
use std::sync::{Mutex, MutexGuard};

#[derive(Default)]
struct RegistryState {
    next: u64,
    /// (sequence number, item, whether its operation is a cut), in start order.
    pending: Vec<(u64, PasteItem, bool)>,
}

/// Every paste item that has not finished yet (waiting or running), of all operations in all windows.
/// Planning treats their destinations as taken, and an item only starts once no earlier pending
/// item (of any operation) overlaps it.
#[derive(Clone, Default)]
pub(crate) struct PasteRegistry(Arc<Mutex<RegistryState>>);

impl PasteRegistry {
    fn lock(&self) -> MutexGuard<'_, RegistryState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Registers the items of a new operation. Call it on the UI thread before the work starts,
    /// so the next paste already sees them.
    pub(crate) fn enter(&self, items: &[PasteItem], cut: bool) -> PasteTicket {
        let mut g = self.lock();
        let first = g.next;
        g.next += items.len() as u64;
        g.pending.extend(items.iter().cloned().enumerate().map(|(i, it)| (first + i as u64, it, cut)));
        PasteTicket { reg: self.clone(), first, count: items.len(), cut }
    }

    /// Destinations that pending items are about to fill: for planning another paste they are taken.
    pub(crate) fn destinations(&self) -> HashSet<PathBuf> {
        self.lock().pending.iter().map(|(_, it, _)| it.dst.clone()).collect()
    }
}

/// One operation's registration; whatever is still pending when it is dropped
/// (cancelled, failed, never started) is removed.
pub(crate) struct PasteTicket {
    reg: PasteRegistry,
    first: u64,
    count: usize,
    cut: bool,
}

impl PasteTicket {
    /// True once no earlier pending item overlaps `it` (`idx` = its index in the operation).
    pub(crate) fn may_start(&self, idx: usize, it: &PasteItem) -> bool {
        let seq = self.first + idx as u64;
        self.reg
            .lock()
            .pending
            .iter()
            .all(|(s, other, other_cut)| *s >= seq || !paste_overlaps(it, self.cut, other, *other_cut))
    }

    pub(crate) fn finish(&self, idx: usize) {
        let seq = self.first + idx as u64;
        self.reg.lock().pending.retain(|(s, _, _)| *s != seq);
    }

    /// Removes whatever of this operation is still pending. Safe to call more than once.
    pub(crate) fn release(&self) {
        let (lo, hi) = (self.first, self.first + self.count as u64);
        self.reg.lock().pending.retain(|(s, _, _)| *s < lo || *s >= hi);
    }
}

impl Drop for PasteTicket {
    fn drop(&mut self) {
        self.release();
    }
}

/// One decided paste operation.
#[derive(Clone)]
pub(crate) struct PasteItem {
    pub(crate) src: PathBuf,
    pub(crate) dst: PathBuf,
    /// Remove an existing destination first.
    pub(crate) replace: bool,
}

/// A paste that is waiting for the user to decide what to do with each name clash.
pub(crate) struct PendingPaste {
    pub(crate) cut: bool,
    /// Items that need no decision (or were already decided).
    pub(crate) ready: Vec<PasteItem>,
    /// (source, destination) pairs whose destination is already taken; the front one is being asked about.
    pub(crate) todo: VecDeque<(PathBuf, PathBuf)>,
    /// "Do this for all remaining conflicts" was ticked.
    pub(crate) apply_all: bool,
    pub(crate) merged_dirs: Vec<PathBuf>,
}

/// True if one path is the other or lies inside it (case-insensitive on Windows).
pub(crate) fn paths_related(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    let (a, b) = (
        PathBuf::from(a.to_string_lossy().to_lowercase()),
        PathBuf::from(b.to_string_lossy().to_lowercase()),
    );
    a.starts_with(&b) || b.starts_with(&a)
}

/// True if `a` and `b` are the same path (case-insensitive on Windows).
pub(crate) fn paths_equal(a: &Path, b: &Path) -> bool {
    a == b || (cfg!(windows) && a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase())
}

/// Two paste items must not run at the same time when one touches what the other writes or removes.
/// Copies may read the same source side by side; when a cut is involved the source is removed, so
/// then the sources count too.
pub(crate) fn paste_overlaps(a: &PasteItem, a_cut: bool, b: &PasteItem, b_cut: bool) -> bool {
    paths_related(&a.dst, &b.dst)
        || paths_related(&a.dst, &b.src)
        || paths_related(&a.src, &b.dst)
        || ((a_cut || b_cut) && paths_related(&a.src, &b.src))
}

/// True for a real directory (a symlink to a directory does not count, so we never merge through links).
pub(crate) fn is_real_dir(p: &Path) -> bool {
    fs::symlink_metadata(p).map(|m| m.is_dir()).unwrap_or(false)
}

/// The result of planning a paste.
#[derive(Default)]
pub(crate) struct PastePlan {
    /// Items with no clash.
    pub(crate) ready: Vec<PasteItem>,
    /// Real clashes that need a user decision.
    pub(crate) todo: VecDeque<(PathBuf, PathBuf)>,
    /// Source folders that were merged into existing ones (parents before children).
    /// For a cut, remove these afterwards if they ended up empty.
    pub(crate) merged_dirs: Vec<PathBuf>,
}

/// Decides what to do with `src` -> `dst`, merging folders that already exist at the destination.
pub(crate) fn plan_item(
    src: PathBuf,
    dst: PathBuf,
    claimed: &mut HashSet<PathBuf>,
    plan: &mut PastePlan,
) -> io::Result<()> {
    if claimed.contains(&dst) {
        plan.todo.push_back((src, dst));
        return Ok(());
    }
    if !occupied(&dst) {
        claimed.insert(dst.clone());
        plan.ready.push(PasteItem { src, dst, replace: false });
        return Ok(());
    }
    // Same-named real folders: merge. Never when one lies in the other (same-folder copy).
    if is_real_dir(&src) && is_real_dir(&dst) && !paths_related(&src, &dst) {
        plan.merged_dirs.push(src.clone());
        for entry in fs::read_dir(&src)? {
            let entry = entry?;
            plan_item(entry.path(), dst.join(entry.file_name()), claimed, plan)?;
        }
        return Ok(());
    }
    plan.todo.push_back((src, dst)); // real conflict
    Ok(())
}

/// After a cut has finished: drop source folders that were merged away and are now empty.
/// Folders that still hold skipped items are left alone (remove_dir fails on non-empty dirs).
pub(crate) fn cleanup_merged_dirs(dirs: &[PathBuf]) {
    for d in dirs.iter().rev() {
        let _ = fs::remove_dir(d);
    }
}

/// How many pasted items are worked on at the same time.
pub(crate) const PASTE_WORKERS: usize = 4;

/// What became of one pasted item.
pub(crate) enum Pasted {
    Done,
    Failed(String),
    /// Refused for lack of access rights: what "Try as administrator" can redo.
    Denied(String),
    /// Stopped by a cancel (of the whole operation or of just this item).
    Stopped,
}

impl Pasted {
    /// A failure with `msg` as its text; `cause` decides whether it counts as "denied".
    pub(crate) fn from_io(msg: String, cause: &io::Error) -> Self {
        if is_denied_io(cause) { Pasted::Denied(msg) } else { Pasted::Failed(msg) }
    }
}

/// Pastes one item (with the park-and-restore dance for "Replace").
pub(crate) fn paste_one(it: &PasteItem, cut: bool, prog: &OpProgress) -> Pasted {
    let name = it
        .dst
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // "Replace": park the old item next to the destination first and delete it only
    // once the new one is in place, so a failure or crash can't leave neither.
    let mut backup: Option<PathBuf> = None;
    if it.replace && occupied(&it.dst) {
        let aside = backup_name(&it.dst);
        match fs::rename(&it.dst, &aside) {
            Ok(()) => backup = Some(aside),
            Err(e) => {
                backup_settled(&aside);
                return Pasted::from_io(format!("Could not replace “{name}”: {e}"), &e);
            }
        }
    }
    // A move across drives can end with the item complete at the destination but its source only
    // partly removed. Then the destination is the only complete copy: it counts as in place (the
    // parked original goes), and the leftover source is reported.
    let mut source_left: Option<io::Error> = None;
    let r = if cut {
        move_item_with(&it.src, &it.dst, prog).map(|left| source_left = left)
    } else {
        copy_item_with(&it.src, &it.dst, prog)
    };
    match (&r, backup) {
        (Ok(()), Some(b)) => {
            let _ = remove_existing(&b);
            backup_settled(&b);
        }
        (Err(_), Some(b)) => {
            let _ = remove_existing(&it.dst); // anything half-written
            let _ = fs::rename(&b, &it.dst); // put the original back
            backup_settled(&b);
        }
        _ => {}
    }
    match r {
        Ok(()) => match source_left {
            None => Pasted::Done,
            // Never `Denied`: an administrator retry would run the whole paste again, and with "Replace"
            // it would throw away the complete copy that is already in place.
            Some(e) => Pasted::Failed(format!("Moved “{name}”, but could not remove the original: {e}")),
        },
        Err(e) if is_cancelled(&e) => Pasted::Stopped,
        Err(e) => Pasted::from_io(format!("Could not paste “{name}”: {e}"), &e),
    }
}
