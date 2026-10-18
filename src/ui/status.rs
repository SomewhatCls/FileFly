//! Status bar.

use crate::prelude::*;

impl Explorer {
    //The bottom status bar, showing the number of items in the current view and potentially
    //a loading symbol.
    pub(crate) fn ui_status(&mut self, ui: &mut Ui) {
        let (m, pal) = (self.m, self.pal);
        let tab = &self.tabs[self.active];
        let maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
        let radius = if maximized { 0 } else { RADIUS };
        let mut text = format!(
            "{} item{}",
            tab.view.len(),
            if tab.view.len() == 1 { "" } else { "s" }
        );
        if tab.settings {
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
        // File operations have their own progress dialog; the status bar only keeps a spinner for
        // one whose dialog the user hid.
        let hidden_op = self.ops.iter().find(|o| o.hidden).map(|o| o.title.clone());
        let busy = self.jobs_running > 0
            || hidden_op.is_some()
            || tab.loading
            || tab.trash.as_ref().is_some_and(|t| t.loading);
        let job = if self.job_label.is_empty() { hidden_op.unwrap_or_default() } else { self.job_label.clone() };
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
                    se: radius,
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
                        ui.add(egui::Spinner::new().size(m.font));
                        if !job.is_empty() {
                            ui.label(
                                RichText::new(job)
                                    .color(pal.text_secondary)
                                    .size(m.font * 0.93),
                            );
                        }
                    }
                });
            });
    }
}
