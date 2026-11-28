//! Status bar.

use crate::prelude::*;

impl Explorer {
    //The bottom status bar, showing the number of items in the current view and potentially
    //a loading symbol.
    pub(crate) fn ui_status(&mut self, ui: &mut Ui) {
        let (m, pal) = (self.win.m, self.win.pal);
        let tab = &self.win.tabs[self.win.active];
        let mut text = format!(
            "{} item{}",
            tab.view.len(),
            if tab.view.len() == 1 { "" } else { "s" }
        );
        if tab.page.is_some() {
            text = String::new();
        } else if let Some(tr) = &tab.trash {
            text = format!(
                "{} item{}",
                tr.view.len(),
                if tr.view.len() == 1 { "" } else { "s" }
            );
            if !tr.selected.is_empty() {
                text.push_str(&format!("   |   {} selected", tr.selected.len()));
            }
        } else if !tab.selected.is_empty() {
            let (mut count, mut bytes, mut all_known) = (0usize, 0u64, true);
            for p in &tab.selected {
                if let Some(e) = tab.find(p) {
                    count += 1;
                    match if e.is_dir { e.dir_size } else { Some(e.size) } {
                        Some(s) => bytes = bytes.saturating_add(s),
                        None => all_known = false,
                    }
                }
            }
            if count > 0 {
                text.push_str(&format!(
                    "   |   {count} item{} selected",
                    if count == 1 { "" } else { "s" }
                ));
                if all_known {
                    text.push_str(&format!("   {}", format_size(bytes)));
                }
            }
        }
        if tab.trash.is_none() && tab.search_job.as_ref().is_some_and(|j| !j.done) {
            text.push_str("   |   searching…");
        }
        // File operations have their own progress dialog; the status bar keeps an entry for them
        // while the user has hidden any of those dialogs. Clicking it opens the dialog again (or, with
        // several operations running, a list to choose from).
        let ops: Vec<(u64, String, String)> =
            self.core.ops.iter().filter(|o| o.hidden && self.is_focus_window()).map(|o| (o.id, o.title.clone(), crate::ui::ops_dialog::op_target(o))).collect();
        // The longest an item name gets before it is cut off with "…".
        let name_limit = m.accessibility_scale * 160.0;
        let busy = self.core.jobs_running > 0 || tab.loading || tab.trash.as_ref().is_some_and(|t| t.loading);
        let job = self.core.job_label.clone();
        let mut reopen: Vec<u64> = Vec::new();
        let clip = self.core.clipboard.as_ref().map(|(p, cut)| {
            let what = if p.len() == 1 {
                p[0].file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            } else {
                format!("{} items", p.len())
            };
            format!("{}: {}", if *cut { "Cut" } else { "Copied" }, what)
        });
        egui::Panel::bottom("status")
            .show_separator_line(false)
            .frame(
                layer_frame(
                    &pal,
                    m.radius,
                    m.pad * 1.5,
                    m.pad,
                    m.pad * 0.4,
                    m.pad * 0.4,
                    false,
                )
                .corner_radius(CornerRadius {
                    nw: 0,
                    ne: 0,
                    sw: 0,
                    se: if get_desktop().contains("GNOME") && !Self::is_maximized(ui.ctx()) && self.core.settings.custom_frame {15}else{0},
                }),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(text)
                            .color(pal.text_secondary)
                            .size(m.font * 0.93),
                    );
                    if let Some(c) = clip {
                        ui.label(
                            RichText::new(format!("   |   {c}"))
                                .color(pal.text_secondary)
                                .size(m.font * 0.93),
                        );
                    }
                    if busy {
                        spinner(ui, m.font, pal.text_secondary);
                        if !job.is_empty() {
                            ui.label(
                                RichText::new(job)
                                    .color(pal.text_secondary)
                                    .size(m.font * 0.93),
                            );
                        }
                    }

                    if let [(id, title, target)] = ops.as_slice() {
                        let entry = ui
                            .horizontal(|ui| {
                                ui.add_space(m.pad);
                                spinner(ui, m.font, pal.text_secondary);
                                let head = if target.is_empty() { title.clone() } else { format!("{title}:") };
                                ui.label(RichText::new(head).color(pal.text_secondary).size(m.font * 0.93));
                                if !target.is_empty() {
                                    let w = text_width(ui, target, m.font * 0.93).min(name_limit);
                                    let (r, _) = ui.allocate_exact_size(vec2(w, m.font * 1.4), Sense::hover());
                                    paint_text(ui, r, target, FontId::proportional(m.font * 0.93), pal.text_secondary, false);
                                }
                            })
                            .response;
                        let full = if target.is_empty() { title.clone() } else { format!("{title}: {target}") };
                        let click = ui
                            .interact(entry.rect, win_id("status_ops"), Sense::click())
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .on_hover_text(full);
                        if click.clicked() {
                            reopen.push(*id);
                        }
                    } else if !ops.is_empty() {
                        // Several hidden operations: pick the one to show again.
                        let longest_title = ops.iter().map(|(_, t, _)| text_width(ui, t, m.font)).fold(0.0, f32::max);
                        cmd_dropdown_wide(
                            ui,
                            &m,
                            &pal,
                            None,
                            true,
                            Some(&format!("{} operations", ops.len())),
                            Some((m.font * 0.93, pal.text_secondary)),  // So the label looks consistent with the non-dropdown version.
                            "Show an operation",
                            true,
                            m.accessibility_scale * 140.0,
                            longest_title + name_limit + m.pad * 3.0,
                            |ui| {
                                egui::ScrollArea::vertical()
                                    .max_height(m.accessibility_scale * 320.0)
                                    .show(ui, |ui| {
                                        for (id, title, target) in &ops {
                                            let text = if target.is_empty() { title.clone() } else { format!("{title}: {target}") };
                                            if ui.add(egui::Button::new(&text).truncate()).on_hover_text(&text).clicked() {
                                                reopen.push(*id);
                                            }
                                        }
                                    });
                            },
                        );
                    }

                    // If the window runs with administrator rights, notify the user.
                    // If it is an archive path, display "Read-only".
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if is_elevated() {
                            status_pill(ui, &m, pal.selected_hover, pal.text_secondary,"Administrator");
                        }
                        // Avoid special tabs from displaying old status because dir change doesn't fire here.
                        // Consider a more elegant solution.
                        if is_archive_path(&*self.win.tabs[self.win.active].dir) &&
                            self.win.tabs[self.win.active].trash.is_none() &&
                            self.win.tabs[self.win.active].page.is_none() {
                            status_pill(ui, &m, pal.control_hover, pal.text_secondary,"Read-only");
                        }
                    });
                });
            });
        for id in reopen {
            if let Some(op) = self.core.ops.iter_mut().find(|o| o.id == id) {
                op.hidden = false;
            }
        }
    }
}
