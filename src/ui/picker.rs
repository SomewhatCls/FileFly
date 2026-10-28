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

/// One line of the list: a folder, or (in icon mode) an `.ico` file.
#[derive(Clone)]
struct Row {
    name: String,
    path: PathBuf,
    is_dir: bool,
    size: u64,
}

type Listing = Result<Vec<Row>, String>;

/// Messages from the background search.
enum SearchMsg {
    Batch(Vec<Row>),
    Done,
}

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
    fwd: Vec<PathBuf>,
    /// The picker has its own "hidden items" toggle, starting from the explorer's setting.
    show_hidden: bool,
    address: String,
    editing_addr: bool,
    addr_focus: bool,
    /// Subfolders (and `.ico` files in icon mode) of `dir`, naturally sorted.
    rows: Vec<Row>,
    /// Pending background listing of `dir`; replacing it drops (and so ignores) an older one.
    loading: Option<Receiver<Listing>>,
    list_error: Option<String>,
    /// A clicked row; it is the destination instead of `dir` while selected (like Windows).
    selected: Option<PathBuf>,
    new_folder: Option<NewFolder>,
    scroll_to_selected: bool,
    /// Last failed action (bad path typed, folder could not be created / renamed).
    problem: Option<String>,
    /// Search text; non-empty means the list shows `search_rows` instead of `rows`.
    search: String,
    search_root: PathBuf,
    search_rows: Vec<Row>,
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

fn sort_rows(v: &mut [Row]) {
    // Folders first, then files; each naturally sorted.
    v.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| natural_cmp(&a.name.to_lowercase(), &b.name.to_lowercase()))
    });
}

fn list_folders(dir: &Path, show_hidden: bool, with_ico: bool) -> Listing {
    let rd = fs::read_dir(dir).map_err(|e| format!("Could not read this folder: {e}"))?;
    let mut out: Vec<Row> = Vec::new();
    for item in rd.flatten() {
        let Ok(ft) = item.file_type() else { continue };
        let path = item.path();
        let is_dir = ft.is_dir() || (ft.is_symlink() && path.is_dir());
        let is_ico = with_ico && !is_dir && is_ico_ext(&path);
        if !(is_dir || is_ico) {
            continue;
        }
        let name = item.file_name().to_string_lossy().into_owned();
        let md = item.metadata().ok();
        if !show_hidden && md.as_ref().is_some_and(|md| is_hidden_entry(&name, md)) {
            continue;
        }
        let size = if is_ico { md.map(|m| m.len()).unwrap_or(0) } else { 0 };
        out.push(Row { name, path, is_dir, size });
    }
    sort_rows(&mut out);
    Ok(out)
}

/// Breadth-first search below `root` for folders (and `.ico` files) whose name contains `query`
/// (already lower-cased). Results arrive in batches; `cancel` stops the walk.
fn search_walk(
    root: PathBuf,
    query: String,
    show_hidden: bool,
    with_ico: bool,
    cancel: Arc<AtomicBool>,
    tx: mpsc::Sender<SearchMsg>,
    ctx: egui::Context,
) {
    const MAX_HITS: usize = 5000;
    let mut queue = std::collections::VecDeque::from([root]);
    let mut batch: Vec<Row> = Vec::new();
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
                let size = if is_ico { md.map(|m| m.len()).unwrap_or(0) } else { 0 };
                batch.push(Row { name, path, is_dir, size });
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
    fn new(purpose: PickFor, tab: u64, start: PathBuf, show_hidden: bool, ctx: &egui::Context) -> Self {
        let mut p = Self {
            purpose,
            tab,
            dir: PathBuf::new(),
            back: Vec::new(),
            fwd: Vec::new(),
            show_hidden,
            address: String::new(),
            editing_addr: false,
            addr_focus: false,
            rows: Vec::new(),
            loading: None,
            list_error: None,
            selected: None,
            new_folder: None,
            scroll_to_selected: false,
            problem: None,
            search: String::new(),
            search_root: PathBuf::new(),
            search_rows: Vec::new(),
            search_rx: None,
            search_cancel: None,
            search_running: false,
            win_pos: None,
            instance: PICKER_SEQ.fetch_add(1, AtomicOrd::Relaxed),
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

    /// The rows on screen.
    fn view(&self) -> &Vec<Row> {
        if self.searching() {
            &self.search_rows
        } else {
            &self.rows
        }
    }

    /// The folder the command will use: the selected subfolder, else the open folder.
    fn target(&self) -> &Path {
        self.selected.as_deref().unwrap_or(&self.dir)
    }

    /// Why the command cannot run on the current target, if it can't.
    fn blocker(&self) -> Option<String> {
        if archive_of(self.target()).is_some() {
            return Some("An opened zip archive is read-only. Choose a folder outside of it.".to_owned());
        }
        if self.loading.is_none() && self.list_error.is_some() && self.selected.is_none() {
            return self.list_error.clone();
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
        self.blocker().is_none()
            && match &self.purpose {
                PickFor::Icon { .. } => self.selected.as_ref().is_some_and(|f| is_ico_path(f)),
                PickFor::Extract { .. } => self.target().is_dir(),
            }
    }

    /// Shows `dir` and lists it in the background (a slow network share must not freeze the window).
    fn show(&mut self, dir: PathBuf, ctx: &egui::Context) {
        self.stop_search();
        self.search.clear();
        self.search_rows.clear();
        self.address = dir.display().to_string();
        self.editing_addr = false;
        self.dir = dir.clone();
        self.rows.clear();
        self.list_error = None;
        self.selected = None;
        self.new_folder = None;
        self.problem = None;
        let (tx, rx) = mpsc::channel();
        self.loading = Some(rx);
        let ctx = ctx.clone();
        let (with_ico, show_hidden) = (self.wants_files(), self.show_hidden);
        std::thread::spawn(move || {
            let _ = tx.send(list_folders(&dir, show_hidden, with_ico));
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
                    self.rows = v;
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
        if let Some(rx) = &self.search_rx {
            loop {
                match rx.try_recv() {
                    Ok(SearchMsg::Batch(b)) => self.search_rows.extend(b),
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
        self.search_rows.clear();
        self.selected = None;
        self.new_folder = None;
        let query = self.search.trim().to_lowercase();
        if query.is_empty() {
            return;
        }
        self.search_root = self.dir.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        self.search_cancel = Some(cancel.clone());
        self.search_rx = Some(rx);
        self.search_running = true;
        let (root, show_hidden, with_ico, ctx) =
            (self.dir.clone(), self.show_hidden, self.wants_files(), ctx.clone());
        std::thread::spawn(move || search_walk(root, query, show_hidden, with_ico, cancel, tx, ctx));
    }

    fn clear_search(&mut self) {
        self.stop_search();
        self.search.clear();
        self.search_rows.clear();
        self.selected = None;
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
        self.rows.push(Row {
            name: name.clone(),
            path: path.clone(),
            is_dir: true,
            size: 0,
        });
        sort_rows(&mut self.rows);
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
        if let Some(r) = self.rows.iter_mut().find(|r| r.path == nf.path) {
            r.name = text.to_owned();
            r.path = new.clone();
        }
        sort_rows(&mut self.rows);
        self.selected = Some(new);
        self.scroll_to_selected = true;
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
}

// ==============================================================================================
// Widgets

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

/// Small chevron drawn from two strokes: "this row opens".
fn chevron_right(ui: &Ui, c: egui::Pos2, size: f32, color: Color32) {
    let s = size * 0.5;
    let st = Stroke::new(1.4_f32, color);
    ui.painter().line_segment([pos2(c.x - s * 0.4, c.y - s), pos2(c.x + s * 0.4, c.y)], st);
    ui.painter().line_segment([pos2(c.x + s * 0.4, c.y), pos2(c.x - s * 0.4, c.y + s)], st);
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
        .hint_text(format!("Search {}", elide(&folder, 18)));
    let resp = ui.put(te_rect, te);
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

/// The list of folders (and `.ico` files in icon mode, or search hits): click selects, double
/// click opens a folder. The folder being named after "New folder" shows a text field in its row.
fn folder_list(ui: &mut Ui, m: &Metrics, pal: &Palette, p: &mut FolderPicker, input: &mut PickerInput) {
    let row_h = m.row_h;
    let icon_mode = p.wants_files();
    let searching = p.searching();
    let avail = ui.available_width();
    let size_w = m.accessibility_scale * 90.0;
    let type_w = m.accessibility_scale * 130.0;
    let name_w = (avail - size_w - type_w).max(m.accessibility_scale * 100.0);

    // ---- column header
    let header_h = row_h * 0.95;
    let (hrect, _) = ui.allocate_exact_size(vec2(avail, header_h), Sense::hover());
    let hfont = FontId::proportional(m.font * 0.93);
    paint_text(
        ui,
        Rect::from_min_max(pos2(hrect.left() + m.pad * 1.7 + m.icon, hrect.top()), pos2(hrect.left() + name_w, hrect.bottom())),
        "Name",
        hfont.clone(),
        pal.text_secondary,
        false,
    );
    paint_text(
        ui,
        Rect::from_min_max(pos2(hrect.left() + name_w + m.pad * 0.6, hrect.top()), pos2(hrect.left() + name_w + type_w, hrect.bottom())),
        "Type",
        hfont.clone(),
        pal.text_secondary,
        false,
    );
    paint_text(
        ui,
        Rect::from_min_max(pos2(hrect.right() - size_w, hrect.top()), pos2(hrect.right() - m.pad * 0.6, hrect.bottom())),
        "Size",
        hfont,
        pal.text_secondary,
        true,
    );
    ui.painter().line_segment(
        [pos2(hrect.left(), hrect.bottom()), pos2(hrect.right(), hrect.bottom())],
        Stroke::new(1.0_f32, pal.divider),
    );
    ui.add_space(m.pad * 0.3);

    // ---- right-click on empty space: New folder (rows are added later and take priority)
    let bg = ui.interact(ui.available_rect_before_wrap(), Id::unique("pk_list_bg"), Sense::click());
    if bg.clicked() {
        p.selected = None;
    }
    let can_make = !icon_mode && archive_of(&p.dir).is_none() && p.list_error.is_none();
    scroll_context_menu(&bg, |ui| {
        if menu_item(ui, m, pal, Some("new_folder"), "New folder", None, can_make) {
            input.new_folder = true;
        }
    });

    // ---- placeholder messages
    let rows_len = p.view().len();
    let message = if p.loading.is_some() {
        Some("Loading…".to_owned())
    } else if let Some(e) = &p.list_error {
        Some(e.clone())
    } else if searching && rows_len == 0 {
        Some(if p.search_running { "Searching…" } else { "No items match your search" }.to_owned())
    } else if rows_len == 0 {
        Some(if icon_mode { "No folders or .ico files here." } else { "This folder has no subfolders." }.to_owned())
    } else {
        None
    };
    if let Some(text) = message {
        let rect = ui.available_rect_before_wrap();
        ui.painter().text(
            pos2(rect.center().x, rect.top() + row_h * 2.5),
            Align2::CENTER_CENTER,
            text,
            FontId::proportional(m.font),
            pal.text_secondary,
        );
        return;
    }

    let height = ui.available_height();
    let mut scroll = egui::ScrollArea::vertical()
        .id_salt(("pk_list", &p.dir, searching))
        .max_height(height)
        .auto_shrink([false, false]);
    if std::mem::take(&mut p.scroll_to_selected) {
        if let Some(i) = p.view().iter().position(|r| Some(&r.path) == p.selected.as_ref()) {
            scroll = scroll.vertical_scroll_offset((i as f32 * row_h - height / 2.0).max(0.0));
        }
    }
    let mut finish: Option<bool> = None;
    let rows: &Vec<Row> = if searching { &p.search_rows } else { &p.rows };
    scroll.show_rows(ui, row_h, rows.len(), |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for row in &rows[range] {
            let (rect, resp) = ui.allocate_exact_size(vec2(avail, row_h), Sense::click());
            let inner = rect.shrink2(vec2(m.accessibility_scale * 2.0, m.accessibility_scale));
            let selected = p.selected.as_ref() == Some(&row.path);
            if selected {
                ui.painter()
                    .rect_filled(inner, CornerRadius::same(m.radius), sel_fill(pal, resp.hovered()));
                paint_sel_marker(ui, rect, m, pal);
            } else {
                list_hover(ui, inner, &resp, pal, m);
            }
            let icon_c = pos2(rect.left() + m.pad + m.icon / 2.0, rect.center().y);
            draw_path_icon(
                ui,
                icon_c,
                m.icon,
                &make_icon_key(row.is_dir, if row.is_dir { None } else { Some("ico") }, &row.path),
                &row.path,
                if row.is_dir { "folder" } else { "image" },
                Color32::WHITE,
            );
            let name_rect = Rect::from_min_max(
                pos2(rect.left() + m.pad * 1.7 + m.icon, rect.top()),
                pos2(rect.left() + name_w - m.pad * 0.5, rect.bottom()),
            );
            let renaming_here = p.new_folder.as_ref().is_some_and(|nf| nf.path == row.path);
            match &mut p.new_folder {
                Some(nf) if renaming_here => {
                    let id = Id::unique("pk_new_folder");
                    let te = egui::TextEdit::singleline(&mut nf.text)
                        .id(id)
                        .background_color(pal.control)
                        .desired_width(name_rect.width())
                        .align(Align2::LEFT_CENTER);
                    let r = ui.put(name_rect.shrink2(vec2(0.0, m.accessibility_scale * 2.0)), te);
                    if nf.focus {
                        nf.focus = false;
                        r.request_focus();
                        select_range(ui, id, 0, nf.text.chars().count());
                    } else if r.lost_focus() {
                        let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
                        if esc {
                            eat_escape(ui);
                        }
                        finish = Some(!esc);
                    }
                }
                _ => {
                    // In search results, show where the item lives relative to the searched folder.
                    let label;
                    let text: &str = if searching {
                        let rel = row.path.parent().and_then(|pp| pp.strip_prefix(&p.search_root).ok());
                        match rel {
                            Some(rel) if !rel.as_os_str().is_empty() => {
                                label = format!("{}   \u{2192}   {}", row.name, rel.display());
                                &label
                            }
                            _ => &row.name,
                        }
                    } else {
                        &row.name
                    };
                    paint_text(ui, name_rect, text, FontId::proportional(m.font), pal.text, false);
                }
            }
            paint_text(
                ui,
                Rect::from_min_max(pos2(rect.left() + name_w + m.pad * 0.6, rect.top()), pos2(rect.left() + name_w + type_w, rect.bottom())),
                if row.is_dir { "File folder" } else { "Icon" },
                FontId::proportional(m.font),
                pal.text_secondary,
                false,
            );
            if !row.is_dir {
                paint_text(
                    ui,
                    Rect::from_min_max(pos2(rect.right() - size_w, rect.top()), pos2(rect.right() - m.pad * 0.6, rect.bottom())),
                    &format_size(row.size),
                    FontId::proportional(m.font),
                    pal.text_secondary,
                    true,
                );
            }
            if (resp.hovered() || selected) && !renaming_here && row.is_dir && !searching {
                // (hint that a double click opens the folder; kept off the size column)
                chevron_right(
                    ui,
                    pos2(rect.left() + name_w - m.pad * 0.2, rect.center().y),
                    m.font * 0.8,
                    pal.text_secondary,
                );
            }

            if resp.double_clicked() && !renaming_here {
                if row.is_dir {
                    input.go = Some(row.path.clone());
                } else {
                    p.selected = Some(row.path.clone());
                    input.accept = true;
                }
            } else if resp.clicked() && !renaming_here {
                // Clicking the selected row again selects the open folder instead (like Windows,
                // where the open folder is the destination when nothing is selected).
                p.selected = if selected { None } else { Some(row.path.clone()) };
            }
        }
    });
    if let Some(keep) = finish {
        p.finish_new_folder(keep);
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
                .resizable(false)
                .exact_size(m.accessibility_scale * 200.0)
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
                        if cmd_button(ui, &m, &pal, "new_folder", Some("New folder"), "Create a new folder here", can_make, false)
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
                            let n = p.view().len();
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

            // ---- keyboard: Esc closes, Alt+arrows navigate, F5 refreshes (not while typing)
            if !ui.egui_wants_keyboard_input() {
                let (esc, left, right, up, f5) = ui.input(|i| {
                    (
                        i.key_pressed(egui::Key::Escape),
                        i.modifiers.alt && i.key_pressed(egui::Key::ArrowLeft),
                        i.modifiers.alt && i.key_pressed(egui::Key::ArrowRight),
                        i.modifiers.alt && i.key_pressed(egui::Key::ArrowUp),
                        i.key_pressed(egui::Key::F5),
                    )
                });
                if esc {
                    input.cancel = true;
                }
                input.back |= left;
                input.forward |= right;
                input.refresh |= f5;
                if up && input.go.is_none() {
                    input.go = p.dir.parent().map(Path::to_path_buf);
                }
            }
        });

        // ---- apply what happened
        if input.cancel {
            // A new folder that is still being named stays.
            p.finish_new_folder(true);
            p.stop_search();
            return;
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
            p.finish_new_folder(true);
            p.make_folder();
        }
        if input.accept && p.can_accept() {
            p.problem = None;
            p.finish_new_folder(true);
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
        match p.purpose {
            PickFor::Icon { folder } => {
                if let Some(icon) = p.selected.filter(|f| is_ico_path(f)) {
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
