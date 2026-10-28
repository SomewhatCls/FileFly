//! Process start-up: command line, window options and `eframe::run_native`.

use crate::prelude::*;

/// True if the physical-pixel rectangle touches any connected monitor.
#[cfg(windows)]
pub(crate) fn rect_touches_a_monitor(left: i32, top: i32, right: i32, bottom: i32) -> bool {
    #[repr(C)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn MonitorFromRect(rc: *const Rect, flags: u32) -> isize;
    }
    const MONITOR_DEFAULTTONULL: u32 = 0;
    let rc = Rect { left, top, right, bottom };
    unsafe { MonitorFromRect(&rc, MONITOR_DEFAULTTONULL) != 0 }
}

/// Other platforms: no monitor query is available, so assume the window is visible.
#[cfg(not(windows))]
pub(crate) fn rect_touches_a_monitor(_: i32, _: i32, _: i32, _: i32) -> bool {
    true
}

/// Rejects positions that are obviously not a real on-screen location (e.g. the -32000
/// the OS reports for a minimised window) before they are saved or restored.
pub(crate) fn window_pos_is_sane(p: [f32; 2]) -> bool {
    p.iter().all(|v| v.is_finite() && *v > -10_000.0 && *v < 20_000.0)
}

/// Folder a window opened by "drag a tab out" starts in.
pub(crate) static START_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Starts another FileFly window (its own process) showing `dir`.
pub(crate) fn spawn_window(dir: &Path, pos: Option<(f32, f32)>) {
    if let Ok(exe) = std::env::current_exe() {
        let mut c = std::process::Command::new(exe);
        c.arg("--open").arg(dir);
        if let Some((x, y)) = pos {
            c.arg("--pos").arg(format!("{x:.0},{y:.0}"));
        }
        let _ = c.spawn();
    }
}

/// Path argument from the command line. Windows passes a drive root as `"D:\"`, which the argument
/// parser reads as an escaped quote and hands over as `D:"`; this restores the real path.
pub(crate) fn clean_path_arg(a: &std::ffi::OsStr) -> PathBuf {
    match a.to_str() {
        Some(s) if s.ends_with('"') => {
            let t = s.trim_end_matches('"');
            if t.len() == 2 && t.ends_with(':') {
                PathBuf::from(format!("{t}\\"))
            } else {
                PathBuf::from(t)
            }
        }
        _ => PathBuf::from(a),
    }
}

pub(crate) fn run() -> eframe::Result<()> {
    // The elevated helper (see elevate.rs) performs one file action and exits; it never shows a window.
    {
        let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
        if args.first().and_then(|a| a.to_str()) == Some("--admin-op") {
            std::process::exit(run_helper(&args[1..]));
        }
    }
    // The release build has no console, so a panic would otherwise vanish without a trace.
    std::panic::set_hook(Box::new(|info| {
        use std::io::Write;
        let dir = app_dir();
        let _ = fs::create_dir_all(&dir);
        if let Ok(mut f) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("crash.log"))
        {
            let _ = writeln!(
                f,
                "---- panic ----\n{info}\n{}\n",
                std::backtrace::Backtrace::force_capture()
            );
        }
    }));

    // Command line (used when a tab is dragged out into a window of its own).
    // `args_os`: `args()` panics on a non-Unicode folder name, and `--open` is a path.
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let mut pos: Option<[f32; 2]> = None;
    let mut k = 0;
    while k < args.len() {
        match args[k].to_str().unwrap_or_default() {
            "--open" if k + 1 < args.len() => {
                let _ = START_DIR.set(clean_path_arg(&args[k + 1]));
                SESSION_READONLY.store(true, AtomicOrdering::Relaxed);
                k += 1;
            }
            "--pos" if k + 1 < args.len() => {
                if let Some((x, y)) = args[k + 1].to_str().and_then(|a| a.split_once(',')) {
                    if let (Ok(x), Ok(y)) = (x.trim().parse::<f32>(), y.trim().parse::<f32>()) {
                        pos = Some([x, y]);
                    }
                }
                k += 1;
            }
            _ => {}
        }
        k += 1;
    }

    // Assumes main.rs is in src/ and logo.ico is in assets/.
    let rgba = image::load_from_memory(include_bytes!("../assets/logo.ico"))
        .expect("Could not load assets/logo.ico")
        .into_rgba8();
    let (width, height) = rgba.dimensions();
    let icon = egui::IconData {
        rgba: rgba.into_raw(),
        width,
        height,
    };

    // Same size as last time.
    let saved = load_settings();
    let size = saved
        .window_size
        .filter(|s| s[0] >= 520.0 && s[1] >= 340.0 && s[0] <= 10000.0 && s[1] <= 10000.0)
        .unwrap_or([1180.0, 760.0]);
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size(size)
        .with_min_inner_size([520.0, 340.0])
        .with_resizable(true)
        .with_decorations(if saved.custom_frame {false} else {true})
        .with_transparent(true) // required for the Mica backdrop
        .with_icon(Arc::new(icon));
    if let Some(p) = pos {
        // Explicit position from "drag a tab out".
        viewport = viewport.with_position(p);
    } else {
        // Same place and state as last time.
        if let Some(p) = saved.window_pos.filter(|p| window_pos_is_sane(*p)) {
            viewport = viewport.with_position(p);
        }
        if saved.maximized {
            viewport = viewport.with_maximized(true);
        }
    }

    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };

    eframe::run_native(
        "FileFly",
        options,
        Box::new(|cc| Ok(Box::new(Explorer::new(cc)))),
    )
}
