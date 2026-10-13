//! File-system helpers and formatting that don't touch the UI.

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DateFormat {
    #[default]
    European, // 31.12.2025 14:05
    Iso,      // 2025-12-31 14:05
    American, // 12/31/2025 2:05 PM
}

impl DateFormat {
    pub fn label(self) -> &'static str {
        match self {
            DateFormat::European => "31.12.2025 14:05",
            DateFormat::Iso => "2025-12-31 14:05",
            DateFormat::American => "12/31/2025 2:05 PM",
        }
    }
    fn pattern(self, seconds: bool) -> &'static str {
        match (self, seconds) {
            (DateFormat::European, false) => "%d.%m.%Y %H:%M",
            (DateFormat::European, true) => "%d.%m.%Y %H:%M:%S",
            (DateFormat::Iso, false) => "%Y-%m-%d %H:%M",
            (DateFormat::Iso, true) => "%Y-%m-%d %H:%M:%S",
            (DateFormat::American, false) => "%m/%d/%Y %-I:%M %p",
            (DateFormat::American, true) => "%m/%d/%Y %-I:%M:%S %p",
        }
    }
}

pub fn format_date(time: Option<SystemTime>, fmt: DateFormat, seconds: bool) -> String {
    match time {
        Some(t) => DateTime::<Local>::from(t).format(fmt.pattern(seconds)).to_string(),
        None => "—".to_owned(),
    }
}

pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{size:.1} {}", UNITS[unit]) }
}

pub fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "report10" sorts after "report2".
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = ai.peek().copied().filter(char::is_ascii_digit) {
                    na.push(c);
                    ai.next();
                }
                let mut nb = String::new();
                while let Some(c) = bi.peek().copied().filter(char::is_ascii_digit) {
                    nb.push(c);
                    bi.next();
                }
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let o = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
                ai.next();
                bi.next();
            }
        }
    }
}

#[cfg(windows)]
pub fn attributes(md: &fs::Metadata) -> u32 {
    use std::os::windows::fs::MetadataExt;
    md.file_attributes()
}
#[cfg(not(windows))]
pub fn attributes(_md: &fs::Metadata) -> u32 {
    0
}

/// Uses the attributes already delivered by `read_dir` (no extra syscall per entry, and correct for
/// junctions such as "Application Data", which `fs::metadata` can't follow).
pub fn is_hidden_entry(name: &str, md: &fs::Metadata) -> bool {
    attributes(md) & (0x2 | 0x4) != 0 || name.starts_with('.')
}

pub fn remove_existing(path: &Path) -> std::io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) }
}

pub fn unique_copy_name(destination: &Path) -> PathBuf {
    if !destination.exists() {
        return destination.to_path_buf();
    }
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let stem = destination.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Copy".into());
    let ext = destination.extension().map(|e| e.to_string_lossy().into_owned());
    for n in 1u64.. {
        let name = match &ext {
            Some(e) => format!("{stem} ({n}).{e}"),
            None => format!("{stem} ({n})"),
        };
        let candidate = parent.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

/// A free name next to `path` for something that is only temporary: the leading dot hides it from
/// listings, the pid keeps two running instances apart, the counter avoids clobbering a leftover.
fn free_sibling(path: &Path, tag: &str) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    for n in 0u32.. {
        let candidate = path.with_file_name(format!(".{name}.{tag}-{}-{n}", std::process::id()));
        if fs::symlink_metadata(&candidate).is_err() {
            return candidate;
        }
    }
    unreachable!()
}

/// Where "Replace" parks the existing item until the new one is safely in place. If the app dies
/// in between, the original is still sitting next to the destination under this name.
pub fn backup_name(path: &Path) -> PathBuf {
    free_sibling(path, "replaced")
}

/// Final rename of a finished temp item. Antivirus/indexers sometimes hold a just-written file or
/// folder for a moment, which shows up as "access denied"; a few short retries ride that out.
fn rename_retry(from: &Path, to: &Path) -> io::Result<()> {
    let mut attempt = 0u64;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if attempt < 5 && (e.kind() == io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(32)) => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(60 * attempt));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Recursive copy into `destination` (always a temp folder, so partial results are never visible).
/// The depth limit turns a junction/symlink loop into an error instead of endless recursion.
fn copy_tree(source: &Path, destination: &Path, depth: usize) -> io::Result<()> {
    if depth > 128 {
        return Err(io::Error::other("folders are nested too deeply (possible link loop)"));
    }
    fs::create_dir_all(destination)?;
    for item in fs::read_dir(source)? {
        let item = item?;
        let (from, to) = (item.path(), destination.join(item.file_name()));
        if from.is_dir() {
            copy_tree(&from, &to, depth + 1)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Copies to a temporary name next to the destination and renames it into place only when the
/// whole copy has succeeded, so the real name never points at a half-written file or folder.
/// Never overwrites: an existing destination is an error (callers move it aside first).
pub fn copy_item(source: &Path, destination: &Path) -> io::Result<()> {
    if fs::symlink_metadata(destination).is_ok() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "the destination already exists"));
    }
    let is_dir = source.is_dir();
    if is_dir && destination.starts_with(source) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "cannot copy a folder into itself"));
    }
    let tmp = free_sibling(destination, "part");
    let copied = if is_dir {
        copy_tree(source, &tmp, 0)
    } else {
        fs::copy(source, &tmp).map(|_| ())
    };
    let result = copied.and_then(|_| rename_retry(&tmp, destination));
    if result.is_err() {
        let _ = remove_existing(&tmp); // don't leave the partial copy behind
    }
    result
}

// ---------------------------------------------------------------------------------------------
// System clipboard: Copy/Cut publish the selection as a file list, Paste reads it back, so it
// works with Explorer/Finder/file managers and other apps. Same shape as the app's own
// `clipboard: Option<(Vec<PathBuf>, bool)>` (paths, is_cut).
// ---------------------------------------------------------------------------------------------

/// Windows marks a cut with this format (DROPEFFECT_MOVE = 2); no format means copy.
const WIN_DROP_EFFECT: &str = "Preferred DropEffect";
/// GNOME/Nautilus-style file managers: first line is "cut" or "copy", then one URI per line.
#[cfg(all(unix, not(target_os = "macos")))]
const GNOME_FILES: &str = "x-special/gnome-copied-files";
/// KDE/Dolphin marks a cut with "1" in this format.
#[cfg(all(unix, not(target_os = "macos")))]
const KDE_CUT: &str = "application/x-kde-cutselection";

/// `canonicalize` yields `\\?\C:\...` on Windows, which many apps can't paste; drop the prefix.
fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => p.to_path_buf(),
    }
}

/// Publishes the items on the system clipboard (with the "cut" marker if `cut`). Nothing is copied
/// or moved here; whichever app pastes does that. macOS has no native file cut, so there a cut
/// is published as a plain copy (the source stays in place, nothing can be lost).
pub fn set_clipboard(paths: &[PathBuf], cut: bool) -> io::Result<()> {
    use clipboard_rs::{Clipboard, ClipboardContent, ClipboardContext};
    let files = paths
        .iter()
        .map(|p| fs::canonicalize(p).map(|c| strip_verbatim(&c).to_string_lossy().into_owned()))
        .collect::<io::Result<Vec<String>>>()?;
    // The text copy exists so egui sees a non-empty clipboard and emits Event::Paste on Ctrl+V;
    // file managers ignore it and use the file list.
    let mut content = vec![ClipboardContent::Files(files.clone()), ClipboardContent::Text(files.join("\n"))];
    if cut {
        #[cfg(windows)]
        content.push(ClipboardContent::Other(WIN_DROP_EFFECT.into(), 2u32.to_le_bytes().to_vec()));
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let mut list = String::from("cut");
            for f in &files {
                if let Ok(u) = url::Url::from_file_path(f) {
                    list.push('\n');
                    list.push_str(u.as_str());
                }
            }
            content.push(ClipboardContent::Other(GNOME_FILES.into(), list.into_bytes()));
            content.push(ClipboardContent::Other(KDE_CUT.into(), b"1".to_vec()));
        }
    }
    let ctx = ClipboardContext::new().map_err(|e| io::Error::other(e.to_string()))?;
    ctx.set(content).map_err(|e| io::Error::other(e.to_string()))
}

/// The files on the system clipboard (from this app or any other) and whether they were cut.
/// `None` if the clipboard holds something else, or only items that no longer exist (which also
/// covers a finished cut-and-paste, since the sources are gone by then).
pub fn get_clipboard() -> Option<(Vec<PathBuf>, bool)> {
    use clipboard_rs::{Clipboard, ClipboardContext};
    let ctx = ClipboardContext::new().ok()?;
    let paths: Vec<PathBuf> = ctx
        .get_files()
        .ok()?
        .into_iter()
        .filter_map(|s| {
            if s.starts_with("file://") {
                url::Url::parse(&s).ok()?.to_file_path().ok()
            } else {
                Some(PathBuf::from(s))
            }
        })
        .filter(|p| fs::symlink_metadata(p).is_ok())
        .collect();
    if paths.is_empty() {
        return None;
    }
    #[cfg(windows)]
    let cut = match ctx.get_buffer(WIN_DROP_EFFECT) {
        Ok(b) if b.len() >= 4 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) & 2 != 0,
        _ => false,
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let cut = ctx.get_buffer(GNOME_FILES).map(|b| b.starts_with(b"cut")).unwrap_or(false)
        || ctx.get_buffer(KDE_CUT).map(|b| b.starts_with(b"1")).unwrap_or(false);
    #[cfg(target_os = "macos")]
    let cut = false;
    Some((paths, cut))
}

fn is_cross_device(e: &io::Error) -> bool {
    // ERROR_NOT_SAME_DEVICE on Windows, EXDEV elsewhere
    e.raw_os_error() == Some(if cfg!(windows) { 17 } else { 18 })
}

/// Rename when possible (atomic on one drive); copy + delete only when the rename failed because
/// the destination is on another drive. Any other failure (item in use, access denied, ...) is
/// reported as is, without half-moving anything. Never replaces an existing destination.
pub fn move_item(source: &Path, destination: &Path) -> io::Result<()> {
    if fs::symlink_metadata(destination).is_ok() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "the destination already exists"));
    }
    match fs::rename(source, destination) {
        Ok(()) => Ok(()),
        Err(e) if is_cross_device(&e) => {
            copy_item(source, destination)?; // source stays untouched until this fully succeeded
            remove_existing(source)
        }
        Err(e) => Err(e),
    }
}

pub fn open_recycle_bin() -> std::io::Result<()> {
    #[cfg(windows)]
    {
        return std::process::Command::new("explorer.exe").arg("shell:RecycleBinFolder").spawn().map(|_| ());
    }
    #[cfg(target_os = "macos")]
    {
        let trash = dirs::home_dir()
            .map(|h| h.join(".Trash"))
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "home directory not found"))?;
        return open::that(trash).map_err(std::io::Error::other);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        return open::that("trash:///").map_err(std::io::Error::other);
    }
    #[allow(unreachable_code)]
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "Recycle Bin is not available here"))
}

/// Opens a terminal window in `dir` (Windows Terminal if installed, else the classic console).
pub fn open_terminal(dir: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        if std::process::Command::new("wt.exe").arg("-d").arg(dir).spawn().is_ok() {
            return Ok(());
        }
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        return std::process::Command::new("cmd.exe")
            .current_dir(dir)
            .creation_flags(CREATE_NEW_CONSOLE)
            .spawn()
            .map(|_| ());
    }
    #[cfg(target_os = "macos")]
    {
        return std::process::Command::new("open").args(["-a", "Terminal"]).arg(dir).spawn().map(|_| ());
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        for term in ["x-terminal-emulator", "gnome-terminal", "konsole", "xfce4-terminal", "alacritty", "xterm"] {
            if std::process::Command::new(term).current_dir(dir).spawn().is_ok() {
                return Ok(());
            }
        }
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no terminal emulator found"));
    }
    #[allow(unreachable_code)]
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "not supported here"))
}

pub fn find_peazip() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        for var in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(p) = std::env::var_os(var) {
                let c = PathBuf::from(p).join("PeaZip").join("peazip.exe");
                if c.is_file() {
                    return Some(c);
                }
            }
        }
        None
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("PATH")
            .and_then(|paths| std::env::split_paths(&paths).map(|d| d.join("peazip")).find(|c| c.is_file()))
    }
}

pub fn open_with_peazip(exe: &Path, path: &Path) -> std::io::Result<()> {
    let mut c = std::process::Command::new(exe);
    c.arg(if path.is_dir() { "-ext2browsepath" } else { "-ext2browse" });
    c.arg(path).spawn().map(|_| ())
}

pub fn add_to_peazip(exe: &Path, path: &Path) -> std::io::Result<()> {
    std::process::Command::new(exe).arg("-add2archive").arg(path).spawn().map(|_| ())
}

#[derive(Default, Clone)]
pub struct FolderStats {
    pub files: u64,
    pub folders: u64,
    pub bytes: u64,
}

/// Iterative walk (no recursion depth issues); `cancelled` lets the Properties dialog abort it.
pub fn folder_stats(root: &Path, cancelled: &dyn Fn() -> bool) -> FolderStats {
    let mut stats = FolderStats::default();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if cancelled() {
            break;
        }
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for item in rd.flatten() {
            let Ok(ft) = item.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                stats.folders += 1;
                stack.push(item.path());
            } else {
                stats.files += 1;
                stats.bytes += item.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    stats
}
