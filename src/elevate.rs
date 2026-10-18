//! "Do as administrator" for individual file actions, opening protected folders and running
//! programs elevated.
//!
//! A running process can't raise its own rights, so elevation always starts another process through
//! the shell's `runas` verb (which shows the UAC prompt):
//!   * file actions   -> `FileFly.exe --admin-op <hex json> "<result file>"`, a headless helper that
//!                       performs exactly one `AdminOp` and exits (see `run_helper`);
//!   * protected folders -> a new, elevated FileFly window (`--open <folder>`);
//!   * programs       -> `runas` on the program itself.
//! The operation travels on the helper's command line (not through a file another process could
//! swap before it runs).

use crate::prelude::*;

/// One file action the elevated helper can perform.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) enum AdminOp {
    Rename { from: PathBuf, to: PathBuf },
    /// Move to the Recycle Bin.
    Delete { paths: Vec<PathBuf> },
    NewFolder { path: PathBuf },
    /// `(source, destination, replace an existing destination)`
    Paste { items: Vec<(PathBuf, PathBuf, bool)>, cut: bool },
}

/// What the "Try as administrator" button of an error does.
#[derive(Clone)]
pub(crate) enum AdminRetry {
    Op(AdminOp),
    /// Open the folder in a new, elevated window.
    OpenFolder(PathBuf),
    /// Run the program elevated.
    RunFile(PathBuf),
}

pub(crate) fn is_denied_io(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(5)
}

/// Whether an error message (already formatted) says "access denied".
pub(crate) fn is_denied_text(s: &str) -> bool {
    let l = s.to_lowercase();
    l.contains("os error 5)")
        || l.contains("os error 5:")
        || l.contains("os error: 5")
        || l.contains("access is denied")
        || l.contains("permission denied")
        || l.contains("toegang geweigerd")
}

/// A program that Windows refused to start because it needs elevation (error 740).
pub(crate) fn needs_elevation(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(740)
}

// ---------------------------------------------------------------- helper process

fn execute(op: AdminOp) -> Result<(), String> {
    match op {
        AdminOp::Rename { from, to } => {
            fs::rename(&from, &to).map_err(|e| format!("Could not rename item: {e}"))
        }
        AdminOp::Delete { paths } => trash::delete_all(&paths)
            .map_err(|e| format!("Could not move item to the Recycle Bin: {e}")),
        AdminOp::NewFolder { path } => {
            fs::create_dir(&path).map_err(|e| format!("Could not create folder: {e}"))
        }
        AdminOp::Paste { items, cut } => {
            let prog = OpProgress::default();
            for (src, dst, replace) in items {
                match paste_one(&PasteItem { src, dst, replace }, cut, &prog) {
                    Pasted::Done | Pasted::Stopped => {}
                    Pasted::Failed(e) => return Err(e),
                }
            }
            Ok(())
        }
    }
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || !s.is_ascii() {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

/// Entry point of the elevated helper process: `args` are what follows `--admin-op`.
/// Returns the process exit code (0 = done). The outcome text goes to the result file.
pub(crate) fn run_helper(args: &[std::ffi::OsString]) -> i32 {
    let result_file = args.get(1).map(PathBuf::from);
    let outcome = (|| -> Result<(), String> {
        let hex = args.first().and_then(|a| a.to_str()).ok_or("Missing operation.")?;
        let bytes = from_hex(hex).ok_or("Corrupt operation.")?;
        let op: AdminOp = serde_json::from_slice(&bytes).map_err(|e| format!("Corrupt operation: {e}"))?;
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| execute(op)))
            .unwrap_or_else(|_| Err("The operation failed unexpectedly.".to_owned()))
    })();
    let (code, text) = match &outcome {
        Ok(()) => (0, "OK".to_owned()),
        Err(e) => (1, format!("ERR:{e}")),
    };
    if let Some(f) = result_file {
        let _ = fs::write(f, text);
    }
    code
}

// ---------------------------------------------------------------- launching (parent side)

/// Runs `op` elevated and waits for it (call from a worker thread: the UAC prompt blocks).
pub(crate) fn run_elevated(op: &AdminOp) -> Result<String, String> {
    let json = serde_json::to_string(op)
        .map_err(|_| "These items can't be handled as administrator (unsupported file names).".to_owned())?;
    let hex: String = json.bytes().map(|b| format!("{b:02x}")).collect();
    if hex.len() > 30_000 {
        return Err("Too many items for one administrator operation. Select fewer items.".to_owned());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let result = std::env::temp_dir().join(format!("filefly-admin-{}-{}.txt", std::process::id(), now_ms()));
    let params = format!("--admin-op {hex} \"{}\"", result.display());
    let code = imp::shell_runas(&exe, &params, None, true)?;
    let text = fs::read_to_string(&result).unwrap_or_default();
    let _ = fs::remove_file(&result);
    match code {
        Some(0) => Ok(String::new()),
        _ => Err(text
            .strip_prefix("ERR:")
            .map(str::to_owned)
            .unwrap_or_else(|| "The administrator operation did not complete.".to_owned())),
    }
}

/// Whether this process runs with administrator rights (cached; it can't change while running).
pub(crate) fn is_elevated() -> bool {
    static ELEVATED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ELEVATED.get_or_init(imp::is_elevated)
}

/// Opens `dir` in a new FileFly window that runs as administrator.
pub(crate) fn open_folder_elevated(dir: &Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut d = dir.display().to_string();
    if d.ends_with('\\') {
        d.push('\\'); // a trailing backslash would escape the closing quote
    }
    imp::shell_runas(&exe, &format!("--open \"{d}\""), None, false).map(|_| ())
}

/// Runs a program (or script, installer) as administrator.
pub(crate) fn run_file_elevated(file: &Path) -> Result<(), String> {
    imp::shell_runas(file, "", file.parent(), false).map(|_| ())
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    #[repr(C)]
    struct ShellExecuteInfo {
        cb_size: u32,
        mask: u32,
        hwnd: isize,
        verb: *const u16,
        file: *const u16,
        params: *const u16,
        dir: *const u16,
        show: i32,
        inst_app: isize,
        id_list: *mut c_void,
        class: *const u16,
        hkey_class: isize,
        hot_key: u32,
        icon_or_monitor: isize,
        process: isize,
    }
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteExW(info: *mut ShellExecuteInfo) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn WaitForSingleObject(h: isize, ms: u32) -> u32;
        fn GetExitCodeProcess(h: isize, code: *mut u32) -> i32;
        fn CloseHandle(h: isize) -> i32;
        fn GetLastError() -> u32;
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn OpenProcessToken(process: isize, access: u32, token: *mut isize) -> i32;
        fn GetTokenInformation(token: isize, class: i32, info: *mut c_void, len: u32, ret: *mut u32) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> isize;
    }

    /// True when the process token is elevated (UAC "run as administrator").
    pub fn is_elevated() -> bool {
        const TOKEN_QUERY: u32 = 0x0008;
        const TOKEN_ELEVATION: i32 = 20;
        let mut token = 0isize;
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return false;
        }
        let mut elevation = 0u32;
        let mut ret = 0u32;
        let ok = unsafe {
            GetTokenInformation(token, TOKEN_ELEVATION, &mut elevation as *mut u32 as *mut c_void, 4, &mut ret)
        };
        unsafe { CloseHandle(token) };
        ok != 0 && elevation != 0
    }

    fn wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
        s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
    }

    /// `ShellExecuteEx` with the `runas` verb. With `wait`, returns the exit code of the process.
    pub fn shell_runas(file: &Path, params: &str, dir: Option<&Path>, wait: bool) -> Result<Option<u32>, String> {
        const SEE_MASK_NOCLOSEPROCESS: u32 = 0x40;
        const SW_HIDE: i32 = 0;
        const SW_SHOWNORMAL: i32 = 1;
        let verb = wide("runas");
        let file_w = wide(file);
        let params_w = wide(params);
        let dir_w = dir.map(wide);
        let mut info = ShellExecuteInfo {
            cb_size: std::mem::size_of::<ShellExecuteInfo>() as u32,
            mask: if wait { SEE_MASK_NOCLOSEPROCESS } else { 0 },
            hwnd: 0,
            verb: verb.as_ptr(),
            file: file_w.as_ptr(),
            params: if params.is_empty() { std::ptr::null() } else { params_w.as_ptr() },
            dir: dir_w.as_ref().map_or(std::ptr::null(), |d| d.as_ptr()),
            show: if wait { SW_HIDE } else { SW_SHOWNORMAL },
            inst_app: 0,
            id_list: std::ptr::null_mut(),
            class: std::ptr::null(),
            hkey_class: 0,
            hot_key: 0,
            icon_or_monitor: 0,
            process: 0,
        };
        if unsafe { ShellExecuteExW(&mut info) } == 0 {
            return Err(match unsafe { GetLastError() } {
                1223 => "Administrator permission was not granted.".to_owned(),
                c => format!("Could not start as administrator (error {c})."),
            });
        }
        if !wait || info.process == 0 {
            return Ok(None);
        }
        let mut code = 1u32;
        unsafe {
            WaitForSingleObject(info.process, u32::MAX);
            GetExitCodeProcess(info.process, &mut code);
            CloseHandle(info.process);
        }
        Ok(Some(code))
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::Path;
    pub fn is_elevated() -> bool {
        false
    }
    pub fn shell_runas(_: &Path, _: &str, _: Option<&Path>, _: bool) -> Result<Option<u32>, String> {
        Err("Running as administrator is only available on Windows.".to_owned())
    }
}
