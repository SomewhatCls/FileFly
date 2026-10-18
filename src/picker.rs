// "Select a destination": a folder picker in the style of the Windows one. Navigation pane (Quick
// access + drives) on the left, the subfolders of the current folder on the right, Back / Up and an
// editable path above, "New folder" below. Used by "Extract to…"; `PickFor` says what the chosen
// folder is for, so other commands ("Copy to…", "Move to…") can reuse the dialog.

use crate::prelude::*;

/// What the chosen folder is for.
pub(crate) enum PickFor {
    /// "Extract to…": every zip goes into a new folder inside the chosen one. `name` is the new
    /// folder's name (only asked for when there is a single zip).
    Extract {
        zips: Vec<PathBuf>,
        name: String,
        show: bool,
    },
    /// "Change folder icon…": pick an `.ico` file for `folder` (files are listed next to folders).
    Icon { folder: PathBuf },
}

fn is_ico_ext(p: &Path) -> bool {
    p.extension().is_some_and(|x| x.eq_ignore_ascii_case("ico"))
}

/// An `.ico` file (not a folder that merely has such a name).
fn is_ico_path(p: &Path) -> bool {
    is_ico_ext(p) && p.is_file()
}

type Listing = Result<Vec<(String, PathBuf)>, String>;

/// A folder made with "New folder" whose name is being typed in place.
struct NewFolder {
    path: PathBuf,
    text: String,
    focus: bool,
}

pub(crate) struct FolderPicker {
    purpose: PickFor,
    /// The tab the command came from (receives errors and the result selection).
    tab: u64,
    dir: PathBuf,
    back: Vec<PathBuf>,
    address: String,
    /// Subfolders of `dir`: (name, path), naturally sorted.
    folders: Vec<(String, PathBuf)>,
    /// Pending background listing of `dir`; replacing it drops (and so ignores) an older one.
    loading: Option<Receiver<Listing>>,
    list_error: Option<String>,
    /// A clicked subfolder; it is the destination instead of `dir` while selected (like Windows).
    selected: Option<PathBuf>,
    new_folder: Option<NewFolder>,
    scroll_to_selected: bool,
    /// Last failed action (bad path typed, folder could not be created / renamed).
    problem: Option<String>,
}

/// Why `name` cannot be used as a folder name, if it can't.
fn name_problem(name: &str) -> Option<&'static str> {
    let n = name.trim();
    let bad = |c: char| {
        matches!(c, '/' | '\\')
            || (cfg!(windows) && (matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control()))
    };
    if n.is_empty() {
        Some("Enter a name for the new folder.")
    } else if n == "." || n == ".." || n.chars().any(bad) {
        Some(if cfg!(windows) {
            "A folder name can't contain any of these characters: \\ / : * ? \" < > |"
        } else {
            "A folder name can't contain \"/\"."
        })
    } else {
        None
    }
}

fn list_folders(dir: &Path, show_hidden: bool, with_ico: bool) -> Listing {
    let rd = fs::read_dir(dir).map_err(|e| format!("Could not read this folder: {e}"))?;
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    let mut icos: Vec<(String, PathBuf)> = Vec::new();
    for item in rd.flatten() {
        let Ok(ft) = item.file_type() else { continue };
        let path = item.path();
        let is_dir = ft.is_dir() || (ft.is_symlink() && path.is_dir());
        let is_ico = with_ico && !is_dir && is_ico_ext(&path);
        if !(is_dir || is_ico) {
            continue;
        }
        let name = item.file_name().to_string_lossy().into_owned();
        if !show_hidden && item.metadata().is_ok_and(|md| is_hidden_entry(&name, &md)) {
            continue;
        }
        if is_ico {
            icos.push((name, path));
        } else {
            out.push((name, path));
        }
    }
    sort_folders(&mut out);
    sort_folders(&mut icos);
    out.extend(icos); // folders first, then the .ico files
    Ok(out)
}

fn sort_folders(v: &mut [(String, PathBuf)]) {
    v.sort_by(|a, b| natural_cmp(&a.0.to_lowercase(), &b.0.to_lowercase()));
}

impl FolderPicker {
    fn new(purpose: PickFor, tab: u64, start: PathBuf, show_hidden: bool, ctx: &egui::Context) -> Self {
        let mut p = Self {
            purpose,
            tab,
            dir: PathBuf::new(),
            back: Vec::new(),
            address: String::new(),
            folders: Vec::new(),
            loading: None,
            list_error: None,
            selected: None,
            new_folder: None,
            scroll_to_selected: false,
            problem: None,
        };
        p.show(start, show_hidden, ctx);
        p
    }

    /// Whether `.ico` files are listed and one of them is picked (instead of a folder).
    fn wants_files(&self) -> bool {
        matches!(self.purpose, PickFor::Icon { .. })
    }

    /// The folder the command will use: the selected subfolder, else the open folder.
    fn target(&self) -> &Path {
        self.selected.as_deref().unwrap_or(&self.dir)
    }

    /// Shows `dir` and lists its subfolders in the background (a slow network share must not
    /// freeze the window).
    fn show(&mut self, dir: PathBuf, show_hidden: bool, ctx: &egui::Context) {
        self.address = dir.display().to_string();
        self.dir = dir.clone();
        self.folders.clear();
        self.list_error = None;
        self.selected = None;
        self.new_folder = None;
        self.problem = None;
        let (tx, rx) = mpsc::channel();
        self.loading = Some(rx);
        let ctx = ctx.clone();
        let with_ico = self.wants_files();
        std::thread::spawn(move || {
            let _ = tx.send(list_folders(&dir, show_hidden, with_ico));
            ctx.request_repaint();
        });
    }

    /// Opens a folder the user picked or typed; a path that is not a folder only reports a problem.
    fn go(&mut self, path: PathBuf, show_hidden: bool, ctx: &egui::Context) {
        let path = if path.is_absolute() { path } else { self.dir.join(path) };
        match dunce::canonicalize(&path) {
            Ok(p) if p.is_dir() => {
                let p = clean_path(&p);
                if p != self.dir {
                    self.back.push(std::mem::take(&mut self.dir));
                }
                self.show(p, show_hidden, ctx);
            }
            _ => {
                self.problem = Some(format!("Can't find the folder “{}”.", path.display()));
                self.address = self.dir.display().to_string();
            }
        }
    }

    fn poll(&mut self) {
        let Some(rx) = &self.loading else { return };
        match rx.try_recv() {
            Ok(Ok(v)) => {
                self.folders = v;
                self.loading = None;
            }
            Ok(Err(e)) => {
                self.list_error = Some(e);
                self.loading = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => self.loading = None,
        }
    }

    /// "New folder": creates it right away (like Explorer) and lets the user type its name.
    fn make_folder(&mut self) {
        let path = unique_copy_name(&self.dir.join("New folder"));
        if let Err(e) = fs::create_dir(&path) {
            self.problem = Some(format!("Could not create the folder: {e}"));
            return;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.folders.push((name.clone(), path.clone()));
        sort_folders(&mut self.folders);
        self.selected = Some(path.clone());
        self.scroll_to_selected = true;
        self.problem = None;
        self.new_folder = Some(NewFolder {
            path,
            text: name,
            focus: true,
        });
    }

    /// Ends typing the new folder's name; `keep` false leaves the name it was created with.
    fn finish_new_folder(&mut self, keep: bool) {
        let Some(nf) = self.new_folder.take() else { return };
        let text = nf.text.trim();
        let old = nf
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !keep || text == old {
            return;
        }
        if let Some(why) = name_problem(text) {
            self.problem = Some(why.to_owned());
            return;
        }
        let new = nf.path.with_file_name(text);
        // (a case-only rename is the same item on Windows / macOS, not a clash)
        if new.exists() && !text.eq_ignore_ascii_case(&old) {
            self.problem = Some(format!("There is already a folder named “{text}” here."));
            return;
        }
        if let Err(e) = fs::rename(&nf.path, &new) {
            self.problem = Some(format!("Could not rename the folder: {e}"));
            return;
        }
        if let Some(f) = self.folders.iter_mut().find(|f| f.1 == nf.path) {
            *f = (text.to_owned(), new.clone());
        }
        sort_folders(&mut self.folders);
        self.selected = Some(new);
        self.scroll_to_selected = true;
    }
}

/// What the user did in the dialog this frame.
#[derive(Default)]
struct PickerInput {
    go: Option<PathBuf>,
    back: bool,
    new_folder: bool,
    accept: bool,
    cancel: bool,
}

/// Fluent 2 text-field focus indicator: a 2 px accent line along the bottom edge of the field.
fn focus_underline(ui: &Ui, resp: &egui::Response, m: &Metrics, pal: &Palette) {
    if !resp.has_focus() {
        return;
    }
    let r = resp.rect;
    let y = r.bottom() - 1.0;
    let inset = m.radius as f32 * 0.6;
    ui.painter().line_segment(
        [pos2(r.left() + inset, y), pos2(r.right() - inset, y)],
        Stroke::new(2.0_f32, pal.accent),
    );
}

/// Small chevron drawn from two strokes (no font glyph needed): "this row opens".
fn chevron_right(ui: &Ui, c: egui::Pos2, size: f32, color: Color32) {
    let s = size * 0.5;
    let st = Stroke::new(1.4_f32, color);
    ui.painter().line_segment([pos2(c.x - s * 0.4, c.y - s), pos2(c.x + s * 0.4, c.y)], st);
    ui.painter().line_segment([pos2(c.x + s * 0.4, c.y), pos2(c.x - s * 0.4, c.y + s)], st);
}

/// Fluent 2 "message bar" (error intent): tinted card with a round "!" badge.
fn message_bar(ui: &mut Ui, m: &Metrics, pal: &Palette, text: &str) {
    let tint = pal.danger;
    egui::Frame::new()
        .fill(tint.gamma_multiply(0.12))
        .stroke(Stroke::new(1.0_f32, tint.gamma_multiply(0.35)))
        .corner_radius(CornerRadius::same(m.radius))
        .inner_margin(Margin::symmetric((m.pad * 0.8) as i8, (m.pad * 0.6) as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let d = m.font * 1.1;
                let (rect, _) = ui.allocate_exact_size(vec2(d, d), Sense::hover());
                ui.painter().circle_filled(rect.center(), d * 0.5, tint);
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    "!",
                    FontId::proportional(m.font * 0.8),
                    Color32::WHITE,
                );
                ui.add(egui::Label::new(RichText::new(text).color(pal.text)).wrap());
            });
        });
}

/// Quiet card with a caption and one value line (the path is cut off with an ellipsis).
fn info_card(ui: &mut Ui, m: &Metrics, pal: &Palette, caption: &str, value: &str) {
    egui::Frame::new()
        .fill(fl_card_fill(pal))
        .stroke(Stroke::new(1.0_f32, pal.divider))
        .corner_radius(CornerRadius::same(m.radius))
        .inner_margin(Margin::symmetric((m.pad * 0.8) as i8, (m.pad * 0.6) as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                egui::Label::new(RichText::new(caption).size(m.font * 0.9).color(pal.text_secondary)).wrap(),
            );
            ui.add_space(m.pad * 0.2);
            ui.add(egui::Label::new(RichText::new(value).color(pal.text)).truncate());
        });
}

impl Explorer {
    /// "Extract to…": asks where to unpack `zips` (starting next to them).
    pub(crate) fn open_extract_to(&mut self, ti: usize, zips: Vec<PathBuf>) {
        let zips: Vec<PathBuf> = zips.into_iter().filter(|z| z.is_file()).collect();
        let Some(parent) = zips.first().and_then(|z| z.parent()) else {
            return;
        };
        // A zip inside an opened zip lives in the temporary cache: start next to the outer zip.
        let start = folder_outside(parent);
        let name = match zips.as_slice() {
            [one] => one
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            _ => String::new(),
        };
        let purpose = PickFor::Extract { zips, name, show: true };
        self.picker = Some(FolderPicker::new(
            purpose,
            self.tabs[ti].id,
            start,
            self.settings.show_hidden,
            &self.ui,
        ));
    }

    /// "Change folder icon…": asks for an `.ico` file (starting in the folder itself, or where the
    /// current custom icon lives).
    pub(crate) fn open_icon_picker(&mut self, ti: usize, folder: PathBuf) {
        let start = custom_icon_of(&folder)
            .and_then(|i| i.parent().map(Path::to_path_buf))
            .filter(|d| d.is_dir())
            .unwrap_or_else(|| folder.clone());
        self.picker = Some(FolderPicker::new(
            PickFor::Icon { folder },
            self.tabs[ti].id,
            start,
            self.settings.show_hidden,
            &self.ui,
        ));
    }

    /// Gives `folder` the icon `icon` (an `.ico` file), or the normal one for `None`.
    pub(crate) fn set_folder_icon(&mut self, folder: &Path, icon: Option<PathBuf>) {
        self.settings.folder_icons.retain(|k, _| !same_path(Path::new(k), folder));
        if let Some(i) = icon {
            self.settings
                .folder_icons
                .insert(folder.to_string_lossy().into_owned(), i.to_string_lossy().into_owned());
        }
        save_settings(&self.settings);
        self.sync_special_icons(); // re-keys the listings on screen
    }

    pub(crate) fn ui_picker(&mut self, ui: &mut Ui) {
        let Some(mut p) = self.picker.take() else { return };
        p.poll();
        let (m, pal) = (self.m, self.pal);
        let show_hidden = self.settings.show_hidden;
        let (quick, drives) = (&self.quick, &self.drives);
        let mut input = PickerInput::default();

        // The dialog must fit into the window: its frame adds `2 * pad` on each side (see
        // `dialog_shell`), plus a little air. Only the folder list grows and shrinks; the height of
        // everything else is measured each frame (first frame: a guess).
        let screen = ui.content_rect();
        let chrome = m.pad * 6.0;
        let width = (screen.width() - chrome).clamp(m.s * 200.0, m.s * 720.0);
        let rest_id = Id::new("picker_rest_h");
        let rest_h = ui.data(|d| d.get_temp::<f32>(rest_id)).unwrap_or(m.s * 330.0);
        let body_h = (screen.height() - chrome - rest_h).clamp(m.row_h * 2.0, m.s * 340.0);
        // In a narrow window the navigation pane goes; the path field and Up still get anywhere.
        let show_nav = width >= m.s * 480.0;
        let icon_mode = p.wants_files();
        let (ok_label, intro) = match &p.purpose {
            PickFor::Icon { folder } => (
                "Set icon",
                format!(
                    "Choose an .ico file to use as the icon of “{}”.",
                    folder
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| folder.display().to_string())
                ),
            ),
            PickFor::Extract { zips, .. } => (
                "Extract",
                match zips.as_slice() {
                    [one] => format!(
                        "Choose the folder to extract “{}” into.",
                        one.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    ),
                    _ => format!("Choose the folder to extract the {} archives into.", zips.len()),
                },
            ),
        };

        let closed = dialog_shell(
            ui,
            &m,
            &pal,
            "folder_picker",
            if icon_mode { "Choose a folder icon" } else { "Select a destination" },
            true,
            width,
            |ui| {
                ui.label(RichText::new(&intro).color(pal.text_secondary));
                ui.add_space(m.pad * 0.6);
                // ---- Back / Up / path
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = m.s * 2.0;
                    if icon_button(ui, &m, &pal, "back", "Back", !p.back.is_empty(), false).clicked() {
                        input.back = true;
                    }
                    let up = p.dir.parent().map(Path::to_path_buf);
                    if icon_button(ui, &m, &pal, "up", "Up", up.is_some(), false).clicked() {
                        input.go = up;
                    }
                    ui.add_space(m.pad * 0.5);
                    let addr_id = Id::new("picker_address");
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut p.address)
                            .id(addr_id)
                            .background_color(pal.control)
                            .desired_width(ui.available_width())
                            .min_size(vec2(0.0, m.ctl_h))
                            .vertical_align(Align::Center)
                            .hint_text("Type a folder path"),
                    );
                    focus_underline(ui, &resp, &m, &pal);
                    if resp.lost_focus() {
                        if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            input.go = Some(PathBuf::from(expand_env(p.address.trim())));
                        } else {
                            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                // Esc only leaves the field; it must not close the dialog.
                                ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
                            }
                            p.address = p.dir.display().to_string();
                        }
                    }
                });
                ui.add_space(m.pad * 0.6);

                // ---- navigation pane | folder list
                // (Both panes need an explicit top-down layout: a child ui inherits the row's
                // left-to-right one, which would put the list items side by side.)
                ui.horizontal_top(|ui| {
                    let nav_w = (width * 0.3).clamp(m.s * 150.0, m.s * 210.0);
                    let top_down = Layout::top_down_justified(Align::Min);
                    if show_nav {
                        ui.allocate_ui_with_layout(vec2(nav_w, body_h), top_down, |ui| {
                            ui.set_width(nav_w);
                            egui::ScrollArea::vertical()
                                .id_salt("picker_nav")
                                .max_height(body_h)
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    ui.spacing_mut().item_spacing.y = 0.0;
                                    section_label(ui, &m, &pal, "Quick access");
                                    for q in quick.iter().filter(|q| q.available) {
                                        let sel = same_path(&q.path, &p.dir);
                                        if nav_item_ex(ui, &m, &pal, q.icon, Some(q.path.as_path()), &q.label, sel)
                                            .clicked()
                                        {
                                            input.go = Some(q.path.clone());
                                        }
                                    }
                                    if !drives.is_empty() {
                                        ui.add_space(m.pad * 0.5);
                                        section_label(ui, &m, &pal, "Drives");
                                    }
                                    for d in drives {
                                        let mount = d.mount_point.display().to_string();
                                        let label = if d.name.is_empty() {
                                            mount.clone()
                                        } else {
                                            format!("{} ({})", d.name, mount.trim_end_matches(['\\', '/']))
                                        };
                                        // Unreachable share: don't ask the shell for its icon (can block).
                                        let offline = d.total == 0 && d.kind == DriveKind::Network;
                                        let icon_path = (!offline).then_some(d.mount_point.as_path());
                                        let sel = same_path(&d.mount_point, &p.dir);
                                        let resp = nav_item_ex(ui, &m, &pal, "drive", icon_path, &label, sel);
                                        if resp.on_hover_text(&mount).clicked() {
                                            input.go = Some(d.mount_point.clone());
                                        }
                                    }
                                });
                        });
                        ui.add_space(m.pad * 0.4);
                        let (line, _) = ui.allocate_exact_size(vec2(1.0, body_h), Sense::hover());
                        ui.painter().rect_filled(line, CornerRadius::ZERO, pal.divider);
                        ui.add_space(m.pad * 0.4);
                    }

                    let list_w = ui.available_width();
                    ui.allocate_ui_with_layout(vec2(list_w, body_h), top_down, |ui| {
                        ui.spacing_mut().item_spacing.y = m.pad * 0.3;
                        // Heading: the open folder and how many subfolders it holds.
                        let head_h = m.font * 2.0;
                        let title = p
                            .dir
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| p.dir.display().to_string());
                        let count = (p.loading.is_none() && p.list_error.is_none()).then(|| {
                            let n = p.folders.len();
                            let what = if icon_mode { "item" } else { "folder" };
                            format!("{n} {what}{}", if n == 1 { "" } else { "s" })
                        });
                        ui.allocate_ui_with_layout(vec2(list_w, head_h), Layout::left_to_right(Align::Center), |ui| {
                            ui.add_space(m.pad * 0.3);
                            ui.label(RichText::new(elide(&title, 30)).strong().size(m.font * 1.05));
                            if let Some(count) = &count {
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.add_space(m.pad * 0.3);
                                    ui.label(RichText::new(count).size(m.font * 0.9).color(pal.text_secondary));
                                });
                            }
                        });
                        let card_h = (body_h - head_h - m.pad * 0.3).max(m.row_h * 2.0);
                        egui::Frame::new()
                            .fill(fl_card_fill(&pal))
                            .stroke(Stroke::new(1.0_f32, pal.divider))
                            .corner_radius(CornerRadius::same(m.radius))
                            .inner_margin(Margin::same((m.pad * 0.4) as i8))
                            .show(ui, |ui| {
                                let inner_h = card_h - m.pad * 0.8;
                                ui.set_width(ui.available_width());
                                ui.set_height(inner_h);
                                folder_list(ui, &m, &pal, &mut p, inner_h, &mut input);
                            });
                    });
                });
                ui.add_space(m.pad);

                // ---- what will happen
                let target = p.target().to_path_buf();
                let mut blocker: Option<String> = None;
                if archive_of(&target).is_some() {
                    blocker = Some("An opened zip archive is read-only. Choose a folder outside of it.".to_owned());
                } else if p.loading.is_none() && p.list_error.is_some() && p.selected.is_none() {
                    blocker = p.list_error.clone();
                }
                match &mut p.purpose {
                    PickFor::Icon { .. } => {
                        let chosen = p.selected.as_ref().filter(|f| is_ico_path(f));
                        let text = chosen
                            .map(|f| f.display().to_string())
                            .unwrap_or_else(|| "Select an .ico file from the list.".to_owned());
                        info_card(ui, &m, &pal, "Selected icon", &text);
                    }
                    PickFor::Extract { zips, name, show } => {
                        if zips.len() == 1 {
                            ui.horizontal(|ui| {
                                ui.allocate_ui_with_layout(
                                    vec2(m.s * 92.0, m.ctl_h),
                                    Layout::left_to_right(Align::Center),
                                    |ui| {
                                        ui.label(RichText::new("Folder name").color(pal.text_secondary));
                                    },
                                );
                                let resp = ui.add(
                                    egui::TextEdit::singleline(name)
                                        .id(Id::new("picker_name"))
                                        .background_color(pal.control)
                                        .desired_width(ui.available_width())
                                        .min_size(vec2(0.0, m.ctl_h))
                                        .vertical_align(Align::Center),
                                );
                                focus_underline(ui, &resp, &m, &pal);
                                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                    input.accept = true;
                                }
                            });
                            ui.add_space(m.pad * 0.6);
                            match name_problem(name) {
                                Some(why) if blocker.is_none() => blocker = Some(why.to_owned()),
                                Some(_) => {}
                                None => {
                                    let dest = unique_copy_name(&target.join(name.trim()));
                                    info_card(ui, &m, &pal, "Files will be extracted to", &dest.display().to_string());
                                }
                            }
                        } else {
                            info_card(
                                ui,
                                &m,
                                &pal,
                                &format!("Each of the {} archives is extracted into its own new folder in", zips.len()),
                                &target.display().to_string(),
                            );
                        }
                        ui.add_space(m.pad * 0.6);
                        ui.checkbox(show, "Show extracted files when complete");
                    }
                }
                if let Some(msg) = p.problem.as_ref().or(blocker.as_ref()) {
                    ui.add_space(m.pad * 0.6);
                    message_bar(ui, &m, &pal, msg);
                }
                ui.add_space(m.pad * 1.5);

                // ---- buttons
                ui.horizontal(|ui| {
                    let can_make = !icon_mode && archive_of(&p.dir).is_none() && p.list_error.is_none();
                    if ui
                        .add_enabled_ui(can_make, |ui| secondary_button(ui, &m, "New folder"))
                        .inner
                        .clicked()
                    {
                        input.new_folder = true;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if secondary_button(ui, &m, "Cancel").clicked() {
                            input.cancel = true;
                        }
                        let ok = blocker.is_none()
                            && if icon_mode {
                                p.selected.as_ref().is_some_and(|f| is_ico_path(f))
                            } else {
                                target.is_dir()
                            };
                        if ui
                            .add_enabled_ui(ok, |ui| primary_button(ui, &m, &pal, ok_label))
                            .inner
                            .clicked()
                        {
                            input.accept = true;
                        }
                        if !ok {
                            input.accept = false;
                        }
                    });
                });
                // Everything but the folder list, for sizing the list next frame.
                let rest = ui.min_rect().height() - body_h;
                if (rest - rest_h).abs() > 0.5 {
                    ui.data_mut(|d| d.insert_temp(rest_id, rest));
                    ui.ctx().request_repaint();
                }
            },
        );

        if closed || input.cancel {
            // A new folder that is still being named stays (it was really created), like in Explorer.
            p.finish_new_folder(true);
            return;
        }
        if input.back {
            if let Some(prev) = p.back.pop() {
                p.show(prev, show_hidden, &self.ui);
            }
        } else if let Some(dir) = input.go {
            p.go(dir, show_hidden, &self.ui);
        } else if input.new_folder {
            p.finish_new_folder(true);
            p.make_folder();
        }
        if input.accept {
            p.problem = None; // (an old message must not block; only a failed rename now does)
            p.finish_new_folder(true);
            if p.problem.is_none() {
                self.accept_picker(p);
                return;
            }
        }
        self.picker = Some(p);
    }

    fn accept_picker(&mut self, p: FolderPicker) {
        let ti = self.tabs.iter().position(|t| t.id == p.tab).unwrap_or(self.active);
        let target = p.target().to_path_buf();
        match p.purpose {
            PickFor::Icon { folder } => {
                if let Some(icon) = p.selected.filter(|f| is_ico_path(f)) {
                    self.set_folder_icon(&folder, Some(icon));
                }
            }
            PickFor::Extract { zips, name, show } => {
                let jobs: Vec<(PathBuf, PathBuf)> = if zips.len() == 1 {
                    vec![(zips[0].clone(), unique_copy_name(&target.join(name.trim())))]
                } else {
                    zips.into_iter()
                        .filter_map(|z| {
                            let dest = unique_copy_name(&target.join(z.file_stem()?));
                            Some((z, dest))
                        })
                        .collect()
                };
                self.run_extract(ti, jobs, show);
            }
        }
    }
}

/// The subfolders of the open folder: click selects, double click opens. The folder being named
/// after "New folder" shows a text field in its row.
fn folder_list(ui: &mut Ui, m: &Metrics, pal: &Palette, p: &mut FolderPicker, height: f32, input: &mut PickerInput) {
    let row_h = m.row_h;
    let icon_mode = p.wants_files();
    let message = if p.loading.is_some() {
        Some("Loading…".to_owned())
    } else if let Some(e) = &p.list_error {
        Some(e.clone())
    } else if p.folders.is_empty() {
        Some(if icon_mode { "No folders or .ico files here." } else { "This folder has no subfolders." }.to_owned())
    } else {
        None
    };
    if let Some(text) = message {
        let rect = ui.available_rect_before_wrap();
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            text,
            FontId::proportional(m.font),
            pal.text_secondary,
        );
        return;
    }

    let mut scroll = egui::ScrollArea::vertical()
        .id_salt(("picker_list", &p.dir))
        .max_height(height)
        .auto_shrink([false, false]);
    if std::mem::take(&mut p.scroll_to_selected) {
        if let Some(i) = p.folders.iter().position(|f| Some(&f.1) == p.selected.as_ref()) {
            scroll = scroll.vertical_scroll_offset((i as f32 * row_h - height / 2.0).max(0.0));
        }
    }
    let mut finish: Option<bool> = None;
    scroll.show_rows(ui, row_h, p.folders.len(), |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for (name, path) in &p.folders[range] {
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
            let inner = rect.shrink2(vec2(0.0, 1.0));
            // In icon mode the list also holds .ico files: those are picked, folders are opened.
            let ico_row = icon_mode && is_ico_ext(path) && !path.is_dir();
            let selected = p.selected.as_ref() == Some(path);
            if selected {
                ui.painter()
                    .rect_filled(inner, CornerRadius::same(m.radius), sel_fill(pal, resp.hovered()));
                paint_sel_marker(ui, inner, m, pal);
            } else {
                list_hover(ui, inner, &resp, pal, m);
            }
            let icon_c = pos2(inner.left() + m.pad * 1.5 + m.icon / 2.0, inner.center().y);
            draw_path_icon(
                ui,
                icon_c,
                m.icon,
                &make_icon_key(!ico_row, if ico_row { Some("ico") } else { None }, path),
                path,
                if ico_row { "image" } else { "folder" },
                Color32::WHITE,
            );
            let text_rect = Rect::from_min_max(
                pos2(inner.left() + m.pad * 2.3 + m.icon, inner.top()),
                pos2(inner.right() - m.pad * 3.2, inner.bottom()),
            );
            let renaming_here = p.new_folder.as_ref().is_some_and(|nf| &nf.path == path);
            if (resp.hovered() || selected) && !renaming_here && !ico_row {
                chevron_right(ui, pos2(inner.right() - m.pad * 1.6, inner.center().y), m.font * 0.8, pal.text_secondary);
            }
            match &mut p.new_folder {
                Some(nf) if &nf.path == path => {
                    let id = Id::new("picker_new_folder");
                    let te = egui::TextEdit::singleline(&mut nf.text)
                        .id(id)
                        .background_color(pal.control)
                        .desired_width(text_rect.width())
                        .vertical_align(Align::Center);
                    let r = ui.put(text_rect.shrink2(vec2(0.0, m.s * 3.0)), te);
                    if nf.focus {
                        nf.focus = false;
                        r.request_focus();
                        select_range(ui, id, 0, nf.text.chars().count());
                    } else if r.lost_focus() {
                        let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
                        if esc {
                            ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
                        }
                        finish = Some(!esc);
                    }
                }
                _ => paint_text(ui, text_rect, name, FontId::proportional(m.font), pal.text, false),
            }
            if resp.double_clicked() {
                if ico_row {
                    p.selected = Some(path.clone());
                    input.accept = true;
                } else {
                    input.go = Some(path.clone());
                }
            } else if resp.clicked() && icon_mode && !ico_row {
                input.go = Some(path.clone()); // folders are only navigated, never chosen
            } else if resp.clicked() {
                // Clicking the selected folder again selects the open folder instead (like Windows,
                // where the open folder is the destination when nothing is selected).
                p.selected = if selected { None } else { Some(path.clone()) };
            }
        }
    });
    if let Some(keep) = finish {
        p.finish_new_folder(keep);
    }
}
