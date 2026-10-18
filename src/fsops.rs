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
    // Built from OsStrings, so a name that is not valid Unicode is kept byte for byte.
    let stem = destination.file_stem().map(|s| s.to_os_string()).unwrap_or_else(|| "Copy".into());
    let ext = destination.extension().map(|e| e.to_os_string());
    for n in 1u64.. {
        let mut name = stem.clone();
        name.push(format!(" ({n})"));
        if let Some(e) = &ext {
            name.push(".");
            name.push(e);
        }
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
    let name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    for n in 0u32.. {
        let mut file = std::ffi::OsString::from(".");
        file.push(&name);
        file.push(format!(".{tag}-{}-{n}", std::process::id()));
        let candidate = path.with_file_name(file);
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
#[cfg(windows)]
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

// ---------------------------------------------------------------------------------------------
// ZIP archives: compress / extract with the tools the operating system ships (no zip library),
// and "open a .zip like a folder" by unpacking it into a private cache that the UI presents
// under the zip's own path (C:\data\photos.zip\2024\...).
//
//   Windows  tar.exe (bsdtar, part of Windows 10+), PowerShell Compress-Archive/Expand-Archive
//   macOS    ditto (the engine behind Finder's "Compress" and double-click extraction)
//   Linux    zip / unzip, then bsdtar, then 7z, whichever is installed
// ---------------------------------------------------------------------------------------------

use std::ffi::{OsStr, OsString};
use std::process::{Command, Stdio};

const OK0: &[i32] = &[0];
/// unzip exits with 1 for "finished, with warnings".
#[cfg(all(unix, not(target_os = "macos")))]
const OK01: &[i32] = &[0, 1];

const NO_ZIP_TOOL: &str = if cfg!(windows) {
    "no ZIP tool found (tar.exe or PowerShell is required)"
} else if cfg!(target_os = "macos") {
    "the system tool \"ditto\" was not found"
} else {
    "no ZIP tool found; install \"zip\" and \"unzip\" (or bsdtar / 7-Zip)"
};

pub fn has_zip_ext(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip"))
}

fn tool(program: impl AsRef<OsStr>) -> Command {
    let mut c = Command::new(program);
    c.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console window flashing up
    }
    c
}

/// Runs a tool to completion. `ErrorKind::NotFound` means the program is not installed (the caller
/// then tries the next candidate); any other error carries the tool's own message.
fn run_tool(mut cmd: Command, ok_codes: &[i32]) -> io::Result<()> {
    let out = cmd.output()?;
    if out.status.success() || out.status.code().is_some_and(|c| ok_codes.contains(&c)) {
        return Ok(());
    }
    let raw = if out.stderr.iter().any(|b| !b.is_ascii_whitespace()) { &out.stderr } else { &out.stdout };
    let text = String::from_utf8_lossy(raw).into_owned();
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).take(3).collect();
    Err(io::Error::other(if lines.is_empty() {
        format!("the ZIP tool failed ({})", out.status)
    } else {
        lines.join(" ")
    }))
}

fn run_first_available(tools: Vec<(Command, &'static [i32])>) -> io::Result<()> {
    for (cmd, ok) in tools {
        match run_tool(cmd, ok) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            done => return done,
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, NO_ZIP_TOOL))
}

/// A name that is safe to hand to a command line tool (a leading "-" would be read as an option).
fn arg_safe(name: &OsStr) -> OsString {
    if name.to_string_lossy().starts_with('-') {
        let mut s = OsString::from("./");
        s.push(name);
        s
    } else {
        name.to_os_string()
    }
}

#[cfg(windows)]
fn system_tar() -> PathBuf {
    // Full path on purpose: a "tar" from Git/MSYS earlier in PATH cannot write zip files.
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32")
        .join("tar.exe")
}

#[cfg(windows)]
fn ps_quote(p: &Path) -> String {
    format!("'{}'", p.to_string_lossy().replace('\'', "''"))
}

#[allow(unused_variables, unused_mut)]
fn compress_tools(parent: &Path, items: &[PathBuf], names: &[OsString], out: &Path) -> Vec<(Command, &'static [i32])> {
    let mut tools: Vec<(Command, &'static [i32])> = Vec::new();
    #[cfg(windows)]
    {
        let mut c = tool(system_tar());
        c.arg("-a").arg("-c").arg("-f").arg(out).arg("-C").arg(parent).args(names);
        tools.push((c, OK0));
        let list = items.iter().map(|p| ps_quote(p)).collect::<Vec<_>>().join(",");
        let mut c = tool("powershell.exe");
        c.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command"]).arg(format!(
            "$ErrorActionPreference='Stop'; Compress-Archive -LiteralPath {list} -DestinationPath {}",
            ps_quote(out)
        ));
        tools.push((c, OK0));
    }
    #[cfg(target_os = "macos")]
    {
        let mut c = tool("ditto");
        c.args(["-c", "-k", "--sequesterRsrc", "--keepParent"]).args(items).arg(out);
        tools.push((c, OK0));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut c = tool("zip");
        c.current_dir(parent).args(["-r", "-q", "-y"]).arg(out).args(names);
        tools.push((c, OK0));
        let mut c = tool("bsdtar");
        c.arg("-a").arg("-c").arg("-f").arg(out).arg("-C").arg(parent).args(names);
        tools.push((c, OK0));
        for sevenzip in ["7z", "7zz"] {
            let mut c = tool(sevenzip);
            c.current_dir(parent).args(["a", "-tzip", "-bd", "-y"]).arg(out).args(names);
            tools.push((c, OK0));
        }
    }
    tools
}

#[allow(unused_variables, unused_mut)]
fn extract_tools(zip: &Path, dest: &Path) -> Vec<(Command, &'static [i32])> {
    let mut tools: Vec<(Command, &'static [i32])> = Vec::new();
    #[cfg(windows)]
    {
        let mut c = tool(system_tar());
        c.arg("-x").arg("-f").arg(zip).arg("-C").arg(dest);
        tools.push((c, OK0));
        let mut c = tool("powershell.exe");
        c.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command"]).arg(format!(
            "$ErrorActionPreference='Stop'; Expand-Archive -LiteralPath {} -DestinationPath {}",
            ps_quote(zip),
            ps_quote(dest)
        ));
        tools.push((c, OK0));
    }
    #[cfg(target_os = "macos")]
    {
        let mut c = tool("ditto");
        c.arg("-x").arg("-k").arg(zip).arg(dest);
        tools.push((c, OK0));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut c = tool("unzip");
        c.arg("-q").arg(zip).arg("-d").arg(dest);
        tools.push((c, OK01));
        let mut c = tool("bsdtar");
        c.arg("-x").arg("-f").arg(zip).arg("-C").arg(dest);
        tools.push((c, OK0));
        for sevenzip in ["7z", "7zz"] {
            let mut o = OsString::from("-o");
            o.push(dest);
            let mut c = tool(sevenzip);
            c.args(["x", "-y", "-bd"]).arg(o).arg(zip);
            tools.push((c, OK0));
        }
    }
    tools
}

/// Hidden temporary name next to `dest` that still ends in ".zip" (the tools pick the format and
/// refuse to write from the extension).
fn zip_temp(dest: &Path) -> PathBuf {
    let mut s = free_sibling(dest, "part").into_os_string();
    s.push(".zip");
    PathBuf::from(s)
}

/// Zips `items` (all in one folder) into the new file `dest`. The archive is built under a temporary
/// name and renamed into place when finished, so `dest` never holds a half-written zip.
/// Never overwrites: an existing `dest` is an error.
pub fn compress_items(items: &[PathBuf], dest: &Path) -> io::Result<()> {
    let invalid = |m: &str| io::Error::new(io::ErrorKind::InvalidInput, m.to_owned());
    let parent = items.first().and_then(|p| p.parent()).ok_or_else(|| invalid("nothing to compress"))?;
    let mut names = Vec::with_capacity(items.len());
    for p in items {
        if p.parent() != Some(parent) {
            return Err(invalid("the items are not all in the same folder"));
        }
        names.push(arg_safe(p.file_name().ok_or_else(|| invalid("an item has no name"))?));
    }
    if fs::symlink_metadata(dest).is_ok() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "the destination already exists"));
    }
    let tmp = zip_temp(dest);
    let result = run_first_available(compress_tools(parent, items, &names, &tmp)).and_then(|_| {
        if tmp.is_file() {
            rename_retry(&tmp, dest)
        } else {
            Err(io::Error::other("the ZIP tool did not create an archive"))
        }
    });
    if result.is_err() {
        let _ = remove_existing(&tmp); // don't leave the partial archive behind
    }
    result
}

/// Unpacks `zip` into the new folder `dest` (same temp-then-rename safety as `compress_items`).
/// Never merges into or overwrites an existing `dest`.
pub fn extract_zip(zip: &Path, dest: &Path) -> io::Result<()> {
    if fs::symlink_metadata(dest).is_ok() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "the destination already exists"));
    }
    let tmp = free_sibling(dest, "part");
    fs::create_dir(&tmp)?;
    let result = run_first_available(extract_tools(zip, &tmp)).and_then(|_| rename_retry(&tmp, dest));
    if result.is_err() {
        let _ = remove_existing(&tmp);
    }
    result
}

// ---------------------------------------------------------------------------------------------
// Browsing a zip like a folder. The archive is unpacked once into a per-process cache folder; tabs
// navigate in that real folder, while everything the user sees (address bar, breadcrumbs, tab
// title, "Up") is translated back to "<zip path>\inner\folder" by `display_path`.
// ---------------------------------------------------------------------------------------------

struct Mount {
    /// The zip file (canonical path; may itself live inside another mount's cache folder).
    zip: PathBuf,
    /// Where it was unpacked.
    root: PathBuf,
    /// Size and modification time of the zip when it was unpacked; a changed zip is unpacked anew.
    len: u64,
    modified: Option<SystemTime>,
}

static MOUNTS: std::sync::Mutex<Vec<Mount>> = std::sync::Mutex::new(Vec::new());
static MOUNT_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
const CACHE_PREFIX: &str = "FileFly-archives-";

fn mounts() -> std::sync::MutexGuard<'static, Vec<Mount>> {
    MOUNTS.lock().unwrap_or_else(|e| e.into_inner())
}

fn cache_root() -> PathBuf {
    std::env::temp_dir().join(format!("{CACHE_PREFIX}{}", std::process::id()))
}

fn join_rel(base: &Path, rel: &Path) -> PathBuf {
    if rel.as_os_str().is_empty() { base.to_path_buf() } else { base.join(rel) }
}

/// The unpacked folder of `zip` if it was unpacked before and the zip has not changed since.
fn fresh_mount_root(zip: &Path) -> Option<PathBuf> {
    let md = fs::metadata(zip).ok()?;
    let modified = md.modified().ok();
    let ms = mounts();
    let found = ms
        .iter()
        .rev()
        .find(|m| m.zip == zip && m.len == md.len() && m.modified == modified && m.root.is_dir())
        .map(|m| m.root.clone());
    found
}

pub enum ArchivePath {
    /// A normal path (possibly translated into an already unpacked archive).
    Real(PathBuf),
    /// The path points into (or at) a zip that still has to be unpacked with `mount_archive`;
    /// `rel` is the part inside the zip.
    Extract { zip: PathBuf, rel: PathBuf },
}

/// The first zip *file* on the way down `path` and the remainder of the path after it.
fn find_zip_in(path: &Path) -> Option<(PathBuf, PathBuf)> {
    let comps: Vec<_> = path.components().collect();
    let mut prefix = PathBuf::new();
    for (i, c) in comps.iter().enumerate() {
        prefix.push(c.as_os_str());
        if has_zip_ext(&prefix) && prefix.is_file() {
            let rel: PathBuf = comps[i + 1..].iter().collect();
            return Some((prefix, rel));
        }
    }
    None
}

/// Turns a path that may run through zip files ("C:\a\b.zip\docs") into a real one. Archives that
/// are not unpacked yet are reported instead of being unpacked here (unpacking takes a while).
pub fn resolve_archive_path(path: &Path) -> ArchivePath {
    let mut path = path.to_path_buf();
    for _ in 0..8 {
        // (zips inside zips: after one translation the new path may run through another zip)
        let Some((prefix, rel)) = find_zip_in(&path) else {
            return ArchivePath::Real(path);
        };
        let zip = dunce::canonicalize(&prefix).unwrap_or(prefix);
        match fresh_mount_root(&zip) {
            Some(root) => path = join_rel(&root, &rel),
            None => return ArchivePath::Extract { zip, rel },
        }
    }
    ArchivePath::Real(path)
}

/// Unpacks `zip` (or reuses the unpacking from earlier) and returns the folder it was unpacked to.
/// Blocking: call it from a worker thread.
pub fn mount_archive(zip: &Path) -> io::Result<PathBuf> {
    if let Some(root) = fresh_mount_root(zip) {
        return Ok(root);
    }
    let md = fs::metadata(zip)?;
    let base = cache_root().join(MOUNT_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed).to_string());
    fs::create_dir_all(&base)?;
    // Canonical, so it compares equal to what the folder tabs get from `canonicalize` (8.3 names on
    // Windows, /var -> /private/var on macOS).
    let base = dunce::canonicalize(&base)?;
    let stem = zip.file_stem().map(|s| s.to_os_string()).unwrap_or_else(|| "archive".into());
    let root = base.join(stem);
    if let Err(e) = extract_zip(zip, &root) {
        let _ = fs::remove_dir_all(&base);
        return Err(e);
    }
    mounts().push(Mount { zip: zip.to_path_buf(), root: root.clone(), len: md.len(), modified: md.modified().ok() });
    Ok(root)
}

/// The path the user should see for `real`: inside an unpacked archive that is
/// "<zip path>\<inner path>", anywhere else the path itself.
pub fn display_path(real: &Path) -> PathBuf {
    let ms = mounts();
    if ms.is_empty() {
        return real.to_path_buf();
    }
    let mut p = real.to_path_buf();
    for _ in 0..8 {
        let Some(m) = ms.iter().filter(|m| p.starts_with(&m.root)).max_by_key(|m| m.root.as_os_str().len()) else {
            break;
        };
        let rel = p.strip_prefix(&m.root).map(Path::to_path_buf).unwrap_or_default();
        p = join_rel(&m.zip, &rel);
    }
    p
}

/// The zip file whose unpacked contents `path` is part of, if any.
pub fn archive_of(path: &Path) -> Option<PathBuf> {
    let ms = mounts();
    let found = ms
        .iter()
        .filter(|m| path.starts_with(&m.root))
        .max_by_key(|m| m.root.as_os_str().len())
        .map(|m| m.zip.clone());
    found
}

/// A real folder next to the outermost zip that `real` is inside of (`real` itself when it is not
/// inside an archive). Used for what has to be written to disk: sessions, "Extract all".
pub fn folder_outside(real: &Path) -> PathBuf {
    if archive_of(real).is_none() {
        return real.to_path_buf();
    }
    let mut p = display_path(real);
    while !p.is_dir() {
        match p.parent() {
            Some(q) => p = q.to_path_buf(),
            None => break,
        }
    }
    p
}

/// Deletes everything unpacked by this process (called on exit).
pub fn remove_archive_cache() {
    let _ = fs::remove_dir_all(cache_root());
}

/// Removes caches that a crashed or killed instance left in the temp folder. Only quite old ones:
/// another FileFly window that is still running may be using a younger one.
pub fn sweep_old_archive_caches() {
    let Ok(rd) = fs::read_dir(std::env::temp_dir()) else { return };
    let ours = cache_root();
    for item in rd.flatten() {
        if !item.file_name().to_string_lossy().starts_with(CACHE_PREFIX) || item.path() == ours {
            continue;
        }
        let old = item
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > Duration::from_secs(7 * 24 * 3600));
        if old {
            let _ = fs::remove_dir_all(item.path());
        }
    }
}
