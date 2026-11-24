//! Popup dialogue Windows.

use crate::prelude::*;

impl Explorer {
    pub(crate) fn ui_confirm(&mut self, ui: &mut Ui) {
        let Some(c) = self.confirm.take() else { return };
        let (m, pal) = (self.m, self.pal);
        let mut choice = 0;
        let closed = dialog_shell_with_footer(
            ui,
            &m,
            &pal,
            "confirm",
            "Delete permanently?",
            true,
            m.accessibility_scale * 420.0,
            |ui| {
                ui.label(&c.text);
            },
            Some(Box::new(|ui| {
                // Right to left: Cancel ends up rightmost, like in a Windows dialog.
                if secondary_button(ui, &m, &pal, "Cancel").clicked() {
                    choice = 2;
                }
                if danger_button(ui, &m, &pal, "Delete").clicked() {
                    choice = 1;
                }
            })),
        );
        match (choice, closed) {
            (1, _) => {
                let ConfirmPurge { items, guard, .. } = c;
                self.run_job("Deleting permanently…", move || {
                    let _busy = guard; // stays busy until the purge has finished
                    trash_bin::purge(items).map(|_| String::new())
                });
            }
            // Cancel or X: `c` is dropped here, which releases the lock with it.
            (2, _) | (_, true) => {}
            _ => self.confirm = Some(c),
        }
    }

    /// "What's new": the release notes of this version from GitHub.
    pub(crate) fn ui_whats_new(&mut self, ui: &mut Ui) {
        let Some(state) = &self.whats_new else { return };
        let (m, pal) = (self.m, self.pal);
        let height = (ui.content_rect().height() * 0.6).clamp(m.accessibility_scale * 160.0, m.accessibility_scale * 520.0);
        let (mut retry, mut close) = (false, false);
        let failed = matches!(state, WhatsNew::Failed(_));
        let mut closed = dialog_shell_with_footer(
            ui,
            &m,
            &pal,
            "whats_new",
            "What's new",
            true,
            (ui.content_rect().width() * 0.9).clamp(m.accessibility_scale * 300.0, m.accessibility_scale * 560.0),
            |ui| match state {
                WhatsNew::Loading => {
                    ui.horizontal(|ui| {
                        spinner(ui, m.font, pal.text_secondary);
                        ui.label(RichText::new("Loading the release notes…").color(pal.text_secondary));
                    });
                }
                WhatsNew::Failed(e) => {
                    ui.label(RichText::new(format!("The release notes could not be loaded: {e}.")).color(pal.text_secondary));
                }
                WhatsNew::Ready { version, notes } => {
                    ui.label(RichText::new(format!("Version {version}")).color(pal.text_secondary));
                    ui.add_space(m.pad * 0.3);
                    egui::ScrollArea::vertical().max_height(height).auto_shrink([false, true]).show(ui, |ui| {
                        changelog_text(ui, &m, &pal, notes);
                    });
                }
            },
            Some(Box::new(|ui| {
                if primary_button(ui, &m, &pal, "Close").clicked() {
                    close = true;
                }
                if failed && secondary_button(ui, &m, &pal, "Try again").clicked() {
                    retry = true;
                }
            })),
        );
        closed |= close;
        if retry {
            self.open_whats_new();
        } else if closed {
            self.close_whats_new();
        }
    }

    /// Asks, one clashing item at a time (like Explorer), whether to replace it, keep both with a
    /// "(n)" suffix, or skip it. "Do this for all" answers the remaining ones the same way.
    pub(crate) fn ui_conflict(&mut self, ui: &mut Ui) {
        let Some(mut c) = self.conflict.take() else {
            return;
        };
        let Some((src, dst)) = c.todo.front().cloned() else {
            self.run_paste(c.ready, c.cut, c.merged_dirs);
            return;
        };
        let (m, pal) = (self.m, self.pal);
        let same = src == dst; // pasting a copy into the folder it came from
        let name = dst
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let taken: HashSet<PathBuf> = c
            .ready
            .iter()
            .map(|r| r.dst.clone())
            .chain(c.todo.iter().map(|(_, d)| d.clone()))
            .collect();
        let keep_name = lowest_free_name(&dst, &taken, src.is_dir())
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let remaining = c.todo.len() - 1;
        let message = if same {
            format!("“{name}” is already in this folder.")
        } else {
            format!("A file or folder named “{name}” already exists here.")
        };
        let mut all = c.apply_all;
        let mut choice = 0;
        let closed = dialog_shell(
            ui,
            &m,
            &pal,
            "conflict",
            "Item already exists",
            true,
            m.accessibility_scale * 460.0,
            |ui| {
                ui.label(&message);
                ui.add_space(m.pad * 0.5);
                ui.label(
                    RichText::new(format!("Keep both saves the new item as “{keep_name}”."))
                        .color(pal.text_secondary),
                );
                if remaining > 0 {
                    ui.add_space(m.pad * 0.5);
                    ui.checkbox(
                        &mut all,
                        format!(
                            "Do this for the {remaining} remaining conflict{}",
                            if remaining == 1 { "" } else { "s" }
                        ),
                    );
                }
                ui.add_space(m.pad * 1.5);
                ui.horizontal(|ui| {
                    if same {
                        if primary_button(ui, &m, &pal, "Keep both").clicked() {
                            choice = 2;
                        }
                    } else {
                        if primary_button(ui, &m, &pal, "Replace").clicked() {
                            choice = 1;
                        }
                        if secondary_button(ui, &m, &pal, "Keep both").clicked() {
                            choice = 2;
                        }
                    }
                    if secondary_button(ui, &m, &pal, "Skip").clicked() {
                        choice = 3;
                    }
                    if secondary_button(ui, &m, &pal, "Cancel").clicked() {
                        choice = 4;
                    }
                });
            },
        );
        match (choice, closed) {
            (4, _) | (_, true) => {} // cancel the whole paste
            (0, false) => {
                c.apply_all = all;
                self.conflict = Some(c);
            }
            (ch, _) => {
                let n = if all { c.todo.len() } else { 1 };
                for _ in 0..n {
                    let Some((src, dst)) = c.todo.pop_front() else {
                        break;
                    };
                    match ch {
                        1 if src != dst => c.ready.push(PasteItem {
                            src,
                            dst,
                            replace: true,
                        }),
                        1 | 2 => {
                            let taken: HashSet<PathBuf> = c
                                .ready
                                .iter()
                                .map(|r| r.dst.clone())
                                .chain(c.todo.iter().map(|(_, d)| d.clone()))
                                .collect();
                            let new = lowest_free_name(&dst, &taken, src.is_dir());
                            c.ready.push(PasteItem {
                                src,
                                dst: new,
                                replace: false,
                            });
                        }
                        _ => {} // skip this one
                    }
                }
                if c.todo.is_empty() {
                    self.run_paste(c.ready, c.cut, c.merged_dirs);
                } else {
                    c.apply_all = all;
                    self.conflict = Some(c);
                }
            }
        }
    }
}

/// Draws GitHub release notes (Markdown) as plain text: headings, bullets and paragraphs. Emphasis marks
/// and inline code ticks are dropped.
fn changelog_text(ui: &mut Ui, m: &Metrics, pal: &Palette, notes: &str) {
    if notes.trim().is_empty() {
        ui.label(RichText::new("This release has no description.").color(pal.text_secondary));
        return;
    }
    for line in notes.lines() {
        let line = line.trim_end();
        let plain = |t: &str| t.replace("**", "").replace('`', "");
        if line.trim().is_empty() {
            ui.add_space(m.pad * 0.5);
        } else if let Some(h) = line.trim_start().strip_prefix('#') {
            ui.add_space(m.pad * 0.4);
            ui.label(RichText::new(plain(h.trim_start_matches('#').trim())).size(m.font * 1.1).color(pal.text));
        } else if let Some(item) = line.trim_start().strip_prefix("- ").or_else(|| line.trim_start().strip_prefix("* ")) {
            // A bullet with a hanging indent: wrapped lines stay under the text, not under the dot.
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = m.pad * 0.6;
                ui.label(RichText::new("•").color(pal.text_secondary));
                ui.add(egui::Label::new(plain(item)).wrap());
            });
        } else if line.trim_start().starts_with("Full Changelog") || line.trim_start().starts_with("http") {
            ui.add(egui::Label::new(RichText::new(plain(line)).size(m.font * 0.9).color(pal.text_secondary)).wrap());
        } else {
            ui.add(egui::Label::new(plain(line)).wrap());
        }
    }
}
