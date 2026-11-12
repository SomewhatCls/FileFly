//! Native Recycle Bin access (Windows + freedesktop Linux) through the `trash` crate.
//! On other platforms the stubs report "not supported" and the app falls back to the system file manager.

use crate::fsops::{format_date, format_size, DateFormat};
use crate::icons::icon_for_extension;
use std::{path::PathBuf, time::SystemTime};

#[cfg(any(windows, all(unix, not(target_os = "macos"))))]
mod imp {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    pub type Item = trash::TrashItem;
    pub const SUPPORTED: bool = true;

    pub fn load(fmt: DateFormat) -> Result<Vec<TrashRow>, String> {
        let items = trash::os_limited::list().map_err(|e| e.to_string())?;
        let mut out = Vec::with_capacity(items.len());
        let mut shown = 0;
        for item in items {
            // A file has a size; for a folder the system only reports how many entries it holds.
            // Its size is summed later by the indexer, from where the folder really is on disk.
            let (mut size, entries, mut is_dir) = match trash::os_limited::metadata(&item) {
                Ok(md) => match md.size {
                    trash::TrashItemSize::Bytes(b) => (Some(b), None, false),
                    trash::TrashItemSize::Entries(n) => (None, Some(n as u64), true),
                },
                Err(_) => (None, None, false),
            };
            let mut dir_path = None;
            if is_dir {
                if let Some(p) = real_path(&item) {
                    match std::fs::metadata(&p) {
                        // Check for folder-likes which work like files, e.g. .zip.
                        Ok(md) if md.is_file() => {
                            size = Some(md.len());
                            is_dir = false;
                        }
                        // A genuine folder: the indexer sums it later
                        Ok(md) if md.is_dir() => dir_path = Some(p),
                        _ => {}
                    }
                }
            }
            if cfg!(debug_assertions) && is_dir && shown < 3 {
                eprintln!("trash folder: id {:?} -> {:?}", item.id, dir_path);
                shown += 1;
            }
            let original = item.original_path();
            let name = original.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let location = original.parent().map(|p| p.display().to_string()).unwrap_or_default();
            let deleted = (item.time_deleted >= 0).then(|| UNIX_EPOCH + Duration::from_secs(item.time_deleted as u64));
            out.push(make_row(item, name, location, deleted, size, entries, dir_path, is_dir, fmt));
        }
        Ok(out)
    }


    /// Where the trashed item really is on disk (file or folder). None if the id doesn't lead there.
    fn real_path(item: &Item) -> Option<PathBuf> {
        #[cfg(windows)]
        let p = PathBuf::from(&item.id);
        #[cfg(not(windows))]
        let p = {
            let info = std::path::Path::new(&item.id);
            let name = info.file_name()?.to_str()?.strip_suffix(".trashinfo")?;
            info.parent()?.parent()?.join("files").join(name)
        };
        p.is_absolute().then_some(p)
    }

    pub fn restore(items: Vec<Item>) -> Result<(), String> {
        trash::os_limited::restore_all(items).map_err(|e| e.to_string())
    }

    pub fn purge(items: Vec<Item>) -> Result<(), String> {
        trash::os_limited::purge_all(items).map_err(|e| e.to_string())
    }
}

#[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
mod imp {
    use super::*;

    #[derive(Clone)]
    pub struct Item;
    pub const SUPPORTED: bool = false;

    pub fn load(_fmt: DateFormat) -> Result<Vec<TrashRow>, String> {
        Err("The Recycle Bin view is not supported on this platform.".into())
    }
    pub fn restore(_items: Vec<Item>) -> Result<(), String> {
        Err("Not supported on this platform.".into())
    }
    pub fn purge(_items: Vec<Item>) -> Result<(), String> {
        Err("Not supported on this platform.".into())
    }
}

pub use imp::*;

pub struct TrashRow {
    pub item: Item,
    pub name: String,
    pub name_lc: String,
    pub location: String,
    pub location_lc: String,
    pub is_dir: bool,
    pub deleted: Option<SystemTime>,
    pub date_text: String,
    /// Bytes: a file's size, or a folder's once the indexer has summed it.
    pub size: Option<u64>,
    pub size_text: String,
    pub icon: &'static str,
    /// Where a trashed folder really is on disk (see `real_dir`).
    pub dir_path: Option<PathBuf>,
    /// The folder's size is still being summed, or is an old value that is being refreshed.
    pub size_pending: bool,
}

impl TrashRow {
    /// Sets a folder's size from the indexer. `pending` = a scan is still running.
    pub fn set_dir_size(&mut self, bytes: u64, pending: bool) {
        self.size = Some(bytes);
        self.size_text = format_size(bytes);
        self.size_pending = pending;
    }
}

#[allow(dead_code)]
fn make_row(
    item: Item,
    name: String,
    location: String,
    deleted: Option<SystemTime>,
    size: Option<u64>,
    entries: Option<u64>,
    dir_path: Option<PathBuf>,
    is_dir: bool,
    fmt: DateFormat,
) -> TrashRow {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    TrashRow {
        item,
        name_lc: name.to_lowercase(),
        location_lc: location.to_lowercase(),
        date_text: format_date(deleted, fmt, false),
        size_text: match (size, entries) {
            (Some(b), _) => format_size(b),
            (None, Some(n)) => format!("{n} item{}", if n == 1 { "" } else { "s" }),
            (None, None) => "—".to_owned(),
        },
        is_dir,
        icon: if is_dir { "folder" } else { icon_for_extension(&ext) },
        name,
        location,
        deleted,
        size,
        dir_path,
        size_pending: false,
    }
}
