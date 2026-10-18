//! Properties dialog.

use crate::prelude::*;

pub(crate) struct Props {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) is_dir: bool,
    pub(crate) kind: String,
    pub(crate) size: u64,
    pub(crate) created: Option<SystemTime>,
    pub(crate) modified: Option<SystemTime>,
    pub(crate) accessed: Option<SystemTime>,
    pub(crate) readonly: bool,
    pub(crate) hidden: bool,
    pub(crate) stats: Option<FolderStats>,
    pub(crate) stats_gen: u64,
}

pub(crate) fn build_props(path: &Path) -> Props {
    let md = fs::metadata(path).ok();
    let is_dir = md.as_ref().is_some_and(|m| m.is_dir());
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_uppercase());
    Props {
        path: path.to_path_buf(),
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string()),
        is_dir,
        kind: if is_dir {
            "File folder".into()
        } else {
            ext.map(|e| format!("{e} file"))
                .unwrap_or_else(|| "File".into())
        },
        size: md.as_ref().map(|m| m.len()).unwrap_or(0),
        created: md.as_ref().and_then(|m| m.created().ok()),
        modified: md.as_ref().and_then(|m| m.modified().ok()),
        accessed: md.as_ref().and_then(|m| m.accessed().ok()),
        readonly: md.as_ref().is_some_and(|m| m.permissions().readonly()),
        hidden: md.as_ref().is_some_and(|m| attributes(m) & 0x2 != 0),
        stats: None,
        stats_gen: 0,
    }
}

impl Explorer {
    pub(crate) fn ui_props(&mut self, ui: &mut Ui) {
        let Some(mut p) = self.props.take() else {
            return;
        };
        let (m, pal) = (self.m, self.pal);
        let fmt = self.settings.date_format;
        let cached = self.indexer.lookup(&p.path);
        let mut readonly = p.readonly;
        let (mut copy_path, mut show_in_folder) = (false, false);
        let closed = dialog_shell(
            ui,
            &m,
            &pal,
            "props",
            "Properties",
            false,
            m.s * 460.0,
            |ui| {
                ui.horizontal(|ui| {
                    let (r, _) =
                        ui.allocate_exact_size(vec2(m.icon * 1.8, m.icon * 1.8), Sense::hover());
                    let icon = if p.is_dir {
                        "folder"
                    } else {
                        icon_for_extension(&p.name.rsplit('.').next().unwrap_or("").to_lowercase())
                    };
                    draw_icon(ui, r.center(), m.icon * 1.8, icon, Color32::WHITE);
                    ui.add(
                        egui::Label::new(RichText::new(&p.name).size(m.font * 1.2))
                            .wrap_mode(egui::TextWrapMode::Truncate),
                    );
                });
                ui.add_space(m.pad);
                ui.separator();
                ui.add_space(m.pad * 0.5);
                egui::Grid::new("props_grid")
                    .num_columns(2)
                    .spacing(vec2(m.pad * 2.0, m.pad * 0.7))
                    .show(ui, |ui| {
                        let row = |ui: &mut Ui, k: &str, v: String| {
                            ui.label(RichText::new(k).color(pal.text_secondary));
                            ui.add(
                                egui::Label::new(v)
                                    .selectable(true)
                                    .wrap_mode(egui::TextWrapMode::Wrap),
                            );
                            ui.end_row();
                        };
                        row(ui, "Type", p.kind.clone());
                        row(
                            ui,
                            "Location",
                            p.path
                                .parent()
                                .map(|x| x.display().to_string())
                                .unwrap_or_default(),
                        );
                        if p.is_dir {
                            match &p.stats {
                                Some(st) => {
                                    row(
                                        ui,
                                        "Size",
                                        format!(
                                            "{} ({} bytes)",
                                            format_size(st.bytes),
                                            group_digits(st.bytes)
                                        ),
                                    );
                                    row(
                                        ui,
                                        "Contains",
                                        format!(
                                            "{} files, {} folders",
                                            group_digits(st.files),
                                            group_digits(st.folders)
                                        ),
                                    );
                                }
                                None => {
                                    row(
                                        ui,
                                        "Size",
                                        cached
                                            .map(format_size)
                                            .map(|s| format!("{s} (calculating…)"))
                                            .unwrap_or_else(|| "Calculating…".into()),
                                    );
                                }
                            }
                        } else {
                            row(
                                ui,
                                "Size",
                                format!("{} ({} bytes)", format_size(p.size), group_digits(p.size)),
                            );
                        }
                        row(ui, "Created", format_date(p.created, fmt, true));
                        row(ui, "Modified", format_date(p.modified, fmt, true));
                        row(ui, "Accessed", format_date(p.accessed, fmt, true));
                    });
                ui.add_space(m.pad * 0.5);
                ui.separator();
                ui.add_space(m.pad * 0.5);
                ui.horizontal(|ui| {
                    ui.checkbox(&mut readonly, "Read-only");
                    let mut hidden = p.hidden;
                    ui.add_enabled(false, egui::Checkbox::new(&mut hidden, "Hidden"));
                });
                ui.add_space(m.pad);
                ui.horizontal(|ui| {
                    if secondary_button(ui, &m, "Copy path").clicked() {
                        copy_path = true;
                    }
                    if secondary_button(ui, &m, "Show in folder").clicked() {
                        show_in_folder = true;
                    }
                });
            },
        );

        if readonly != p.readonly {
            if let Ok(md) = fs::metadata(&p.path) {
                let mut perm = md.permissions();
                perm.set_readonly(readonly);
                if fs::set_permissions(&p.path, perm).is_ok() {
                    p.readonly = readonly;
                }
            }
        }
        if copy_path {
            ui.copy_text(p.path.display().to_string());
        }
        if show_in_folder {
            self.apply(Action::OpenContaining(p.path.clone()));
        }
        if closed {
            self.props_gen.fetch_add(1, AO::SeqCst); // cancels a running size scan
        } else {
            self.props = Some(p);
        }
    }
}
