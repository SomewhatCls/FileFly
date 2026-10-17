use eframe::egui::{self, vec2, Color32, Pos2, Rect, Ui};
use file_icon_provider::get_file_icon;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, OnceLock, RwLock};
use crate::Entry;

//Consts for Worker thread metrics and icon size
const ICON_SIZE: u16 = 32;
const THREADS: usize = 4;
const MAX_SLOTS: usize = 1024;
const UPLOADS_PER_FRAME: usize = 32;
const KEEP_FRAMES: u64 = 120;

/// Extensions whose icon depends on the individual file, not just the type.
#[cfg(windows)]
const PER_FILE_EXTS: &[&str] = &["exe", "lnk", "ico", "url", "msi", "scr", "cur"];
#[cfg(not(windows))]
const PER_FILE_EXTS: &[&str] = &["desktop", "appimage", "ico"];

/// Folders that get their own platform icon instead of the shared generic folder icon:
/// the well-known user folders (Documents, Downloads, ...), folders pinned to Quick access
/// and drive roots / mount points.
#[derive(Default)]
struct Special {
    folders: HashSet<PathBuf>,
    drives: HashSet<PathBuf>,
}

fn special() -> &'static RwLock<Special> {
    static S: OnceLock<RwLock<Special>> = OnceLock::new();
    S.get_or_init(|| RwLock::new(Special::default()))
}

/// Windows paths are case-insensitive; compare them lower-cased there.
fn norm(p: &Path) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(p.to_string_lossy().to_lowercase())
    } else {
        p.to_path_buf()
    }
}

/// Replaces the set of well-known / pinned folders that show their own system icon.
pub fn set_special_folders<I: IntoIterator<Item = PathBuf>>(paths: I) {
    if let Ok(mut s) = special().write() {
        s.folders = paths.into_iter().map(|p| norm(&p)).collect();
    }
}

/// Replaces the set of drive roots / mount points that show their own system icon.
pub fn set_special_drives<I: IntoIterator<Item = PathBuf>>(paths: I) {
    if let Ok(mut s) = special().write() {
        s.drives = paths.into_iter().map(|p| norm(&p)).collect();
    }
}

fn is_special_dir(path: &Path) -> bool {
    if path.parent().is_none() {
        return true; // drive root (C:\, \\server\share\, /)
    }
    match special().read() {
        Ok(s) => {
            if s.folders.is_empty() && s.drives.is_empty() {
                return false;
            }
            let k = norm(path);
            s.folders.contains(&k) || s.drives.contains(&k)
        }
        Err(_) => false,
    }
}

/// Cache key for a platform icon: one per extension, one per path for per-file icons like .exe on
/// Windows, and one per path for special folders (Documents, drives, pinned folders).
pub fn make_icon_key(is_dir: bool, ext: Option<&str>, path: &Path) -> String {
    if is_dir {
        return if is_special_dir(path) { path.to_string_lossy().into_owned() } else { "<dir>".into() };
    }
    match ext {
        Some(e) if PER_FILE_EXTS.contains(&e) => path.to_string_lossy().into_owned(),
        Some(e) => format!(".{e}"),
        None => "<file>".into(),
    }
}

struct Job {
    key: String,
    path: PathBuf,
}

struct Done {
    key: String,
    img: Option<egui::ColorImage>,
}

struct Queue {
    jobs: Mutex<VecDeque<Job>>,
    cv: Condvar,
    closed: AtomicBool,
}

struct Slot {
    tex: Option<egui::TextureHandle>, // None = platform has no icon, use built-in
    last_used: u64,
}

pub struct IconCache {
    ctx: egui::Context,
    queue: Arc<Queue>,
    rx: mpsc::Receiver<Done>,
    slots: HashMap<String, Slot>,
    pending: HashSet<String>, // queued or in flight
    frame: u64,
}

fn load_icon(path: &Path) -> Option<egui::ColorImage> {
    let icon = get_file_icon(path, ICON_SIZE).ok()?;
    if icon.width == 0 || icon.height == 0 {
        return None;
    }
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [icon.width as usize, icon.height as usize],
        &icon.pixels,
    ))
}

impl IconCache {
    pub fn new(ctx: &egui::Context) -> Self {
        let queue = Arc::new(Queue {
            jobs: Mutex::new(VecDeque::new()),
            cv: Condvar::new(),
            closed: AtomicBool::new(false),
        });
        let (tx, rx) = mpsc::channel();

        for i in 0..THREADS {
            let (q, tx, ctx) = (queue.clone(), tx.clone(), ctx.clone());
            std::thread::Builder::new()
                .name(format!("icon-worker-{i}"))
                .spawn(move || loop {
                    let job = {
                        let mut g = q.jobs.lock().unwrap();
                        loop {
                            if q.closed.load(Ordering::Relaxed) {
                                return;
                            }
                            if let Some(j) = g.pop_front() {
                                break j;
                            }
                            g = q.cv.wait(g).unwrap();
                        }
                    };
                    let img = load_icon(&job.path);
                    if tx.send(Done { key: job.key, img }).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                })
                .expect("spawn icon worker");
        }

        Self { ctx: ctx.clone(), queue, rx, slots: HashMap::new(), pending: HashSet::new(), frame: 0 }
    }

    /// Call once at the top of every frame.
    pub fn begin_frame(&mut self) {
        self.frame += 1;

        // Drop jobs that haven't started: whatever is still visible re-queues itself this frame.
        {
            let mut jobs = self.queue.jobs.lock().unwrap();
            for j in jobs.drain(..) {
                self.pending.remove(&j.key);
            }
        }

        // Turn finished icons into textures, a few per frame.
        for _ in 0..UPLOADS_PER_FRAME {
            let Ok(d) = self.rx.try_recv() else { return self.evict() };
            self.pending.remove(&d.key);
            let tex = d.img.map(|img| {
                self.ctx.load_texture(format!("icon:{}", d.key), img, egui::TextureOptions::LINEAR)
            });
            self.slots.insert(d.key, Slot { tex, last_used: self.frame });
        }
        self.ctx.request_repaint(); // more results may be waiting
        self.evict();
    }

    fn evict(&mut self) {
        if self.slots.len() > MAX_SLOTS {
            let cutoff = self.frame.saturating_sub(KEEP_FRAMES);
            self.slots.retain(|_, s| s.last_used >= cutoff); // dropping the handle frees the texture
        }
    }

    /// Never blocks. None = not ready yet or no platform icon -> caller draws the built-in one.
    pub fn get(&mut self, key: &str, path: &Path) -> Option<egui::TextureHandle> {
        if let Some(slot) = self.slots.get_mut(key) {
            slot.last_used = self.frame;
            return slot.tex.clone();
        }
        if self.pending.insert(key.to_owned()) {
            self.queue.jobs.lock().unwrap().push_back(Job { key: key.to_owned(), path: path.to_owned() });
            self.queue.cv.notify_one();
        }
        None
    }
}

impl Drop for IconCache {
    fn drop(&mut self) {
        self.queue.closed.store(true, Ordering::Relaxed);
        self.queue.cv.notify_all();
    }
}

thread_local! {
    static ICON_CACHE: RefCell<Option<IconCache>> = const { RefCell::new(None) };
}

pub fn icons_begin_frame(ctx: &egui::Context) {
    ICON_CACHE.with(|c| c.borrow_mut().get_or_insert_with(|| IconCache::new(ctx)).begin_frame());
}

/// Draws the platform icon of `path` (cached under `key`); until it is ready, or if the platform has
/// none, the built-in SVG called `fallback` is drawn instead.
#[allow(clippy::too_many_arguments)]
pub fn draw_path_icon(ui: &Ui, center: Pos2, size: f32, key: &str, path: &Path, fallback: &str, tint: Color32) {
    let ctx = ui.ctx().clone();
    let tex = ICON_CACHE.with(|c| c.borrow_mut().get_or_insert_with(|| IconCache::new(&ctx)).get(key, path));
    match tex {
        Some(tex) => {
            let sized = egui::load::SizedTexture::new(tex.id(), vec2(size, size));
            egui::Image::new(sized)
                .tint(Color32::from_white_alpha(tint.a()))
                .paint_at(ui, Rect::from_center_size(center, vec2(size, size)));
        }
        None => draw_icon(ui, center, size, fallback, tint),
    }
}

/// Name of the built-in icon that stands in for an entry until its platform icon has loaded.
pub fn fallback_icon_name(e: &Entry) -> &'static str {
    if e.is_dir {
        "folder"
    } else {
        icon_for_extension(&e.path.extension().and_then(|x| x.to_str()).unwrap_or("").to_lowercase())
    }
}

pub fn draw_system_icon(ui: &Ui, center: Pos2, size: f32, e: &Entry, tint: Color32) {
    draw_path_icon(ui, center, size, &e.icon_key, &e.path, fallback_icon_name(e), tint);
}

/// Built-in SVG icon (moved here from main.rs, so delete it there).
pub fn draw_icon(ui: &Ui, center: Pos2, size: f32, name: &str, tint: Color32) {
    egui::Image::new(icon_source(name))
        .tint(tint)
        .paint_at(ui, Rect::from_center_size(center, vec2(size, size)));
}

/// Looks up an embedded SVG by name. Unknown names fall back to the generic file icon.
/// DEPRECATED: FileFly will now always prefer OS provided icons over its default fallbacks listed below.
pub fn icon_source(name: &str) -> egui::ImageSource<'static> {
    match name {
        "app" => egui::include_image!("../assets/app.svg"),
        "archive" => egui::include_image!("../assets/archive.svg"),
        "audio" => egui::include_image!("../assets/audio.svg"),
        "back" => egui::include_image!("../assets/back.svg"),
        "check" => egui::include_image!("../assets/check.svg"),
        "chevron_down" => egui::include_image!("../assets/chevron_down.svg"),
        "chevron_right" => egui::include_image!("../assets/chevron_right.svg"),
        "close" => egui::include_image!("../assets/close.svg"),
        "code" => egui::include_image!("../assets/code.svg"),
        "copy" => egui::include_image!("../assets/copy.svg"),
        "cut" => egui::include_image!("../assets/cut.svg"),
        "delete" => egui::include_image!("../assets/delete.svg"),
        "desktop" => egui::include_image!("../assets/desktop.svg"),
        "document" => egui::include_image!("../assets/document.svg"),
        "documents" => egui::include_image!("../assets/documents.svg"),
        "downloads" => egui::include_image!("../assets/downloads.svg"),
        "drive" => egui::include_image!("../assets/drive.svg"),
        "external" => egui::include_image!("../assets/external.svg"),
        "eye" => egui::include_image!("../assets/eye.svg"),
        "file" => egui::include_image!("../assets/file.svg"),
        "folder" => egui::include_image!("../assets/folder.svg"),
        "forward" => egui::include_image!("../assets/forward.svg"),
        "home" => egui::include_image!("../assets/home.svg"),
        "image" => egui::include_image!("../assets/image.svg"),
        "info" => egui::include_image!("../assets/info.svg"),
        "link" => egui::include_image!("../assets/link.svg"),
        "music" => egui::include_image!("../assets/music.svg"),
        "new_folder" => egui::include_image!("../assets/new_folder.svg"),
        "new_tab" => egui::include_image!("../assets/new_tab.svg"),
        "paste" => egui::include_image!("../assets/paste.svg"),
        "pictures" => egui::include_image!("../assets/pictures.svg"),
        "plus" => egui::include_image!("../assets/plus.svg"),
        "recycle_bin" => egui::include_image!("../assets/recycle_bin.svg"),
        "refresh" => egui::include_image!("../assets/refresh.svg"),
        "rename" => egui::include_image!("../assets/rename.svg"),
        "search" => egui::include_image!("../assets/search.svg"),
        "settings" => egui::include_image!("../assets/settings.svg"),
        "sort_ascending" => egui::include_image!("../assets/sort_ascending.svg"),
        "sort_descending" => egui::include_image!("../assets/sort_descending.svg"),
        "up" => egui::include_image!("../assets/up.svg"),
        "video" => egui::include_image!("../assets/video.svg"),
        "videos" => egui::include_image!("../assets/videos.svg"),
        "restore" => egui::include_image!("../assets/restore.svg"),
        "empty_bin" => egui::include_image!("../assets/empty_bin.svg"),
        _ => egui::include_image!("../assets/file.svg"),
    }
}

/// Picks a file-type icon from a lower-case extension.
pub fn icon_for_extension(ext: &str) -> &'static str {
    match ext {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg" | "ico" | "tif" | "tiff" | "heic" | "avif" => "image",
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" | "opus" | "wma" => "audio",
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "wmv" | "flv" | "m4v" => "video",
        "zip" | "7z" | "rar" | "tar" | "gz" | "bz2" | "xz" | "zst" | "iso" => "archive",
        "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "c" | "h" | "cpp" | "hpp" | "cs" | "java" | "go" | "html" | "css"
        | "json" | "toml" | "yaml" | "yml" | "xml" | "sh" | "ps1" | "bat" | "cmd" | "sql" | "lua" | "kt" | "php" => "code",
        "txt" | "md" | "pdf" | "doc" | "docx" | "odt" | "rtf" | "xls" | "xlsx" | "ods" | "csv" | "ppt" | "pptx" | "log" => "document",
        "exe" | "msi" | "dll" | "appx" | "lnk" | "com" | "scr" => "app",
        _ => "file",
    }
}
