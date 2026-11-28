//! Long-running file operations: progress bookkeeping and formatting.

use crate::prelude::*;

/// What a background operation tells the user in its progress dialog.
pub(crate) struct OpSpec {
    pub(crate) title: String,
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) cancelable: bool,
}

/// A running background operation and the state of its progress dialog.
pub(crate) struct ActiveOp {
    pub(crate) id: u64,
    pub(crate) title: String,
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) cancelable: bool,
    /// What the operation reads, changes or creates. Until it is over (its entry is removed) none of
    /// this can be deleted, moved or renamed, and Recycle Bin items that came from it can't be touched.
    pub(crate) paths: Vec<PathBuf>,
    pub(crate) progress: Arc<OpProgress>,
    pub(crate) started: Instant,
    /// "More details" (speed graph, time remaining) is open.
    pub(crate) expanded: bool,
    /// The user closed the dialog of an operation that can't be cancelled.
    pub(crate) hidden: bool,
    pub(crate) cancelling: bool,
    pub(crate) last_t: Instant,
    /// The count (bytes, or items when `by_items`) at the last sample.
    pub(crate) last_bytes: u64,
    /// The operation counts items but no bytes (deleting, moving within a drive): `speed` and
    /// `history` are then items per second.
    pub(crate) by_items: bool,
    /// Smoothed bytes (or items) per second, and the raw samples the graph draws.
    pub(crate) speed: f32,
    pub(crate) history: Vec<f32>,
}

impl ActiveOp {
    pub(crate) fn new(id: u64, spec: OpSpec, progress: Arc<OpProgress>) -> Self {
        let now = Instant::now();
        Self {
            id,
            title: spec.title,
            from: spec.from,
            to: spec.to,
            cancelable: spec.cancelable,
            paths: Vec::new(),
            progress,
            started: now,
            expanded: false,
            hidden: false,
            cancelling: false,
            last_t: now,
            last_bytes: 0,
            by_items: false,
            speed: 0.0,
            history: Vec::new(),
        }
    }

    /// Takes a speed sample about four times a second: bytes per second, or items per second for
    /// an operation that counts no bytes.
    pub(crate) fn sample(&mut self, now: Instant) {
        let dt = now.duration_since(self.last_t).as_secs_f32();
        if dt < 0.25 {
            return;
        }
        let by_items =
            self.progress.total_bytes.load(AO::Relaxed) == 0 && self.progress.total_items.load(AO::Relaxed) > 1;
        let done = if by_items {
            self.progress.done_items.load(AO::Relaxed)
        } else {
            self.progress.done_bytes.load(AO::Relaxed)
        };
        if by_items != self.by_items {
            // The unit changed (the amount of work became known): start the graph over.
            self.by_items = by_items;
            self.speed = 0.0;
            self.history.clear();
            self.last_t = now;
            self.last_bytes = done;
            return;
        }
        let paused = self.progress.paused.load(AO::Relaxed);
        let inst = if paused { 0.0 } else { done.saturating_sub(self.last_bytes) as f32 / dt };
        self.speed = if self.speed <= 0.0 { inst } else { self.speed * 0.7 + inst * 0.3 };
        if paused {
            self.speed = 0.0;
        }
        self.history.push(inst);
        if self.history.len() > 120 {
            self.history.remove(0);
        }
        self.last_t = now;
        self.last_bytes = done;
    }

    /// 0..=1, or None while the amount of work isn't known (preparing, a single item, or a job without progress).
    pub(crate) fn fraction(&self) -> Option<f32> {
        let p = &self.progress;
        if p.preparing.load(AO::Relaxed) {
            return None;
        }
        let (tb, ti) = (p.total_bytes.load(AO::Relaxed), p.total_items.load(AO::Relaxed));
        if tb > 0 {
            Some((p.done_bytes.load(AO::Relaxed) as f64 / tb as f64).clamp(0.0, 1.0) as f32)
        } else if ti > 1 {
            // (a single item has no steps to count: the sliding bar says "working")
            Some((p.done_items.load(AO::Relaxed) as f64 / ti as f64).clamp(0.0, 1.0) as f32)
        } else {
            None
        }
    }
}

/// Everything the running operations work on (`ActiveOp::paths`). The one lock every window consults:
/// call it with the shared `Core::ops`, never with a per-window copy. A path is locked until its operation's
/// entry leaves the list, i.e. until the operation really ended.
pub(crate) fn locked_paths(ops: &[ActiveOp]) -> Vec<PathBuf> {
    ops.iter().flat_map(|o| o.paths.iter().cloned()).collect()
}

/// Records what operation `id` works on, until it is over. Does nothing for an unknown id.
pub(crate) fn lock_op_paths(ops: &mut [ActiveOp], id: u64, paths: impl IntoIterator<Item = PathBuf>) {
    if let Some(op) = ops.iter_mut().find(|o| o.id == id) {
        op.paths.extend(paths);
    }
}

/// Shortens the middle of a long name for use in a title.
pub(crate) fn truncate(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_owned();
    }
    let keep = max.saturating_sub(1);
    let head: String = s.chars().take(keep * 2 / 3).collect();
    let tail: String = s.chars().skip(n - (keep - keep * 2 / 3)).collect();
    format!("{head}…{tail}")
}

pub(crate) fn format_eta(secs: f64) -> String {
    let s = secs.ceil().max(0.0) as u64;
    if s < 5 {
        "A few seconds".to_owned()
    } else if s < 60 {
        format!("About {} seconds", (s + 2) / 5 * 5)
    } else if s < 3600 {
        let mn = (s + 30) / 60;
        format!("About {mn} minute{}", if mn == 1 { "" } else { "s" })
    } else {
        format!("About {} h {} min", s / 3600, (s % 3600 + 30) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(id: u64) -> ActiveOp {
        let spec = OpSpec { title: String::new(), from: String::new(), to: String::new(), cancelable: true };
        ActiveOp::new(id, spec, Arc::default())
    }

    #[test]
    fn a_lock_lasts_as_long_as_its_operation_is_listed() {
        let mut ops = vec![op(1), op(2)];
        lock_op_paths(&mut ops, 1, [PathBuf::from("C:/a/file")]);
        lock_op_paths(&mut ops, 2, [PathBuf::from("C:/b")]);
        assert!(touches_locked(&locked_paths(&ops), &[PathBuf::from("C:/a/file")]));
        // Containing and contained paths count as touching.
        assert!(touches_locked(&locked_paths(&ops), &[PathBuf::from("C:/a")]));
        assert!(touches_locked(&locked_paths(&ops), &[PathBuf::from("C:/b/sub/x")]));
        assert!(!touches_locked(&locked_paths(&ops), &[PathBuf::from("C:/c")]));
        ops.remove(0);
        assert!(!touches_locked(&locked_paths(&ops), &[PathBuf::from("C:/a/file")]));
        assert!(touches_locked(&locked_paths(&ops), &[PathBuf::from("C:/b")]));
    }

    #[test]
    fn locking_for_an_unknown_op_changes_nothing() {
        let mut ops = vec![op(1)];
        lock_op_paths(&mut ops, 9, [PathBuf::from("C:/x")]);
        assert!(locked_paths(&ops).is_empty());
    }
}
