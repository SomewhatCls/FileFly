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
    pub(crate) progress: Arc<OpProgress>,
    pub(crate) started: Instant,
    /// "More details" (speed graph, time remaining) is open.
    pub(crate) expanded: bool,
    /// The user closed the dialog of an operation that can't be cancelled.
    pub(crate) hidden: bool,
    pub(crate) cancelling: bool,
    pub(crate) last_t: Instant,
    pub(crate) last_bytes: u64,
    /// Smoothed bytes per second, and the raw samples the graph draws.
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
            progress,
            started: now,
            expanded: false,
            hidden: false,
            cancelling: false,
            last_t: now,
            last_bytes: 0,
            speed: 0.0,
            history: Vec::new(),
        }
    }

    /// Takes a speed sample about four times a second.
    pub(crate) fn sample(&mut self, now: Instant) {
        let dt = now.duration_since(self.last_t).as_secs_f32();
        if dt < 0.25 {
            return;
        }
        let done = self.progress.done_bytes.load(AO::Relaxed);
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

    /// 0..=1, or None while the amount of work isn't known (preparing, or a job without progress).
    pub(crate) fn fraction(&self) -> Option<f32> {
        let p = &self.progress;
        if p.preparing.load(AO::Relaxed) {
            return None;
        }
        let (tb, ti) = (p.total_bytes.load(AO::Relaxed), p.total_items.load(AO::Relaxed));
        if tb > 0 {
            Some((p.done_bytes.load(AO::Relaxed) as f64 / tb as f64).clamp(0.0, 1.0) as f32)
        } else if ti > 0 {
            Some((p.done_items.load(AO::Relaxed) as f64 / ti as f64).clamp(0.0, 1.0) as f32)
        } else {
            None
        }
    }
}

/// Shortens the middle of a long name for use in a title.
pub(crate) fn elide(s: &str, max: usize) -> String {
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
