//! Progress dialog for running file operations.

use crate::prelude::*;

/// Thin rounded progress bar; a sliding segment when the amount of work isn't known.
pub(crate) fn progress_bar(ui: &mut Ui, m: &Metrics, pal: &Palette, frac: Option<f32>, paused: bool) {
    let h = m.accessibility_scale * 6.0;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    let r = CornerRadius::same((h * 0.5) as u8);
    let p = ui.painter_at(rect);
    p.rect_filled(rect, r, pal.divider);
    let fill = if paused { pal.accent.gamma_multiply(0.5) } else { pal.accent };
    match frac {
        Some(f) => {
            let w = (rect.width() * f.clamp(0.0, 1.0)).max(if f > 0.0 { h } else { 0.0 });
            if w > 0.0 {
                p.rect_filled(Rect::from_min_size(rect.min, vec2(w, h)), r, fill);
            }
        }
        None => {
            let t = ui.input(|i| i.time) as f32;
            let seg = rect.width() * 0.3;
            let x = rect.left() + (t * 0.7 % 1.0) * (rect.width() + seg) - seg;
            let (a, b) = (x.max(rect.left()), (x + seg).min(rect.right()));
            if b > a {
                p.rect_filled(Rect::from_min_max(pos2(a, rect.top()), pos2(b, rect.bottom())), r, fill);
            }
        }
    }
}

/// Throughput graph of the last samples (newest at the right), like Explorer's "More details".
pub(crate) fn speed_graph(ui: &mut Ui, m: &Metrics, pal: &Palette, history: &[f32]) {
    const SLOTS: usize = 60;
    let h = m.accessibility_scale * 64.0;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    let p = ui.painter_at(rect);
    let r = CornerRadius::same(m.radius);
    p.rect_filled(rect, r, pal.control);
    let shown = &history[history.len().saturating_sub(SLOTS)..];
    let max = shown.iter().copied().fold(1.0_f32, f32::max) * 1.15;
    let col = rect.width() / SLOTS as f32;
    let mut line: Vec<egui::Pos2> = Vec::with_capacity(shown.len());
    for (i, v) in shown.iter().enumerate() {
        let x1 = rect.right() - (shown.len() - i - 1) as f32 * col;
        let top = rect.bottom() - (v / max) * (h - 4.0);
        p.rect_filled(
            Rect::from_min_max(pos2((x1 - col).max(rect.left()), top), pos2(x1, rect.bottom())),
            CornerRadius::ZERO,
            pal.accent.gamma_multiply(0.28),
        );
        line.push(pos2(x1 - col * 0.5, top));
    }
    if line.len() > 1 {
        p.add(egui::Shape::line(line, Stroke::new(1.5_f32, pal.accent)));
    }
    p.rect_stroke(rect, r, Stroke::new(1.0_f32, pal.divider), StrokeKind::Inside);
}

/// "From   <path>" line; the path is cut off with an ellipsis instead of widening the dialog.
pub(crate) fn op_line(ui: &mut Ui, m: &Metrics, pal: &Palette, key: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(vec2(m.accessibility_scale * 92.0, m.font * 1.5), Layout::left_to_right(Align::Center), |ui| {
            ui.label(RichText::new(key).color(pal.text_secondary).size(m.font * 0.95));
        });
        ui.add(egui::Label::new(RichText::new(value).size(m.font * 0.95)).truncate());
    });
}

/// What the user did in one operation's panel during this frame.
#[derive(Default)]
struct OpUi {
    cancel: bool,
    hide: bool,
    pause: bool,
    more: bool,
    skip: Vec<u64>,
    /// The answer to "delete permanently?": Some(true) = delete, Some(false) = cancel.
    perm: Option<bool>,
}

/// One operation's section: headline, bar, details, buttons. Several of these are stacked
/// vertically in the single operations window, like Explorer's copy dialog.
///
/// NOTE: `FileOp` stands for whatever type `Explorer::ops` holds; rename it to match.
fn op_panel(ui: &mut Ui, m: &Metrics, pal: &Palette, op: &mut ActiveOp, width: f32) -> OpUi {
    let mut out = OpUi::default();
    let paused = op.progress.paused.load(AO::Relaxed);
    ui.set_width(width);

    let preparing = op.progress.preparing.load(AO::Relaxed);
    // While sizes are being calculated the amount of work isn't known: the sliding bar, not an empty one.
    let frac = if preparing { None } else { op.fraction() };
    // Set while the worker waits for "delete permanently?" (items the Recycle Bin refused).
    let question = op.progress.permanent_question();
    let headline = if op.cancelling {
        "Cancelling…".to_owned()
    } else if question.is_some() {
        "Waiting for your decision…".to_owned()
    } else if preparing {
        "Calculating…".to_owned()
    } else if let Some(f) = frac {
        format!("{}% complete{}", (f * 100.0) as u32, if paused { "  (paused)" } else { "" })
    } else {
        "Working…".to_owned()
    };
    // The operation's own title is shown per panel, since the window can hold several.
    ui.add_space(m.pad * 0.3);
    ui.label(RichText::new(headline).size(m.font * 1.05));
    ui.add_space(m.pad * 0.6);
    progress_bar(ui, m, pal, frac, paused);
    // Running total under the bar: bytes done of total, or items done of total.
    let (tb, db, ti, di) = (
        op.progress.total_bytes.load(AO::Relaxed),
        op.progress.done_bytes.load(AO::Relaxed),
        op.progress.total_items.load(AO::Relaxed),
        op.progress.done_items.load(AO::Relaxed),
    );
    let totals = if preparing {
        None
    } else if tb > 0 {
        Some(format!("{} of {}", format_size(db.min(tb)), format_size(tb)))
    } else if ti > 1 {
        Some(format!("{} of {} items", di.min(ti), ti))
    } else {
        None
    };
    ui.add_space(m.pad * 0.5);
    if let Some(t) = totals {
        ui.label(RichText::new(t).size(m.font * 0.95).color(pal.text_secondary));
    }
    ui.add_space(m.pad * 0.5);

    if !op.from.is_empty() {
        op_line(ui, m, pal, "From", &op.from);
    }
    if !op.to.is_empty() {
        op_line(ui, m, pal, "To", &op.to);
    }
    let running = op.progress.running_items();
    let many = op.cancelable && op.progress.total_items.load(AO::Relaxed) > 1;
    if many && !running.is_empty() && !op.cancelling {
        // Several items at once: each one gets its own cancel button (the others carry on).
        for (id, name) in running.iter().take(4) {
            ui.horizontal(|ui| {
                let w = (ui.available_width() - m.ctl_h * 1.3).max(m.accessibility_scale * 40.0);
                ui.allocate_ui_with_layout(vec2(w, m.ctl_h), Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(RichText::new(name).size(m.font * 0.95)).truncate());
                });
                if icon_button(ui, m, pal, "close", "Cancel this item only", true, false).clicked() {
                    out.skip.push(*id);
                }
            });
        }
        if running.len() > 4 {
            ui.label(
                RichText::new(format!("+{} more in progress", running.len() - 4))
                    .size(m.font * 0.9)
                    .color(pal.text_secondary),
            );
        }
    } else {
        let current = op.progress.current();
        if !current.is_empty() {
            op_line(ui, m, pal, "Name", &current);
        }
    }

    // Operations that count items but no bytes (moving to the Recycle Bin, moving within a drive) show
    // items per second, once there is more than one item; operations that report nothing at all
    // (`run_job`) have no details to show.
    let by_items = tb == 0 && ti > 1;
    let has_details = op.cancelable || tb > 0 || ti > 0;
    if op.expanded {
        ui.add_space(m.pad);
        if by_items || tb > 0 || op.speed > 0.0 {
            speed_graph(ui, m, pal, &op.history);
            ui.add_space(m.pad * 0.6);
            let speed = if by_items {
                format!("{:.1} items/s", op.speed)
            } else {
                format!("{}/s", format_size(op.speed as u64))
            };
            op_line(ui, m, pal, "Speed", &speed);
        }
        let eta = if preparing {
            "Calculating…".to_owned()
        } else if tb > db && op.speed > 1.0 {
            format_eta((tb - db) as f64 / op.speed as f64)
        } else if by_items && ti > di && op.speed > 0.05 {
            format_eta((ti - di) as f64 / op.speed as f64)
        } else {
            "—".to_owned()
        };
        op_line(ui, m, pal, "Time left", &eta);
        if ti > 0 {
            let mut left = format!("{}", ti.saturating_sub(di));
            if tb > db {
                left.push_str(&format!("  ({})", format_size(tb - db)));
            }
            op_line(ui, m, pal, "Items left", &left);
        }
    }

    if let Some((count, example, reason)) = &question {
        ui.add_space(m.pad);
        let what = if *count == 1 {
            format!("“{example}” can't be moved to the Recycle Bin.")
        } else {
            format!("{count} items can't be moved to the Recycle Bin, among them “{example}”.")
        };
        ui.add(egui::Label::new(RichText::new(what).size(m.font * 0.95)).wrap());
        ui.add(
            egui::Label::new(
                RichText::new(format!(
                    "{} Delete permanently instead? This can't be undone.",
                    if reason.is_empty() { "They may be too large for it." } else { reason.as_str() }
                ))
                    .size(m.font * 0.9)
                    .color(pal.text_secondary),
            )
            .wrap(),
        );
        ui.add_space(m.pad * 1.2);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // Right to left: Cancel ends up rightmost, like in a Windows dialog.
            if secondary_button(ui, m, pal, "Cancel").clicked() {
                out.perm = Some(false);
            }
            if primary_button(ui, m, pal, "Delete permanently").clicked() {
                out.perm = Some(true);
            }
        });
        return out;
    }

    ui.add_space(m.pad * 1.2);
    ui.horizontal(|ui| {
        if has_details {
            let label = if op.expanded { "Fewer details" } else { "More details" };
            if Btn::new().label(label).show(ui, m, pal).clicked() {
                out.more = true;
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if op.cancelable {
                let idle = !op.cancelling;
                if Btn::new().label("Cancel").kind(BtnKind::Standard).enabled(idle).show(ui, m, pal).clicked() {
                    out.cancel = true;
                }
                if !op.progress.unpausable.load(AO::Relaxed)
                    && Btn::new()
                        .label(if paused { "Resume" } else { "Pause" })
                        .kind(BtnKind::Standard)
                        .enabled(idle)
                        .show(ui, m, pal)
                        .clicked()
                {
                    out.pause = true;
                }
            } else if secondary_button(ui, m, pal, "Hide").clicked() {
                out.hide = true;
            }
        });
    });
    out
}

impl Explorer {
    /// One progress window for all running file operations (a separate OS window): every
    /// operation is a panel of its own, stacked top to bottom with its own bar and graph, like
    /// Explorer's copy dialog. A new operation adds a panel; a finished one removes its panel.
    pub(crate) fn ui_ops(&mut self, ui: &mut Ui) {
        if self.ops.is_empty() {
            forget_tool_window(ui.ctx(), "file-ops");
            return;
        }
        self.ui.request_repaint_after(Duration::from_millis(100));
        let (m, pal) = (self.m, self.pal);
        let now = Instant::now();
        let ctx = ui.ctx().clone();
        let width = m.accessibility_scale * 440.0;

        // Operations that currently get a panel (hidden ones keep running without one, and very
        // short ones never pop up at all).
        // An operation that is waiting for the user's answer always gets its panel.
        let is_shown = |op: &ActiveOp| {
            op.progress.permanent_question().is_some()
                || (!op.hidden && op.started.elapsed() >= Duration::from_millis(500))
        };
        let mut shown = 0usize;
        let mut first_title = String::new();
        let mut any_cancelable = false;
        for op in self.ops.iter_mut().filter(|o| is_shown(o)) {
            op.sample(now);
            if shown == 0 {
                first_title = op.title.clone();
            }
            shown += 1;
            any_cancelable |= op.cancelable;
        }
        if shown == 0 {
            forget_tool_window(&ctx, "file-ops");
            return;
        }

        let title = if shown == 1 { first_title } else { format!("{shown} operations") };
        let spec = ToolWindow {
            key: "file-ops",
            caption: &title,
            width,
            max_height: m.accessibility_scale * 640.0,
            transparency: self.backdrop_transparency.expect("A transparency effect should always be set"),
            custom_frame: self.settings.custom_frame,
            // The window's own close button / Alt+F4 cancels every cancelable operation in it (the window
            // then stays until they have really stopped); without any, it just hides them.
            hold_open: any_cancelable,
        };
        let mut results: Vec<(usize, OpUi)> = Vec::new();
        let close_requested = tool_window(&ctx, &m, &pal, &spec, |ui| {
            ui.add_space(m.pad * 0.5);
            let mut first = true;
            for (i, op) in self.ops.iter_mut().enumerate() {
                if !is_shown(op) {
                    continue;
                }
                if !first {
                    let y = ui.cursor().top() + m.pad * 0.5;
                    ui.painter().hline(ui.max_rect().x_range(), y, Stroke::new(1.0_f32, pal.divider));
                    ui.add_space(m.pad * 1.5);
                }
                first = false;
                let r = op_panel(ui, &m, &pal, op, width);
                results.push((i, r));
            }
        });

        for (i, r) in results {
            let op = &mut self.ops[i];
            if r.more {
                op.expanded = !op.expanded;
            }
            for id in r.skip {
                op.progress.skip_item(id);
            }
            if let Some(delete) = r.perm {
                op.progress.answer_permanent(delete);
            }
            if r.pause {
                let paused = op.progress.paused.load(AO::Relaxed);
                op.progress.paused.store(!paused, AO::Relaxed);
            }
            let (mut cancel, mut hide) = (r.cancel, r.hide);
            if close_requested {
                if op.cancelable {
                    cancel = true;
                } else {
                    hide = true;
                }
            }
            if cancel && !op.cancelling {
                op.cancelling = true;
                op.progress.cancel.store(true, AO::Relaxed);
                op.progress.paused.store(false, AO::Relaxed);
            }
            if hide {
                op.hidden = true;
            }
        }
    }
}
