//! Progress dialog for running file operations.

use crate::prelude::*;

/// Thin rounded progress bar; a sliding segment when the amount of work isn't known.
pub(crate) fn progress_bar(ui: &mut Ui, m: &Metrics, pal: &Palette, frac: Option<f32>, paused: bool) {
    let h = m.s * 6.0;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    let r = CornerRadius::same((h * 0.5) as u8);
    let p = ui.painter_at(rect);
    p.rect_filled(rect, r, pal.divider);
    let fill = if paused { pal.accent.gamma_multiply(0.5) } else { pal.accent };
    match frac {
        Some(f) => {
            let w = (rect.width() * f.clamp(0.0, 1.0)).max(if f > 0.0 { h } else { 0.0 });
            if w > 0.0 {
                p.rect_filled(egui::Rect::from_min_size(rect.min, vec2(w, h)), r, fill);
            }
        }
        None => {
            let t = ui.input(|i| i.time) as f32;
            let seg = rect.width() * 0.3;
            let x = rect.left() + (t * 0.7 % 1.0) * (rect.width() + seg) - seg;
            let (a, b) = (x.max(rect.left()), (x + seg).min(rect.right()));
            if b > a {
                p.rect_filled(egui::Rect::from_min_max(pos2(a, rect.top()), pos2(b, rect.bottom())), r, fill);
            }
        }
    }
}

/// Throughput graph of the last samples (newest at the right), like Explorer's "More details".
pub(crate) fn speed_graph(ui: &mut Ui, m: &Metrics, pal: &Palette, history: &[f32]) {
    const SLOTS: usize = 60;
    let h = m.s * 64.0;
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
            egui::Rect::from_min_max(pos2((x1 - col).max(rect.left()), top), pos2(x1, rect.bottom())),
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
        ui.allocate_ui_with_layout(vec2(m.s * 92.0, m.font * 1.5), Layout::left_to_right(Align::Center), |ui| {
            ui.label(RichText::new(key).color(pal.text_secondary).size(m.font * 0.95));
        });
        ui.add(egui::Label::new(RichText::new(value).size(m.font * 0.95)).truncate());
    });
}

impl Explorer {
    /// One progress dialog per running file operation, in the style of the other dialogs: title,
    /// percent, bar, from/to, current name, Pause/Cancel, and an expandable speed graph.
    pub(crate) fn ui_ops(&mut self, ui: &mut Ui) {
        if self.ops.is_empty() {
            return;
        }
        self.ui.request_repaint_after(Duration::from_millis(100));
        let (m, pal) = (self.m, self.pal);
        let now = Instant::now();
        let center = ui.content_rect().center();
        let width = m.s * 440.0;
        let mut shown = 0.0_f32;
        for op in self.ops.iter_mut() {
            if op.hidden || op.started.elapsed() < Duration::from_millis(500) {
                continue;
            }
            op.sample(now);
            let pos = pos2(
                center.x - width * 0.5 - m.pad * 2.0 + shown * 26.0,
                center.y - m.s * 120.0 + shown * 26.0,
            );
            shown += 1.0;
            let (mut cancel, mut hide, mut pause, mut more) = (false, false, false, false);
            let mut skip: Vec<u64> = Vec::new();
            let paused = op.progress.paused.load(AO::Relaxed);
            egui::Window::new(op.title.clone())
                .id(Id::new(("file-op", op.id)))
                .title_bar(false)
                .collapsible(false)
                .resizable(false)
                .order(egui::Order::Foreground)
                .default_pos(pos)
                .frame(
                    egui::Frame::window(&ui.global_style())
                        .fill(pal.flyout)
                        .shadow(popup_shadow(pal.dark))
                        .stroke(Stroke::new(1.0_f32, pal.divider))
                        .inner_margin(Margin::same((m.pad * 2.0) as i8)),
                )
                .show(ui, |ui| {
                    ui.set_width(width);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&op.title).size(m.font * 1.3).strong());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let tip = if op.cancelable { "Cancel" } else { "Hide" };
                            if icon_button(ui, &m, &pal, "close", tip, true, false).clicked() {
                                if op.cancelable {
                                    cancel = true;
                                } else {
                                    hide = true;
                                }
                            }
                        });
                    });
                    ui.add_space(m.pad * 0.5);

                    let preparing = op.progress.preparing.load(AO::Relaxed);
                    let frac = op.fraction();
                    let headline = if op.cancelling {
                        "Cancelling…".to_owned()
                    } else if preparing {
                        "Calculating…".to_owned()
                    } else if let Some(f) = frac {
                        format!("{}% complete{}", (f * 100.0) as u32, if paused { "  (paused)" } else { "" })
                    } else {
                        "Working…".to_owned()
                    };
                    ui.label(RichText::new(headline).size(m.font * 1.05));
                    ui.add_space(m.pad * 0.6);
                    progress_bar(ui, &m, &pal, frac, paused);
                    ui.add_space(m.pad);

                    if !op.from.is_empty() {
                        op_line(ui, &m, &pal, "From", &op.from);
                    }
                    if !op.to.is_empty() {
                        op_line(ui, &m, &pal, "To", &op.to);
                    }
                    let running = op.progress.running_items();
                    let many = op.cancelable && op.progress.total_items.load(AO::Relaxed) > 1;
                    if many && !running.is_empty() && !op.cancelling {
                        // Several items at once: each one gets its own cancel button (the others carry on).
                        for (id, name) in running.iter().take(4) {
                            ui.horizontal(|ui| {
                                let w = (ui.available_width() - m.ctl_h * 1.3).max(m.s * 40.0);
                                ui.allocate_ui_with_layout(
                                    vec2(w, m.ctl_h),
                                    Layout::left_to_right(Align::Center),
                                    |ui| {
                                        ui.add(egui::Label::new(RichText::new(name).size(m.font * 0.95)).truncate());
                                    },
                                );
                                if icon_button(ui, &m, &pal, "close", "Cancel this item only", true, false).clicked() {
                                    skip.push(*id);
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
                            op_line(ui, &m, &pal, "Name", &current);
                        }
                    }

                    if op.expanded {
                        ui.add_space(m.pad);
                        speed_graph(ui, &m, &pal, &op.history);
                        ui.add_space(m.pad * 0.6);
                        let (tb, db) = (
                            op.progress.total_bytes.load(AO::Relaxed),
                            op.progress.done_bytes.load(AO::Relaxed),
                        );
                        let (ti, di) = (
                            op.progress.total_items.load(AO::Relaxed),
                            op.progress.done_items.load(AO::Relaxed),
                        );
                        if tb > 0 || op.speed > 0.0 {
                            op_line(ui, &m, &pal, "Speed", &format!("{}/s", format_size(op.speed as u64)));
                        }
                        let eta = if preparing {
                            "Calculating…".to_owned()
                        } else if tb > db && op.speed > 1.0 {
                            format_eta((tb - db) as f64 / op.speed as f64)
                        } else {
                            "—".to_owned()
                        };
                        op_line(ui, &m, &pal, "Time left", &eta);
                        if ti > 0 {
                            let mut left = format!("{}", ti.saturating_sub(di));
                            if tb > db {
                                left.push_str(&format!("  ({})", format_size(tb - db)));
                            }
                            op_line(ui, &m, &pal, "Items left", &left);
                        }
                    }

                    ui.add_space(m.pad * 1.2);
                    ui.horizontal(|ui| {
                        let label = if op.expanded { "Fewer details" } else { "More details" };
                        if ui
                            .add(egui::Button::new(RichText::new(label).color(pal.accent)).frame(false))
                            .clicked()
                        {
                            more = true;
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if op.cancelable {
                                if ui.add_enabled(!op.cancelling, egui::Button::new("Cancel")
                                    .corner_radius(CornerRadius::same(m.radius))
                                    .min_size(vec2(m.s * 92.0, m.ctl_h))).clicked()
                                {
                                    cancel = true;
                                }
                                if ui.add_enabled(!op.cancelling, egui::Button::new(if paused { "Resume" } else { "Pause" })
                                    .corner_radius(CornerRadius::same(m.radius))
                                    .min_size(vec2(m.s * 92.0, m.ctl_h))).clicked()
                                {
                                    pause = true;
                                }
                            } else if secondary_button(ui, &m, "Hide").clicked() {
                                hide = true;
                            }
                        });
                    });
                });
            if more {
                op.expanded = !op.expanded;
            }
            for id in skip {
                op.progress.skip_item(id);
            }
            if pause {
                op.progress.paused.store(!paused, AO::Relaxed);
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
