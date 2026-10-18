//! Directory-listing rows, sidebar items and sort/search enums.

use crate::prelude::*;

#[derive(Clone)]
pub(crate) struct Entry {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) name_lc: String,
    pub(crate) is_dir: bool,
    pub(crate) is_link: bool,
    pub(crate) size: u64,
    pub(crate) modified: Option<SystemTime>,
    pub(crate) date_text: String,
    pub(crate) type_text: String,
    pub(crate) icon_key: String,
    pub(crate) dir_size: Option<u64>,
}

impl Entry {
    pub(crate) fn sort_size(&self) -> u64 {
        if self.is_dir {
            self.dir_size.unwrap_or(0)
        } else {
            self.size
        }
    }
    pub(crate) fn from_path(name: String, path: PathBuf, md: &fs::Metadata, fmt: DateFormat) -> Self {
        let is_link = md.file_type().is_symlink();
        let target = if is_link { fs::metadata(&path).ok() } else { None };
        let eff = target.as_ref().unwrap_or(md);
        let is_dir = eff.is_dir();
        let modified = eff.modified().ok().or_else(|| md.modified().ok());
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase());

        let type_text = if is_dir {
            "File folder".to_owned()
        } else {
            match &ext {
                Some(e) => format!("{} file", e.to_uppercase()),
                None => "File".to_owned(),
            }
        };

        let icon_key = make_icon_key(is_dir, ext.as_deref(), &path);

        note_name(&name); // may need a fallback font for this script
        Self {
            name_lc: name.to_lowercase(),
            name,
            path,
            is_dir,
            is_link,
            size: if is_dir { 0 } else { eff.len() },
            modified,
            date_text: format_date(modified, fmt, false),
            type_text,
            icon_key,
            dir_size: None,
        }
    }
}

/// One entry of the Quick access list in the sidebar.
#[derive(Clone)]
pub(crate) struct QuickItem {
    pub(crate) label: String,
    pub(crate) path: PathBuf,
    /// Built-in icon shown until (or unless) the platform icon of the folder is available.
    pub(crate) icon: &'static str,
    /// False while the folder sits on a removable / network drive that is not connected: the entry stays
    /// pinned in the settings but is hidden from the sidebar until the drive comes back.
    pub(crate) available: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SortColumn {
    Name,
    Modified,
    Type,
    Size,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SearchScope {
    #[default]
    CurrentDir,
    System,
}

impl SearchScope {
    pub(crate) fn label(self) -> &'static str {
        match self {
            SearchScope::CurrentDir => "Current folder (Ctrl+F)",
            SearchScope::System => "Entire system (Ctrl+G)",
        }
    }
}
