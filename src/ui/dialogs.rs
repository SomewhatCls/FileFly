//! Confirmation and conflict dialogs.

use crate::prelude::*;

impl Explorer {
    pub(crate) fn ui_confirm(&mut self, ui: &mut Ui) {
        let Some(c) = self.confirm.take() else { return };
        let (m, pal) = (self.m, self.pal);
        let mut choice = 0;
        let closed = dialog_shell(
            ui,
            &m,
            &pal,
            "confirm",
            "Delete permanently?",
            true,
            m.s * 420.0,
            |ui| {
                ui.label(&c.text);
                ui.add_space(m.pad * 1.5);
                ui.horizontal(|ui| {
                    if danger_button(ui, &m, "Delete").clicked() {
                        choice = 1;
                    }
                    if secondary_button(ui, &m, "Cancel").clicked() {
                        choice = 2;
                    }
                });
            },
        );
        match (choice, closed) {
            (1, _) => {
                let items = c.items;
                self.run_job("Deleting permanently…", move || {
                    trash_bin::purge(items).map(|_| String::new())
                });
            }
            (2, _) | (_, true) => {}
            _ => self.confirm = Some(c),
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
            m.s * 460.0,
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
                        if secondary_button(ui, &m, "Keep both").clicked() {
                            choice = 2;
                        }
                    }
                    if secondary_button(ui, &m, "Skip").clicked() {
                        choice = 3;
                    }
                    if secondary_button(ui, &m, "Cancel").clicked() {
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
