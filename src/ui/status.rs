//! Status bar.

use crate::prelude::*;

impl Explorer {
    //The bottom status bar, showing the number of items in the current view and potentially
    //a loading symbol.
    pub(crate) fn ui_status(&mut self, ui: &mut Ui) {
        let (m, pal) = (self.m, self.pal);
        let tab = &self.tabs[self.active];
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
        let ops: Vec<(u64, String)> = self.ops.iter().map(|o| (o.id, o.title.clone())).collect();
        let any_hidden = self.ops.iter().any(|o| o.hidden);
        let busy = self.jobs_running > 0 || tab.loading || tab.trash.as_ref().is_some_and(|t| t.loading);
        let job = self.job_label.clone();
        let mut reopen: Vec<u64> = Vec::new();
        let clip = self.clipboard.as_ref().map(|(p, cut)| {
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
                    se: if get_desktop().contains("GNOME") && !Self::is_maximized(ui.ctx()) && self.settings.custom_frame {15}else{0},
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

                    if any_hidden {
                        let entry = ui
                            .horizontal(|ui| {
                                ui.add_space(m.pad);
                                spinner(ui, m.font, pal.text_secondary);
                                let label = match ops.as_slice() {
                                    [(_, title)] => title.clone(),
                                    _ => format!("{} operations", ops.len()),
                                };
                                ui.label(RichText::new(label).color(pal.text_secondary).size(m.font * 0.93));
                            })
                            .response;
                        let click = ui
                            .interact(entry.rect, Id::unique("status_ops"), Sense::click())
                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                        if let [(id, _)] = ops.as_slice() {
                            if click.clicked() {
                                reopen.push(*id);
                            }
                        } else {
                            egui::Popup::menu(&click).align(egui::RectAlign::TOP_START).show(|ui| {
                                for (id, title) in &ops {
                                    if menu_item(ui, &m, &pal, None, title, None, true) {
                                        reopen.push(*id);
                                        ui.close();
                                    }
                                }
                            });
                        }
                    }

                    // If the window runs with administrator rights, notify the user.
                    // If it is an archive path, display "Read-only".
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if is_elevated() {
                            status_pill(ui, &m, pal.selected_hover, pal.text_secondary,"Administrator");
                        }
                        // Avoid special tabs from displaying old status because dir change doesn't fire here.
                        // Consider a more elegant solution.
                        if is_archive_path(&*self.tabs[self.active].dir) &&
                            self.tabs[self.active].trash.is_none() &&
                            self.tabs[self.active].page.is_none() {
                            status_pill(ui, &m, pal.control_hover, pal.text_secondary,"Read-only");
                        }
                    });
                });
            });
        for id in reopen {
            if let Some(op) = self.ops.iter_mut().find(|o| o.id == id) {
                op.hidden = false;
            }
        }
    }
}
