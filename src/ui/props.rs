//! Properties window: a window of its own that follows the item on disk and edits its attributes.

use crate::prelude::*;

/// How often the window looks at the item on disk again.
const REFRESH_EVERY: Duration = Duration::from_millis(750);

/// What the disk says about the item right now.
#[derive(Clone, PartialEq)]
pub(crate) struct PropsMeta {
    pub(crate) exists: bool,
    pub(crate) is_dir: bool,
    pub(crate) size: u64,
    pub(crate) created: Option<SystemTime>,
    pub(crate) modified: Option<SystemTime>,
    pub(crate) accessed: Option<SystemTime>,
    pub(crate) readonly: bool,
    pub(crate) hidden: bool,
    /// Where a symbolic link points.
    pub(crate) link_target: Option<PathBuf>,
}

/// Reads `path` from disk. For a link the figures are those of what it points to, when that exists.
pub(crate) fn read_props_meta(path: &Path) -> PropsMeta {
    let own = fs::symlink_metadata(path).ok();
    let link_target = own
        .as_ref()
        .filter(|m| m.file_type().is_symlink())
        .and_then(|_| fs::read_link(path).ok());
    let Some(md) = fs::metadata(path).ok().or(own) else {
        return PropsMeta {
            exists: false,
            is_dir: false,
            size: 0,
            created: None,
            modified: None,
            accessed: None,
            readonly: false,
            hidden: false,
            link_target,
        };
    };
    PropsMeta {
        exists: true,
        is_dir: md.is_dir(),
        size: md.len(),
        created: md.created().ok(),
        modified: md.modified().ok(),
        accessed: md.accessed().ok(),
        readonly: md.permissions().readonly(),
        hidden: attributes(&md) & 0x2 != 0,
        link_target,
    }
}

pub(crate) struct Props {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) meta: PropsMeta,
    /// Identifies this window's refresh results (see `AppMsg::PropsMeta`).
    pub(crate) id: u64,
    /// Contents of a folder, counted in the background (None while that runs).
    pub(crate) stats: Option<FolderStats>,
    pub(crate) stats_gen: u64,
    /// What went wrong the last time an attribute was changed.
    pub(crate) error: Option<String>,
    pub(crate) refreshed: Instant,
    pub(crate) refreshing: bool,
}

/// What the user asked for in the window during one frame.
#[derive(Default)]
struct PropsUi {
    open: bool,
    show_in_folder: bool,
    copy_path: bool,
    set_readonly: Option<bool>,
    set_hidden: Option<bool>,
}

fn kind_text(path: &Path, meta: &PropsMeta) -> String {
    if meta.is_dir {
        return "File folder".to_owned();
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some(e) => format!("{} file", e.to_uppercase()),
        None => "File".to_owned(),
    }
}

/// One "label   value" line inside a card. The value is selectable and wraps.
fn info_row(ui: &mut Ui, m: &Metrics, pal: &Palette, key: &str, add_value: impl FnOnce(&mut Ui)) {
    egui::Frame::new().inner_margin(Margin::symmetric((m.pad * 1.6) as i8, (m.pad * 0.8) as i8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                vec2(m.accessibility_scale * 92.0, m.font * 1.3),
                Layout::left_to_right(Align::Min),
                |ui| {
                    ui.label(RichText::new(key).color(pal.text_secondary));
                },
            );
            add_value(ui);
        });
    });
}

fn value_text(ui: &mut Ui, text: impl Into<String>) {
    ui.add(egui::Label::new(text.into()).selectable(true).wrap());
}

fn notice_banner(ui: &mut Ui, m: &Metrics, pal: &Palette, text: &str) {
    egui::Frame::new()
        .fill(pal.danger_bg)
        .corner_radius(CornerRadius::same(m.radius))
        .inner_margin(Margin::symmetric((m.pad * 1.4) as i8, (m.pad * 0.9) as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(RichText::new(text).color(pal.danger)).wrap());
        });
    ui.add_space(m.pad * 0.8);
}

/// The whole window content.
fn props_content(
    ui: &mut Ui,
    m: &Metrics,
    pal: &Palette,
    p: &Props,
    fmt: DateFormat,
    cached_size: Option<u64>,
) -> PropsUi {
    let mut out = PropsUi::default();
    let meta = &p.meta;

    // Header: the item itself.
    let icon_size = m.icon * 2.4;
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(icon_size, icon_size), Sense::hover());
        let icon = if meta.is_dir {
            "folder"
        } else {
            icon_for_extension(&p.name.rsplit('.').next().unwrap_or("").to_lowercase())
        };
        draw_icon(ui, r.center(), icon_size, icon, Color32::WHITE);
        ui.vertical(|ui| {
            ui.add(egui::Label::new(RichText::new(&p.name).size(m.font * 1.4)).selectable(true).wrap());
            let summary = if !meta.exists {
                "Not found".to_owned()
            } else if meta.is_dir {
                match &p.stats {
                    Some(st) => format!("{}  ·  {}", kind_text(&p.path, meta), format_size(st.bytes)),
                    None => kind_text(&p.path, meta),
                }
            } else {
                format!("{}  ·  {}", kind_text(&p.path, meta), format_size(meta.size))
            };
            ui.label(RichText::new(summary).color(pal.text_secondary));
        });
    });
    ui.add_space(m.pad);

    if !meta.exists {
        notice_banner(ui, m, pal, "This item no longer exists. It was moved, renamed or deleted.");
    }
    if let Some(e) = &p.error {
        notice_banner(ui, m, pal, e);
    }

    // General
    fl_card(ui, m, pal, |ui| {
        info_row(ui, m, pal, "Type", |ui| value_text(ui, kind_text(&p.path, meta)));
        fl_divider(ui, m, pal);
        info_row(ui, m, pal, "Location", |ui| {
            value_text(ui, p.path.parent().map(|x| x.display().to_string()).unwrap_or_default())
        });
        if let Some(target) = &meta.link_target {
            fl_divider(ui, m, pal);
            info_row(ui, m, pal, "Points to", |ui| value_text(ui, target.display().to_string()));
        }
        fl_divider(ui, m, pal);
        info_row(ui, m, pal, "Size", |ui| {
            if meta.is_dir {
                match &p.stats {
                    Some(st) => value_text(ui, format!("{} ({} bytes)", format_size(st.bytes), group_digits(st.bytes))),
                    None => {
                        if meta.exists {
                            spinner(ui, m.font * 0.9, pal.text_secondary);
                        }
                        let text = match cached_size {
                            Some(s) => format!("{} (calculating…)", format_size(s)),
                            None if meta.exists => "Calculating…".to_owned(),
                            None => "—".to_owned(),
                        };
                        ui.label(RichText::new(text).color(pal.text_secondary));
                    }
                }
            } else {
                value_text(ui, format!("{} ({} bytes)", format_size(meta.size), group_digits(meta.size)));
            }
        });
        if let (true, Some(st)) = (meta.is_dir, &p.stats) {
            fl_divider(ui, m, pal);
            info_row(ui, m, pal, "Contains", |ui| {
                value_text(
                    ui,
                    format!("{} files, {} folders", group_digits(st.files), group_digits(st.folders)),
                )
            });
        }
    });

    // Dates
    fl_section(ui, m, pal, "Dates");
    fl_card(ui, m, pal, |ui| {
        info_row(ui, m, pal, "Created", |ui| value_text(ui, format_date(meta.created, fmt, true)));
        fl_divider(ui, m, pal);
        info_row(ui, m, pal, "Modified", |ui| value_text(ui, format_date(meta.modified, fmt, true)));
        fl_divider(ui, m, pal);
        info_row(ui, m, pal, "Accessed", |ui| value_text(ui, format_date(meta.accessed, fmt, true)));
    });

    // Attributes: changes are applied right away and then read back from disk.
    if meta.exists {
        fl_section(ui, m, pal, "Attributes");
        fl_card(ui, m, pal, |ui| {
            let readonly_hint = if meta.is_dir {
                "Marks the folder itself. The items inside are not touched."
            } else {
                "Stops programs from changing or deleting the file by accident."
            };
            fl_row(ui, m, pal, "Read-only", Some(readonly_hint), |ui| {
                let mut on = meta.readonly;
                fl_switch(ui, m, pal, &mut on);
                if on != meta.readonly {
                    out.set_readonly = Some(on);
                }
            });
            if cfg!(windows) {
                fl_divider(ui, m, pal);
                fl_row(ui, m, pal, "Hidden", Some("Hides the item from listings unless hidden items are shown."), |ui| {
                    let mut on = meta.hidden;
                    fl_switch(ui, m, pal, &mut on);
                    if on != meta.hidden {
                        out.set_hidden = Some(on);
                    }
                });
            }
        });
    }

    ui.add_space(m.pad * 1.2);
    ui.horizontal(|ui| {
        if meta.exists && secondary_button(ui, m, pal, "Open").clicked() {
            out.open = true;
        }
        if meta.exists && secondary_button(ui, m, pal, "Show in folder").clicked() {
            out.show_in_folder = true;
        }
        if secondary_button(ui, m, pal, "Copy path").clicked() {
            out.copy_path = true;
        }
    });
    ui.add_space(m.pad * 0.5);
    out
}

impl Explorer {
    /// Opens the Properties window for `path` and starts counting what a folder contains.
    pub(crate) fn open_properties(&mut self, path: PathBuf) {
        let id = self.props_gen.fetch_add(1, AO::SeqCst) + 1;
        let meta = read_props_meta(&path);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let mut props = Props {
            path,
            name,
            meta,
            id,
            stats: None,
            stats_gen: 0,
            error: None,
            refreshed: Instant::now(),
            refreshing: false,
        };
        if props.meta.is_dir {
            props.stats_gen = self.spawn_folder_stats(&props.path);
        }
        self.props = Some(props);
    }

    /// Counts the contents of `root` in the background. Returns the number that identifies the result; any
    /// later change of `props_gen` (closing the window, a newer count) cancels this one.
    fn spawn_folder_stats(&self, root: &Path) -> u64 {
        let seq = self.props_gen.fetch_add(1, AO::SeqCst) + 1;
        let (tx, ui, g) = (self.msg_tx.clone(), self.ui.clone(), Arc::clone(&self.props_gen));
        let root = root.to_path_buf();
        std::thread::spawn(move || {
            let stats = folder_stats(&root, &|| g.load(AO::Relaxed) != seq);
            if g.load(AO::Relaxed) == seq {
                let _ = tx.send(AppMsg::Stats(seq, stats));
                ui.request_repaint();
            }
        });
        seq
    }

    /// A refresh of the item on disk arrived. A folder whose modification time changed is counted again.
    pub(crate) fn props_meta_arrived(&mut self, id: u64, meta: PropsMeta) {
        let Some(p) = self.props.as_mut().filter(|p| p.id == id) else { return };
        p.refreshing = false;
        p.refreshed = Instant::now();
        let changed_inside = meta.is_dir && meta.exists && meta.modified != p.meta.modified;
        p.meta = meta;
        if changed_inside {
            let path = p.path.clone();
            let seq = self.spawn_folder_stats(&path);
            if let Some(p) = self.props.as_mut() {
                p.stats_gen = seq;
            }
        }
    }

    pub(crate) fn ui_props(&mut self, ui: &mut Ui) {
        let Some(mut p) = self.props.take() else {
            forget_tool_window(ui.ctx(), "props");
            return;
        };
        let (m, pal) = (self.m, self.pal);
        let fmt = self.settings.date_format;

        // Follow the item on disk: look again regularly, off the UI thread (a network drive can be slow).
        if !p.refreshing && p.refreshed.elapsed() >= REFRESH_EVERY {
            p.refreshing = true;
            let (path, id, tx, ctx) = (p.path.clone(), p.id, self.msg_tx.clone(), self.ui.clone());
            std::thread::spawn(move || {
                let _ = tx.send(AppMsg::PropsMeta { id, meta: read_props_meta(&path) });
                ctx.request_repaint();
            });
        }
        self.ui.request_repaint_after(REFRESH_EVERY / 3);

        let cached = self.indexer.as_ref().and_then(|ix| ix.lookup(&p.path));
        let caption = format!("{} – Properties", truncate(&p.name, 40));
        let spec = ToolWindow {
            key: "props",
            caption: &caption,
            width: m.accessibility_scale * 460.0,
            max_height: m.accessibility_scale * 760.0,
            transparency: self.backdrop_transparency.unwrap_or(TransparencyEffect::None),
            custom_frame: self.settings.custom_frame,
            hold_open: false,
        };
        let ctx = ui.ctx().clone();
        let mut asked = PropsUi::default();
        let close_requested = tool_window(&ctx, &m, &pal, &spec, |ui| {
            ui.add_space(m.pad * 0.5);
            asked = props_content(ui, &m, &pal, &p, fmt, cached);
        });

        if let Some(on) = asked.set_readonly {
            self.change_attribute(&mut p, Attribute::ReadOnly, on);
        }
        if let Some(on) = asked.set_hidden {
            self.change_attribute(&mut p, Attribute::Hidden, on);
        }
        if asked.copy_path {
            ui.copy_text(p.path.display().to_string());
        }
        if asked.open {
            self.open_path(&p.path.clone());
        }
        if asked.show_in_folder {
            self.apply(Action::OpenContaining(p.path.clone()));
        }
        if close_requested {
            self.props_gen.fetch_add(1, AO::SeqCst); // cancels a running count
            forget_tool_window(&ctx, "props");
        } else {
            self.props = Some(p);
        }
    }

    /// Sets an attribute and reads the item back, so the window shows what is really on disk.
    fn change_attribute(&mut self, p: &mut Props, attribute: Attribute, on: bool) {
        match set_attribute(&p.path, attribute, on) {
            Ok(()) => p.error = None,
            Err(e) => {
                p.error = Some(if is_denied_io(&e) {
                    "Windows refused the change: this item needs administrator rights.".to_owned()
                } else {
                    format!("The attribute could not be changed: {e}")
                });
            }
        }
        p.meta = read_props_meta(&p.path);
        if attribute == Attribute::Hidden {
            self.reload_all(); // listings hide or show the item accordingly
        }
    }
}
