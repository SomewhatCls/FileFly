//! Copy/move (paste) worker logic and conflict-free naming.

use crate::prelude::*;

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

/// Two paste items overlap when either touches (as source or destination) something inside the
/// other's source or destination. Items that overlap must not run at the same time.
pub(crate) fn paste_overlaps(a: &PasteItem, b: &PasteItem) -> bool {
    [&a.src, &a.dst].iter().any(|x| [&b.src, &b.dst].iter().any(|y| paths_related(x, y)))
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
) -> std::io::Result<()> {
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
    /// Stopped by a cancel (of the whole operation or of just this item).
    Stopped,
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
                return Pasted::Failed(format!("Could not replace “{name}”: {e}"));
            }
        }
    }
    let r = if cut {
        move_item_with(&it.src, &it.dst, prog)
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
        Ok(()) => Pasted::Done,
        Err(e) if is_cancelled(&e) => Pasted::Stopped,
        Err(e) => Pasted::Failed(format!("Could not paste “{name}”: {e}")),
    }
}

pub(crate) fn occupied(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}

/// "name (n).ext" with the lowest n that is free on disk and not claimed by this paste.
pub(crate) fn lowest_free_name(dest: &Path, taken: &HashSet<PathBuf>, is_dir: bool) -> PathBuf {
    let parent = dest.parent().map(Path::to_path_buf).unwrap_or_default();
    // OsStrings throughout, so a name that is not valid Unicode is kept byte for byte.
    let name = dest.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    let (stem, ext) = if is_dir {
        (name.clone(), None)
    } else {
        let p = Path::new(&name);
        (
            p.file_stem().map(|s| s.to_os_string()).unwrap_or_else(|| name.clone()),
            p.extension().map(|e| e.to_os_string()),
        )
    };
    let mut n = 1u32;
    loop {
        let mut file = stem.clone();
        file.push(format!(" ({n})"));
        if let Some(e) = &ext {
            file.push(".");
            file.push(e);
        }
        let cand = parent.join(file);
        if !occupied(&cand) && !taken.contains(&cand) {
            return cand;
        }
        n += 1;
    }
}
