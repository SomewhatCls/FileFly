use eframe::egui::{self, vec2, Color32, Pos2, Rect, Ui};
use file_icon_provider::get_file_icon;
use std::{collections::HashMap, path::Path};

use crate::Entry;

use std::cell::RefCell;

thread_local! {
    static ICON_CACHE: RefCell<IconCache> = RefCell::new(IconCache::default());
}

const ICON_SIZE: u16 = 32;

/// Extensions whose icon depends on the individual file, not just the type.
#[cfg(windows)]
const PER_FILE_EXTS: &[&str] = &["exe", "lnk", "ico", "url", "msi", "scr", "cur"];
#[cfg(not(windows))]
const PER_FILE_EXTS: &[&str] = &["desktop", "appimage", "ico"];

/// Cache key for a platform icon: one per extension, or one per path for per-file icons like .exe on Windows.
pub fn make_icon_key(is_dir: bool, ext: Option<&str>, path: &Path) -> String {
    if is_dir {
        return "<dir>".into();
    }
    match ext {
        Some(e) if PER_FILE_EXTS.contains(&e) => path.to_string_lossy().into_owned(),
        Some(e) => format!(".{e}"),
        None => "<file>".into(),
    }
}

///Lookup Table for streaming of certain images for certain entries.
#[derive(Default)]
pub struct IconCache {
    map: HashMap<String, Option<egui::TextureHandle>>,
}

impl IconCache {
    ///gets and caches icons for files shown.
    /// uses get_file_icon() from file_icon_provider crate.
    /// If an icon is found, a key is created to associate the entry with a specific texture
    /// in the lookup table.
    /// None means the platform couldn't supply an icon; the caller draws the built-in one.
    pub fn get(&mut self, ctx: &egui::Context, key: &str, path: &Path) -> Option<&egui::TextureHandle> {
        if !self.map.contains_key(key) {
            let tex = (|| {
                let icon = get_file_icon(path, ICON_SIZE).ok()?;
                if icon.width == 0 || icon.height == 0 {
                    return None;
                }
                let img = egui::ColorImage::from_rgba_unmultiplied(
                    [icon.width as usize, icon.height as usize],
                    &icon.pixels,
                );
                Some(ctx.load_texture(format!("icon:{key}"), img, egui::TextureOptions::LINEAR))
            })();
            self.map.insert(key.to_owned(), tex);
        }
        self.map.get(key).and_then(|t| t.as_ref())
    }
}

/// Built-in SVG icon (moved here from main.rs, so delete it there).
pub fn draw_icon(ui: &Ui, center: Pos2, size: f32, name: &str, tint: Color32) {
    egui::Image::new(icon_source(name))
        .tint(tint)
        .paint_at(ui, Rect::from_center_size(center, vec2(size, size)));
}

/// Platform icon if available, otherwise the built-in one.
pub fn draw_system_icon(ui: &Ui, center: Pos2, size: f32, e: &Entry, tint: Color32) {
    let ctx = ui.ctx().clone();
    let tex = ICON_CACHE.with(|c| c.borrow_mut().get(&ctx, &e.icon_key, &e.path).cloned());
    match tex {
        Some(tex) => {
            let sized = egui::load::SizedTexture::new(tex.id(), vec2(size, size));
            egui::Image::new(sized)
                .tint(Color32::from_white_alpha(tint.a()))
                .paint_at(ui, Rect::from_center_size(center, vec2(size, size)));
        }
        None => draw_icon(ui, center, size, &e.icon_key, tint),
    }
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
