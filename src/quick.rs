//! Quick-access folders and path helpers.

use crate::prelude::*;

/// Quick access folders that exist on every system: (built-in icon, label, folder).
pub(crate) fn known_folders() -> Vec<(&'static str, &'static str, Option<PathBuf>)> {
    vec![
        ("home", "Home", dirs::home_dir()),
        ("desktop", "Desktop", dirs::desktop_dir()),
        ("downloads", "Downloads", dirs::download_dir()),
        ("documents", "Documents", dirs::document_dir()),
        ("pictures", "Pictures", dirs::picture_dir()),
        ("music", "Music", dirs::audio_dir()),
        ("videos", "Videos", dirs::video_dir()),
    ]
}

pub(crate) fn folder_label(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// The Quick access list: the user's pinned folders, or the defaults if never customised.
pub(crate) fn build_quick(s: &Settings) -> Vec<QuickItem> {
    let known = known_folders();
    let Some(list) = &s.quick_access else {
        return known
            .into_iter()
            .filter_map(|(icon, label, p)| {
                p.filter(|p| p.is_dir())
                    .map(|path| QuickItem { label: label.to_owned(), path, icon, available: true })
            })
            .collect();
    };
    list.iter()
        .map(PathBuf::from)
        .map(|path| match known.iter().find(|(_, _, p)| p.as_deref() == Some(path.as_path())) {
            Some((icon, label, _)) => QuickItem { label: (*label).to_owned(), path, icon: *icon, available: true },
            None => QuickItem { label: folder_label(&path), path, icon: "folder", available: true },
        })
        .collect()
}

pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy())
    } else {
        a == b
    }
}

pub(crate) fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find('%') {
            Some(j) if j > 0 => match std::env::var(&after[..j]) {
                Ok(v) => {
                    out.push_str(&v);
                    rest = &after[j + 1..];
                }
                Err(_) => {
                    out.push('%');
                    rest = after;
                }
            },
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}
