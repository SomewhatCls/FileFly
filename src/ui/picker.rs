//! "Select a destination" / "Choose a folder icon": a Fluent-style picker in a window of its own.
//!
//! It is laid out like the main explorer window: custom title bar, navigation row (Back / Forward /
//! Up / Refresh, breadcrumb address bar, search box), command bar, navigation pane on the left
//! (Quick access + drives) and the folder list on the right, with the action area at the bottom.
//! Like the progress windows (`ops_dialog.rs`) it is an immediate viewport: a separate OS window
//! with our own caption buttons and the same backdrop as the main window.
//!
//! `PickFor` says what the chosen folder is for, so other commands ("Copy to…", "Move to…") can
//! reuse the dialog.

use crate::prelude::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrd};
use crate::ui::toolbar::crumbs;

/// Numbers the picker windows, so every opening gets a fresh viewport and a fresh backdrop state.
static PICKER_SEQ: AtomicU64 = AtomicU64::new(0);

// ==============================================================================================
// Model

/// What the chosen folder is for.
pub(crate) enum PickFor {
    /// "Extract to…": the zips are unpacked into the chosen folder. With `subfolder` every zip gets
    /// a new folder of its own inside it (`name` is that folder's name, only asked for when there
    /// is a single zip); without it the files go straight into the chosen folder.
    Extract {
        zips: Vec<PathBuf>,
        name: String,
        show: bool,
        subfolder: bool,
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

type Listing = Result<Vec<Entry>, String>;

/// Messages from the background search.
enum SearchMsg {
    Batch(Vec<Entry>),
    Done,
}

pub(crate) struct FolderPicker {
    purpose: PickFor,
    /// The tab the command came from (receives errors and the result selection).
    tab: u64,
    dir: PathBuf,
    back: Vec<PathBuf>,
    fwd: Vec<PathBuf>,
    /// The picker has its own "hidden items" toggle, starting from the explorer's setting.
    show_hidden: bool,
    /// Date format and column widths of the explorer, copied when the dialog opens.
    fmt: DateFormat,
    cols: [f32; 3],
    address: String,
    editing_addr: bool,
    addr_focus: bool,
    /// What is on screen: a `Tab` of its own (never part of `Explorer::tabs`), drawn by the same
    /// `table_ui` as the explorer's panes. Its selection is the picked item (at most one).
    list: Tab,
    /// Pending background listing of `dir`; replacing it drops (and so ignores) an older one.
    loading: Option<Receiver<Listing>>,
    list_error: Option<String>,
    /// Last failed action (bad path typed, folder could not be created / renamed).
    problem: Option<String>,
    /// Search text; non-empty means the list shows `list.search_results` instead of `list.entries`.
    search: String,
    search_rx: Option<Receiver<SearchMsg>>,
    search_cancel: Option<Arc<AtomicBool>>,
    search_running: bool,
    /// Where the window opens (centered over the main window), fixed after the first frame.
    win_pos: Option<egui::Pos2>,
    /// Unique per opening (see `PICKER_SEQ`).
    instance: u64,
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

fn list_folders(dir: &Path, show_hidden: bool, fmt: DateFormat, with_ico: bool) -> Listing {
    // The same listing the explorer's tabs use, cut down to what the picker offers.
    let mut v = read_listing(dir, show_hidden, fmt)?;
    v.retain(|e| e.is_dir || (with_ico && is_ico_ext(&e.path)));
    Ok(v)
}

/// Breadth-first search below `root` for folders (and `.ico` files) whose name contains `query`
/// (already lower-cased). Results arrive in batches; `cancel` stops the walk.
#[allow(clippy::too_many_arguments)]
fn search_walk(
    root: PathBuf,
    query: String,
    show_hidden: bool,
    with_ico: bool,
    fmt: DateFormat,
    cancel: Arc<AtomicBool>,
    tx: mpsc::Sender<SearchMsg>,
    ctx: egui::Context,
) {
    const MAX_HITS: usize = 5000;
    let mut queue = std::collections::VecDeque::from([root]);
    let mut batch: Vec<Entry> = Vec::new();
    let mut found = 0usize;
    while let Some(dir) = queue.pop_front() {
        if cancel.load(AtomicOrd::Relaxed) {
            return;
        }
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for item in rd.flatten() {
            if cancel.load(AtomicOrd::Relaxed) {
                return;
            }
            let Ok(ft) = item.file_type() else { continue };
            let name = item.file_name().to_string_lossy().into_owned();
            let md = item.metadata().ok();
            if !show_hidden && md.as_ref().is_some_and(|md| is_hidden_entry(&name, md)) {
                continue;
            }
            let path = item.path();
            let is_dir = ft.is_dir(); // symlinks are not followed: no loops
            let is_ico = with_ico && ft.is_file() && is_ico_ext(&path);
            if !(is_dir || is_ico) {
                continue;
            }
            if is_dir {
                queue.push_back(path.clone());
            }
            if name.to_lowercase().contains(&query) {
                let Some(md) = md else { continue };
                batch.push(Entry::from_path(name, path, &md, fmt));
                found += 1;
                if batch.len() >= 40 || found >= MAX_HITS {
                    if tx.send(SearchMsg::Batch(std::mem::take(&mut batch))).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
                if found >= MAX_HITS {
                    let _ = tx.send(SearchMsg::Done);
                    ctx.request_repaint();
                    return;
                }
            }
        }
    }
    if !batch.is_empty() {
        let _ = tx.send(SearchMsg::Batch(batch));
    }
    let _ = tx.send(SearchMsg::Done);
    ctx.request_repaint();
}

impl FolderPicker {
    fn new(purpose: PickFor, tab: u64, start: PathBuf, settings: &Settings, ctx: &egui::Context) -> Self {
        let instance = PICKER_SEQ.fetch_add(1, AtomicOrd::Relaxed);
        let mut p = Self {
            purpose,
            tab,
            dir: PathBuf::new(),
            back: Vec::new(),
            fwd: Vec::new(),
            show_hidden: settings.show_hidden,
            fmt: settings.date_format,
            cols: settings.col_widths,
            address: String::new(),
            editing_addr: false,
            addr_focus: false,
            // An id no explorer tab has, so per-tab UI state (column widths, scroll) never collides.
            list: Tab::new(u64::MAX - instance, PathBuf::new()),
            loading: None,
            list_error: None,
            problem: None,
            search: String::new(),
            search_rx: None,
            search_cancel: None,
            search_running: false,
            win_pos: None,
            instance,
        };
        p.show(start, ctx);
        p
    }

    /// Whether `.ico` files are listed and one of them is picked (instead of a folder).
    fn wants_files(&self) -> bool {
        matches!(self.purpose, PickFor::Icon { .. })
    }

    fn searching(&self) -> bool {
        !self.search.trim().is_empty()
    }

    /// The clicked row (the table keeps at most one selected in picker mode). It is the
    /// destination instead of `dir` while selected, like in Windows.
    fn selected(&self) -> Option<&PathBuf> {
        self.list.selected.iter().next()
    }

    fn select(&mut self, path: Option<PathBuf>) {
        self.list.selected.clear();
        self.list.anchor = path.clone();
        if let Some(p) = path {
            self.list.selected.insert(p);
        }
    }

    /// The table filters by `list.search`; keep it equal to the search box.
    fn sync_search_text(&mut self) {
        if self.list.search != self.search {
            self.list.search.clone_from(&self.search);
            self.list.view_dirty = true;
        }
    }

    /// The folder the command will use: the selected subfolder, else the open folder.
    fn target(&self) -> &Path {
        self.selected().map(PathBuf::as_path).unwrap_or(&self.dir)
    }

    /// Why the command cannot run on the current target, if it can't.
    fn blocker(&self) -> Option<String> {
        if archive_of(self.target()).is_some() {
            return Some("An opened zip archive is read-only. Choose a folder outside of it.".to_owned());
        }
        if let PickFor::Extract { zips, name, subfolder, .. } = &self.purpose {
            if *subfolder && zips.len() == 1 {
                if let Some(why) = name_problem(name) {
                    return Some(why.to_owned());
                }
            }
        }
        None
    }

    fn can_accept(&self) -> bool {
        // (the table's error bar explains an unreadable folder; it is only usable via a subfolder)
        let unreadable = self.loading.is_none() && self.list_error.is_some() && self.selected().is_none();
        self.blocker().is_none()
            && !unreadable
            && match &self.purpose {
                PickFor::Icon { .. } => self.selected().is_some_and(|f| is_ico_path(f)),
                PickFor::Extract { .. } => self.target().is_dir(),
            }
    }

    /// Shows `dir` and lists it in the background (a slow network share must not freeze the window).
    fn show(&mut self, dir: PathBuf, ctx: &egui::Context) {
        self.stop_search();
        self.search.clear();
        self.address = dir.display().to_string();
        self.editing_addr = false;
        self.dir = dir.clone();
        self.list_error = None;
        self.problem = None;
        let l = &mut self.list;
        l.dir = dir.clone();
        l.search.clear();
        l.entries.clear();
        l.search_results.clear();
        l.view.clear();
        l.view_dirty = true;
        l.rename = None;
        l.error = None;
        l.loading = true;
        l.clear_selection();
        let (tx, rx) = mpsc::channel();
        self.loading = Some(rx);
        let ctx = ctx.clone();
        let (with_ico, show_hidden, fmt) = (self.wants_files(), self.show_hidden, self.fmt);
        std::thread::spawn(move || {
            let _ = tx.send(list_folders(&dir, show_hidden, fmt, with_ico));
            ctx.request_repaint();
        });
    }

    /// Opens a folder the user picked or typed; a path that is not a folder only reports a problem.
    fn go(&mut self, path: PathBuf, ctx: &egui::Context) {
        let path = if path.is_absolute() { path } else { self.dir.join(path) };
        match dunce::canonicalize(&path) {
            Ok(p) if p.is_dir() => {
                let p = clean_path(&p);
                if p != self.dir {
                    self.back.push(std::mem::take(&mut self.dir));
                    self.fwd.clear();
                }
                self.show(p, ctx);
            }
            _ => {
                self.problem = Some(format!("Can't find the folder “{}”.", path.display()));
                self.address = self.dir.display().to_string();
            }
        }
    }

    fn go_back(&mut self, ctx: &egui::Context) {
        if let Some(prev) = self.back.pop() {
            self.fwd.push(self.dir.clone());
            self.show(prev, ctx);
        }
    }

    fn go_forward(&mut self, ctx: &egui::Context) {
        if let Some(next) = self.fwd.pop() {
            self.back.push(self.dir.clone());
            self.show(next, ctx);
        }
    }

    fn poll(&mut self) {
        if let Some(rx) = &self.loading {
            match rx.try_recv() {
                Ok(Ok(v)) => {
                    self.list.entries = v;
                    self.list.view_dirty = true;
                    self.list.loading = false;
                    self.loading = None;
                }
                Ok(Err(e)) => {
                    self.list.error = Some(e.clone());
                    self.list_error = Some(e);
                    self.list.loading = false;
                    self.loading = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.list.loading = false;
                    self.loading = None;
                }
            }
        }
        if let Some(rx) = &self.search_rx {
            loop {
                match rx.try_recv() {
                    Ok(SearchMsg::Batch(b)) => {
                        self.list.search_results.extend(b);
                        self.list.view_dirty = true;
                    }
                    Ok(SearchMsg::Done) => {
                        self.search_running = false;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.search_running = false;
                        break;
                    }
                }
            }
        }
    }

    /// Cancels a running search.
    fn stop_search(&mut self) {
        if let Some(c) = self.search_cancel.take() {
            c.store(true, AtomicOrd::Relaxed);
        }
        self.search_rx = None;
        self.search_running = false;
    }

    /// (Re)starts the search for the current `search` text below the open folder.
    fn start_search(&mut self, ctx: &egui::Context) {
        self.stop_search();
        self.list.search_results.clear();
        self.list.view_dirty = true;
        self.list.rename = None;
        self.select(None);
        let query = self.search.trim().to_lowercase();
        if query.is_empty() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        self.search_cancel = Some(cancel.clone());
        self.search_rx = Some(rx);
        self.search_running = true;
        let (root, show_hidden, with_ico, fmt, ctx) =
            (self.dir.clone(), self.show_hidden, self.wants_files(), self.fmt, ctx.clone());
        std::thread::spawn(move || search_walk(root, query, show_hidden, with_ico, fmt, cancel, tx, ctx));
    }

    fn clear_search(&mut self) {
        self.stop_search();
        self.search.clear();
        self.list.search.clear();
        self.list.search_results.clear();
        self.list.view_dirty = true;
        self.select(None);
    }

    fn make_folder(&mut self) {
        if self.searching() {
            self.clear_search();
        }
        let path = unique_copy_name(&self.dir.join("New folder"));
        if let Err(e) = fs::create_dir(&path) {
            self.problem = Some(format!("Could not create the folder: {e}"));
            return;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Ok(md) = fs::metadata(&path) else { return };
        self.list.entries.push(Entry::from_path(name.clone(), path.clone(), &md, self.fmt));
        self.reveal(path.clone());
        self.problem = None;
        // The table's own inline rename takes over, like "New folder" in the explorer.
        self.list.rename = Some(RenameState {
            text: name,
            is_dir: true,
            path,
            focus_pending: true,
        });
    }

    /// Sorts the list again, selects `path` and scrolls to it.
    fn reveal(&mut self, path: PathBuf) {
        self.list.view_dirty = true;
        rebuild_view(&mut self.list);
        self.list.scroll_to = self.list.view.iter().position(|&i| self.list.entries[i].path == path);
        self.select(Some(path));
    }

    /// Ends typing a folder name; `keep` false leaves the name it was created with.
    fn finish_rename(&mut self, keep: bool) {
        if let Some(rs) = self.list.rename.take() {
            if keep {
                self.rename_folder(rs.path, rs.text);
            }
        }
    }

    fn rename_folder(&mut self, path: PathBuf, text: String) {
        let text = text.trim();
        let old = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if text == old {
            return;
        }
        if let Some(why) = name_problem(text) {
            self.problem = Some(why.to_owned());
            return;
        }
        let new = path.with_file_name(text);
        // (a case-only rename is the same item on Windows / macOS, not a clash)
        if new.exists() && !text.eq_ignore_ascii_case(&old) {
            self.problem = Some(format!("There is already a folder named “{text}” here."));
            return;
        }
        if let Err(e) = fs::rename(&path, &new) {
            self.problem = Some(format!("Could not rename the folder: {e}"));
            return;
        }
        if let Some(e) = self.list.entries.iter_mut().find(|e| e.path == path) {
            e.name = text.to_owned();
            e.name_lc = text.to_lowercase();
            e.path = new.clone();
            e.icon_key = make_icon_key(true, None, &new);
        }
        self.reveal(new);
    }

    /// Starts typing a new name for the folder `path` (F2 / context menu).
    fn begin_rename(&mut self, path: PathBuf) {
        if !path.is_dir() || archive_of(&path).is_some() {
            return;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.select(Some(path.clone()));
        self.problem = None;
        self.list.rename = Some(RenameState {
            text: name,
            is_dir: true,
            path,
            focus_pending: true,
        });
    }

    /// Sends `path` (a folder, or an `.ico` file in icon mode) to the recycle bin and drops it from the list.
    fn delete_item(&mut self, path: PathBuf) {
        self.list.rename = None;
        if archive_of(&path).is_some() {
            self.problem = Some("An opened zip archive is read-only.".to_owned());
            return;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // NOTE: if the explorer deletes through a helper of its own, call that here instead.
        if let Err(e) = trash::delete(&path) {
            self.problem = Some(format!("Could not delete “{name}”: {e}"));
            return;
        }
        self.problem = None;
        self.list.entries.retain(|e| e.path != path);
        self.list.search_results.retain(|e| e.path != path);
        self.list.view_dirty = true;
        if self.selected() == Some(&path) {
            self.select(None);
        }
        // Back / Forward must not lead into the deleted folder.
        self.back.retain(|d| !d.starts_with(&path));
        self.fwd.retain(|d| !d.starts_with(&path));
    }

    /// Selects the row at position `vi` of the shown list and scrolls to it.
    fn select_view_index(&mut self, vi: usize) {
        let Some(&ei) = self.list.view.get(vi) else { return };
        let entries = if self.searching() { &self.list.search_results } else { &self.list.entries };
        let path = entries[ei].path.clone();
        self.list.scroll_to = Some(vi);
        self.select(Some(path));
    }

    /// Up / Down / PageUp / PageDown: moves the selection by `delta` rows (nothing selected yet:
    /// starts at the first row going down, at the last going up).
    fn move_selection(&mut self, delta: isize) {
        let n = self.list.view.len();
        if n == 0 {
            return;
        }
        let entries = if self.searching() { &self.list.search_results } else { &self.list.entries };
        let cur = self
            .selected()
            .and_then(|s| self.list.view.iter().position(|&i| &entries[i].path == s));
        let next = match cur {
            Some(c) => (c as isize + delta).clamp(0, n as isize - 1) as usize,
            None if delta > 0 => 0,
            None => n - 1,
        };
        self.select_view_index(next);
    }
}

/// What the user did in the dialog this frame.
#[derive(Default)]
struct PickerInput {
    go: Option<PathBuf>,
    back: bool,
    forward: bool,
    refresh: bool,
    new_folder: bool,
    toggle_hidden: bool,
    accept: bool,
    cancel: bool,
    /// An item to delete (context menu or Del).
    delete: Option<PathBuf>,
}

// ==============================================================================================
// Widgets

/// Esc inside a text field only leaves the field; it must not close the window.
fn eat_escape(ui: &Ui) -> bool {
    ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
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

/// Breadcrumb address bar; click empty space (or "…") to type a path.
fn picker_address(ui: &mut Ui, m: &Metrics, pal: &Palette, p: &mut FolderPicker, width: f32, input: &mut PickerInput) {
    let (pill, pill_resp) = ui.allocate_exact_size(vec2(width, m.ctl_h), Sense::click());
    let r = CornerRadius::same(m.radius);
    ui.painter().rect_filled(pill, r, pal.control);
    ui.painter()
        .rect_stroke(pill, r, Stroke::new(1.0_f32, pal.control_stroke), StrokeKind::Inside);
    let inner = pill.shrink2(vec2(m.pad * 0.4, 0.0));

    if p.editing_addr {
        let id = Id::unique("pk_addr_edit");
        let te = egui::TextEdit::singleline(&mut p.address)
            .id(id)
            .frame(egui::Frame::NONE)
            .margin(Margin::symmetric(4, 0))
            .desired_width(inner.width())
            .align(Align2::LEFT_CENTER)
            .hint_text("Type a folder path and press Enter");
        // A child ui (not `put`) keeps the margins of the field stable.
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::left_to_right(Align::Center)),
        );
        let resp = child.add(te);
        if p.addr_focus {
            resp.request_focus();
            select_range(ui, id, 0, p.address.chars().count());
            p.addr_focus = false;
        }
        ui.painter().line_segment(
            [
                pill.left_bottom() + vec2(m.accessibility_scale * 3.0, -1.0),
                pill.right_bottom() + vec2(-m.accessibility_scale * 3.0, -1.0),
            ],
            Stroke::new(2.0 * m.accessibility_scale, pal.accent),
        );
        if resp.lost_focus() {
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                input.go = Some(PathBuf::from(expand_env(p.address.trim())));
            } else {
                eat_escape(ui);
            }
            p.editing_addr = false;
            p.address = p.dir.display().to_string();
        }
        return;
    }

    if pill_resp.clicked() {
        p.editing_addr = true;
        p.addr_focus = true;
        p.address = p.dir.display().to_string();
    }

    let segs = crumbs(&p.dir);
    let seg_pad = m.pad * 0.7;
    let chev = m.small_icon * 0.7;
    let sep_w = chev + m.accessibility_scale * 4.0;
    let more_w = m.ctl_h * 0.7;
    let widths: Vec<f32> = segs
        .iter()
        .map(|(l, _)| text_width(ui, l, m.font) + seg_pad * 2.0)
        .collect();

    // Always show the current folder; add parents from the right while they fit.
    let mut used = 0.0;
    let mut first = segs.len();
    for i in (0..segs.len()).rev() {
        let need = widths[i] + if i + 1 < segs.len() { sep_w } else { 0.0 };
        let reserve = if i > 0 { more_w + sep_w } else { 0.0 };
        if first < segs.len() && used + need + reserve > inner.width() {
            break;
        }
        used += need;
        first = i;
    }

    let top = inner.top() + m.accessibility_scale * 3.0;
    let h = inner.height() - m.accessibility_scale * 6.0;
    let mut x = inner.left();
    if first > 0 {
        let rect = Rect::from_min_size(pos2(x, top), vec2(more_w, h));
        let resp = ui.interact(rect, Id::unique("pk_crumb_more"), Sense::click());
        hover_fill(ui, rect, &resp, pal, m);
        paint_text(ui, rect, "…", FontId::proportional(m.font), pal.text, false);
        if resp.clicked() {
            p.editing_addr = true;
            p.addr_focus = true;
            p.address = p.dir.display().to_string();
        }
        x += more_w;
        draw_icon(ui, pos2(x + sep_w / 2.0, inner.center().y), chev, "chevron_right", pal.text_secondary);
        x += sep_w;
    }
    for i in first..segs.len() {
        if i > first {
            draw_icon(ui, pos2(x + sep_w / 2.0, inner.center().y), chev, "chevron_right", pal.text_secondary);
            x += sep_w;
        }
        let rect = Rect::from_min_size(pos2(x, top), vec2(widths[i].min((inner.right() - x).max(0.0)), h));
        let resp = ui.interact(rect, Id::unique(("pk_crumb", i)), Sense::click());
        hover_fill(ui, rect, &resp, pal, m);
        paint_text(
            ui,
            rect.shrink2(vec2(seg_pad, 0.0)),
            &segs[i].0,
            FontId::proportional(m.font),
            pal.text,
            false,
        );
        if resp.clicked() {
            input.go = Some(segs[i].1.clone());
        }
        x += rect.width();
    }
}

/// Search box (searches the open folder and everything below it). Returns true when the text changed.
fn picker_search(ui: &mut Ui, m: &Metrics, pal: &Palette, p: &mut FolderPicker, width: f32) -> bool {
    let (pill, _) = ui.allocate_exact_size(vec2(width, m.ctl_h), Sense::hover());
    let r = CornerRadius::same(m.radius);
    ui.painter().rect_filled(pill, r, pal.control);
    ui.painter()
        .rect_stroke(pill, r, Stroke::new(1.0_f32, pal.control_stroke), StrokeKind::Inside);

    // Magnifier at the right end, clear button to its left. Both slots are always reserved so
    // the layout never shifts while typing.
    let icon_w = m.small_icon + m.pad * 2.0;
    let icon_rect = Rect::from_min_max(pos2(pill.right() - icon_w, pill.top()), pill.right_bottom());
    let clear_w = m.ctl_h * 0.8;
    let clear_rect = Rect::from_center_size(
        pos2(icon_rect.left() - clear_w / 2.0, pill.center().y),
        vec2(clear_w, clear_w),
    );
    let te_rect = Rect::from_min_max(
        pill.min + vec2(m.pad * 0.4, 0.0),
        pill.max - vec2(icon_w + clear_w, 0.0),
    );
    let folder = p
        .dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.dir.display().to_string());
    let te = egui::TextEdit::singleline(&mut p.search)
        .id(Id::unique("pk_search"))
        .frame(egui::Frame::NONE)
        .margin(Margin::symmetric(4, 0))
        .desired_width(te_rect.width())
        .align(Align2::LEFT_CENTER)
        .hint_text(format!("Search {}", truncate(&folder, 18)));
    let resp = ui
        .put(te_rect, te)
        .on_hover_text("Search this folder (Ctrl+F)");
    let mut changed = resp.changed();
    if resp.has_focus() {
        ui.painter().line_segment(
            [
                pill.left_bottom() + vec2(m.accessibility_scale * 3.0, -1.0),
                pill.right_bottom() + vec2(-m.accessibility_scale * 3.0, -1.0),
            ],
            Stroke::new(2.0 * m.accessibility_scale, pal.accent),
        );
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            eat_escape(ui);
            if !p.search.is_empty() {
                p.search.clear();
                changed = true;
            }
            resp.surrender_focus();
        }
    }
    if !p.search.is_empty() {
        let cr = ui.interact(clear_rect, Id::unique("pk_search_clear"), Sense::click());
        hover_fill(ui, clear_rect, &cr, pal, m);
        draw_icon(ui, clear_rect.center(), m.small_icon * 0.8, "close", pal.text);
        if cr.clicked() {
            p.search.clear();
            changed = true;
        }
    }
    draw_icon(
        ui,
        pos2(icon_rect.center().x, pill.center().y),
        m.small_icon,
        "search",
        pal.text_secondary,
    );
    changed
}

/// The list of folders (and `.ico` files in icon mode, or search hits). It is the explorer's own
/// file table (`table_ui`, picker mode): click selects, double click opens a folder, and a folder
/// made with "New folder" is named in place.
fn folder_list(ui: &mut Ui, m: &Metrics, pal: &Palette, p: &mut FolderPicker, input: &mut PickerInput) {
    p.sync_search_text();
    if p.list.view_dirty {
        rebuild_view(&mut p.list);
    }
    let can_make = !p.wants_files() && archive_of(&p.dir).is_none() && p.list_error.is_none();
    let hit = ui.max_rect();
    let mut actions: Vec<Action> = Vec::new();
    table_ui(
        ui,
        m,
        pal,
        &mut p.list,
        &mut p.cols,
        None,
        false,
        false,
        hit,
        TableMode::Picker { can_make },
        &mut actions,
    );
    // The table reports what happened as `Action`s; the picker only cares about a few of them.
    for a in actions {
        match a {
            Action::Open(path) => {
                if path.is_dir() {
                    input.go = Some(path);
                } else {
                    // an `.ico` file: choose it
                    p.select(Some(path));
                    input.accept = true;
                }
            }
            Action::NewFolder => input.new_folder = true,
            Action::CommitRename(path, text) => {
                p.list.rename = None;
                p.rename_folder(path, text);
            }
            Action::CancelRename => {
                p.list.rename = None;
                eat_escape(ui); // Esc ends the name box, not the dialog
            }
            Action::Rename(path) => p.begin_rename(path),
            Action::Delete(paths) => input.delete = paths.into_iter().next(),
            _ => {}
        }
    }
}

// ==============================================================================================
// The window

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
        let purpose = PickFor::Extract { zips, name, show: true, subfolder: false };
        self.picker = Some(FolderPicker::new(
            purpose,
            self.tabs[ti].id,
            start,
            &self.settings,
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
            &self.settings,
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
        let transparency = self.backdrop_transparency;
        let ctx = ui.ctx().clone();
        let (quick, drives) = (&self.quick, &self.drives);
        let mut input = PickerInput::default();
        let icon_mode = p.wants_files();

        // While the window is open it needs frames for the async listing / search results.
        if p.loading.is_some() || p.search_running {
            ctx.request_repaint_after(Duration::from_millis(50));
        }

        let (ok_label, title) = match &p.purpose {
            PickFor::Icon { .. } => ("Set icon", "Choose a folder icon"),
            PickFor::Extract { .. } => ("Extract", "Select a destination"),
        };

        let win_title = format!("{title} – FileFly");
        let (win_w, win_h) = (m.accessibility_scale * 900.0, m.accessibility_scale * 600.0);
        if p.win_pos.is_none() {
            p.win_pos = ctx
                .input(|i| i.viewport().outer_rect)
                .map(|r| r.center() - vec2(win_w, win_h) / 2.0);
        }
        let mut builder = egui::ViewportBuilder::default()
            .with_title(win_title.clone())
            .with_inner_size([win_w, win_h])
            .with_min_inner_size([m.accessibility_scale * 560.0, m.accessibility_scale * 420.0])
            .with_resizable(true)
            .with_maximize_button(false)
            .with_decorations(!self.use_custom_window_frame)
            .with_transparent(true);
        if let Some(pos) = p.win_pos {
            builder = builder.with_position(pos);
        }
        let bar_h = (m.accessibility_scale * 32.0).round();
        // Rounded corners of the card where it meets the sidebar (none without a backdrop).
        let card_r: u8 = m.radius + 2;
        let instance = p.instance;
        let vid = egui::ViewportId::from_hash_of(("folder-picker", instance));

        ctx.show_viewport_immediate(vid, builder, |ui, _class| {
            // Whether a text box had the keyboard when the frame began (a field that loses focus
            // to Enter / Esc during this frame must not also trigger the shortcuts below).
            let typing = ui.egui_wants_keyboard_input();
            // Same backdrop as the main window (retried until the native window exists, redone
            // when the theme changes).
            let key = Id::unique(("folder-picker-backdrop", instance));
            let want = (pal.dark, transparency.expect("A transparency effect should always be set"));
            if ui.ctx().data(|d| d.get_temp::<(bool, TransparencyEffect)>(key)) != Some(want)
                && backdrop_by_title(&win_title, pal.dark, want.1)
            {
                ui.ctx().data_mut(|d| d.insert_temp(key, want));
            }
            if ui.ctx().input(|i| i.viewport().close_requested()) {
                input.cancel = true;
            }
            // Undecorated windows have no native resize border: same edge handling as the main window.
            if self.use_custom_window_frame {
                handle_resize(ui.ctx(), &m);
            }

            // ---- title bar
            egui::Panel::top("pk_title")
                .show_separator_line(false)
                .exact_size(bar_h)
                .frame(bar_frame(&pal, 0.0, 0.0, 0.0, 0.0))
                .show(ui, |ui| {
                    let full = ui.max_rect();
                    let caption_w = m.accessibility_scale * 46.0 * 2.0;
                    let drag = ui.interact(
                        Rect::from_min_max(full.min, pos2(full.right() - caption_w, full.bottom())),
                        Id::unique("pk_drag"),
                        Sense::click_and_drag(),
                    );
                    if drag.drag_started_by(egui::PointerButton::Primary) {
                        ui.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    ui.painter().text(
                        pos2(full.left() + m.pad * 2.0, full.center().y),
                        Align2::LEFT_CENTER,
                        title,
                        FontId::proportional(m.font * 0.95),
                        pal.text,
                    );
                    if self.use_custom_window_frame {
                        caption_buttons(ui, &m, &pal, false, false);
                    }
                });

            // ---- navigation pane: Quick access + drives
            egui::Panel::left("pk_side")
                .resizable(true)
                .default_size(
                    self.sidebar_width
                        .unwrap_or(m.accessibility_scale * 200.0)
                )
                .size_range(
                    m.accessibility_scale * 150.0..=m.accessibility_scale * 400.0
                )
                .show_separator_line(false)
                .frame(bar_frame(&pal, m.pad * 0.6, m.pad * 0.6, m.pad * 0.4, m.pad * 0.4))
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("pk_nav_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            section_label(ui, &m, &pal, "Quick access");
                            for q in quick.iter().filter(|q| q.available) {
                                let sel = same_path(&q.path, &p.dir);
                                if nav_item_ex(ui, &m, &pal, q.icon, Some(q.path.as_path()), &q.label, sel).clicked() {
                                    input.go = Some(q.path.clone());
                                }
                            }
                            let mut any = false;
                            for (head, kind) in [
                                ("System drives", DriveKind::System),
                                ("Removable drives", DriveKind::Removable),
                                ("Network drives", DriveKind::Network),
                            ] {
                                let group: Vec<&DriveInfo> = drives.iter().filter(|d| d.kind == kind).collect();
                                if group.is_empty() {
                                    continue;
                                }
                                any = true;
                                ui.add_space(m.pad);
                                section_label(ui, &m, &pal, head);
                                for d in group {
                                    // Produces the drive card the same way as in sidebar.
                                    let sel = same_path(&d.mount_point, &p.dir);
                                    if drive_card(ui, &m, &pal, d, sel).clicked() {
                                        input.go = Some(d.mount_point.clone());
                                    }
                                }
                            }
                            if !any {
                                ui.add_space(m.pad);
                                section_label(ui, &m, &pal, "No drives found");
                            }
                        });
                });

            // ---- navigation row: Back / Forward / Up / Refresh, address bar, search
            egui::Panel::top("pk_nav")
                .show_separator_line(false)
                .frame(band_frame(&pal, card_r, m.pad, m.pad, m.pad * 0.7, m.pad * 0.3))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = m.accessibility_scale * 2.0;
                        if icon_button(ui, &m, &pal, "back", "Back (Alt+←)", !p.back.is_empty(), false).clicked() {
                            input.back = true;
                        }
                        if icon_button(ui, &m, &pal, "forward", "Forward (Alt+→)", !p.fwd.is_empty(), false).clicked() {
                            input.forward = true;
                        }
                        let up = p.dir.parent().map(Path::to_path_buf);
                        if icon_button(ui, &m, &pal, "up", "Up (Alt+↑)", up.is_some(), false).clicked() {
                            input.go = up;
                        }
                        if icon_button(ui, &m, &pal, "refresh", "Refresh (F5)", true, false).clicked() {
                            input.refresh = true;
                        }
                        ui.add_space(m.pad * 0.5);
                        let gap = ui.spacing().item_spacing.x + m.pad * 0.5;
                        let search_w = (m.accessibility_scale * 240.0).min(ui.available_width() * 0.38);
                        let addr_w = (ui.available_width() - search_w - gap).max(m.accessibility_scale * 120.0);
                        picker_address(ui, &m, &pal, &mut p, addr_w, &mut input);
                        ui.add_space(m.pad * 0.5);
                        if picker_search(ui, &m, &pal, &mut p, search_w) {
                            p.start_search(ui.ctx());
                        }
                    });
                });

            // ---- command bar
            egui::Panel::top("pk_cmd")
                .show_separator_line(false)
                .frame(layer_frame(&pal, m.radius, m.pad, m.pad, 0.0, m.pad * 0.3, false))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = m.accessibility_scale * 2.0;
                        let can_make = !icon_mode && archive_of(&p.dir).is_none() && p.list_error.is_none();
                        if cmd_button(ui, &m, &pal, "new_folder", Some("New folder"), "New folder (Ctrl + Shift + F)", can_make, false)
                            .clicked()
                        {
                            input.new_folder = true;
                        }
                        vsep(ui, &m, &pal);
                        if cmd_button(ui, &m, &pal, "eye", Some("Hidden items"), "Show hidden items", true, p.show_hidden)
                            .clicked()
                        {
                            input.toggle_hidden = true;
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let n = p.list.view.len();
                            let mut text = format!("{n} {}{}", if icon_mode { "item" } else { "folder" }, if n == 1 { "" } else { "s" });
                            if p.search_running {
                                text.push_str("   |   searching…");
                            }
                            if p.loading.is_none() && p.list_error.is_none() {
                                ui.label(RichText::new(text).size(m.font * 0.93).color(pal.text_secondary));
                            }
                        });
                    });
                });

            // ---- bottom: destination, options, buttons
            egui::Panel::bottom("pk_footer")
                .show_separator_line(false)
                .frame(layer_frame(&pal, m.radius, m.pad * 1.5, m.pad * 1.5, m.pad * 0.5, m.pad * 1.1, false).corner_radius(CornerRadius::ZERO))
                .show(ui, |ui| {
                    let blocker = p.blocker();
                    if let Some(msg) = p.problem.as_ref().or(blocker.as_ref()) {
                        message_bar(ui, &m, &pal, msg);
                        ui.add_space(m.pad * 0.6);
                    }
                    let ok = p.can_accept();
                    ui.horizontal(|ui| {
                        if let PickFor::Extract { show, .. } = &mut p.purpose {
                            ui.checkbox(show, "Show extracted files when complete");
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if secondary_button(ui, &m, "Cancel").clicked() {
                                input.cancel = true;
                            }
                            if ui
                                .add_enabled_ui(ok, |ui| primary_button(ui, &m, &pal, ok_label))
                                .inner
                                .clicked()
                            {
                                input.accept = true;
                            }
                        });
                    });
                });

            // ---- the folder list
            egui::CentralPanel::default()
                .frame(layer_frame(&pal, 0, m.pad, m.pad, m.pad * 0.4, m.pad * 0.4, false))
                .show(ui, |ui| {
                    folder_list(ui, &m, &pal, &mut p, &mut input);
                });

            // ---- keyboard: the explorer's shortcuts (see `shortcuts.rs`), for this window's own input
            let can_make = !icon_mode && archive_of(&p.dir).is_none() && p.list_error.is_none();
            let (mut focus_address, mut focus_search) = (false, false);
            ui.input_mut(|i| {
                use egui::{Key, Modifiers as M};
                // These work while a text box has the keyboard, too. (Shift variants first.)
                if i.consume_key(M::CTRL | M::SHIFT, Key::F) || i.consume_key(M::CTRL | M::SHIFT, Key::N) {
                    input.new_folder |= can_make;
                }
                if i.consume_key(M::CTRL, Key::L) || i.consume_key(M::ALT, Key::D) {
                    focus_address = true;
                }
                if i.consume_key(M::CTRL, Key::F) || i.consume_key(M::CTRL, Key::E) {
                    focus_search = true;
                }
                if i.consume_key(M::NONE, Key::F5) {
                    input.refresh = true;
                }
                if i.consume_key(M::ALT, Key::ArrowLeft) {
                    input.back = true;
                }
                if i.consume_key(M::ALT, Key::ArrowRight) {
                    input.forward = true;
                }
                if i.consume_key(M::ALT, Key::ArrowUp) && input.go.is_none() {
                    input.go = p.dir.parent().map(Path::to_path_buf);
                }
                // Mouse "back" / "forward" buttons.
                if i.pointer.button_pressed(egui::PointerButton::Extra1) {
                    input.back = true;
                }
                if i.pointer.button_pressed(egui::PointerButton::Extra2) {
                    input.forward = true;
                }
                if typing {
                    return;
                }
                if i.consume_key(M::NONE, Key::Escape) {
                    input.cancel = true;
                }
                if i.consume_key(M::NONE, Key::Backspace) {
                    input.back = true;
                }
                if i.consume_key(M::NONE, Key::ArrowDown) {
                    p.move_selection(1);
                }
                if i.consume_key(M::NONE, Key::ArrowUp) {
                    p.move_selection(-1);
                }
                if i.consume_key(M::NONE, Key::PageDown) {
                    p.move_selection(10);
                }
                if i.consume_key(M::NONE, Key::PageUp) {
                    p.move_selection(-10);
                }
                if i.consume_key(M::NONE, Key::Home) {
                    p.select_view_index(0);
                }
                if i.consume_key(M::NONE, Key::End) {
                    p.select_view_index(p.list.view.len().saturating_sub(1));
                }
                if i.consume_key(M::NONE, Key::Enter) {
                    match p.selected().cloned() {
                        // A folder is opened; an `.ico` file is the answer; with nothing selected
                        // Enter presses the default button.
                        Some(path) if path.is_dir() => input.go = Some(path),
                        _ => input.accept = true,
                    }
                }
                if let Some(path) = p.selected().cloned() {
                    if i.consume_key(M::NONE, Key::F2) {
                        p.begin_rename(path.clone());
                    }
                    if i.consume_key(M::NONE, Key::Delete) {
                        input.delete = Some(path);
                    }
                }
            });
            // (egui state may not be touched while the input is locked, hence after the closure)
            if focus_address {
                p.editing_addr = true;
                p.addr_focus = true;
                p.address = p.dir.display().to_string();
                ui.ctx().request_repaint();
            }
            if focus_search {
                ui.ctx().memory_mut(|mem| mem.request_focus(Id::unique("pk_search")));
            }

            // Draws chrome layer depending on desktop environment
            let corner_radius = match get_desktop() {
                d if d.contains("KDE") => 0.0,
                d if d.contains("GNOME") => 15.0,
                d if d.is_empty() => 8.0,
                _ => 15.0,
            };

            if self.pal.transparency != TransparencyEffect::None {
                ui.painter().rect_filled(ui.ctx().content_rect(), corner_radius, self.pal.chrome);
            } else {
                ui.painter().rect_filled(ui.ctx().content_rect(),
                                         if !Self::is_maximized(ui.ctx()){corner_radius}else{0.0}, self.pal.layer);
            }
        });

        // ---- apply what happened
        if input.cancel {
            // A new folder that is still being named stays.
            p.finish_rename(true);
            p.stop_search();
            return;
        }
        if let Some(path) = input.delete.take() {
            p.delete_item(path);
        }
        if input.toggle_hidden {
            p.show_hidden = !p.show_hidden;
            input.refresh = true;
        }
        if input.back {
            p.go_back(&self.ui);
        } else if input.forward {
            p.go_forward(&self.ui);
        } else if let Some(dir) = input.go {
            p.go(dir, &self.ui);
        } else if input.refresh {
            let (dir, search) = (p.dir.clone(), p.search.clone());
            p.show(dir, &self.ui);
            if !search.trim().is_empty() {
                p.search = search;
                p.start_search(&self.ui);
            }
        } else if input.new_folder {
            p.finish_rename(true);
            p.make_folder();
        }
        if input.accept && p.can_accept() {
            p.problem = None;
            p.finish_rename(true);
            if p.problem.is_none() {
                p.stop_search();
                self.accept_picker(p);
                return;
            }
        }
        self.picker = Some(p);
    }

    fn accept_picker(&mut self, p: FolderPicker) {
        let ti = self.tabs.iter().position(|t| t.id == p.tab).unwrap_or(self.active);
        let target = p.target().to_path_buf();
        let chosen = p.selected().cloned();
        match p.purpose {
            PickFor::Icon { folder } => {
                if let Some(icon) = chosen.filter(|f| is_ico_path(f)) {
                    self.set_folder_icon(&folder, Some(icon));
                }
            }
            PickFor::Extract { zips, name, show, subfolder } => {
                let jobs: Vec<(PathBuf, PathBuf)> = if !subfolder {
                    // Straight into the chosen folder.
                    zips.into_iter().map(|z| (z, target.clone())).collect()
                } else if zips.len() == 1 {
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
