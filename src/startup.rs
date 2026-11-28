//! Process start-up: command line and the host app around `eframe::run_native`.

use crate::peers::forward_open;
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
        if is_handled_viewport_failure(&info.to_string()) {
            return; // `show_viewport` retries the window next frame
        }
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
    let mut open_dir: Option<PathBuf> = None;
    let mut k = 0;
    while k < args.len() {
        match args[k].to_str().unwrap_or_default() {
            "--open" if k + 1 < args.len() => {
                open_dir = Some(clean_path_arg(&args[k + 1]));
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

    // A folder opened from outside while FileFly already runs (with the same elevation) opens as a window of that
    // instance: one process, one set of operations and locks. Only when there is none does this process go on.
    if let Some(dir) = &open_dir {
        if forward_open(&app_dir(), dir, is_elevated(), Duration::from_secs(3)) {
            return Ok(());
        }
    }

    // Assumes main.rs is in src/ and logo.ico is in assets/.
    let rgba = image::load_from_memory(include_bytes!("../assets/logo.ico"))
        .expect("Could not load assets/logo.ico")
        .into_rgba8();
    let (width, height) = rgba.dimensions();
    let icon = Arc::new(egui::IconData {
        rgba: rgba.into_raw(),
        width,
        height,
    });

    let host = host_options(icon.clone());
    let explorer = Explorer::new(open_dir, pos, icon);
    eframe::run_native(
        "FileFly",
        host,
        Box::new(move |cc| {
            let mut ex = explorer;
            ex.attach(cc);
            Ok(Box::new(Host { ex, frames: 0 }))
        }),
    )
}

/// The root OS window is only a host for egui: hidden, off the taskbar, never shown. Every real window is a
/// viewport of it (see `app/windows.rs`), so closing any of them can never end the app by itself.
///
/// `with_transparent(true)` is required even though this window is never seen: eframe derives the one GL
/// config shared by all windows from the root's builder, and without it no window gets an alpha channel, so
/// the Mica/Acrylic backdrop of every window renders black.
///
/// `icon` must be the app icon even though this window never shows: eframe applies the root's icon (the default
/// egui one when there is none) to whichever window is active at startup, which is our first real window.
fn host_options(icon: Arc<egui::IconData>) -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_icon(icon)
            .with_transparent(true)
            .with_visible(false)
            .with_taskbar(false)
            .with_decorations(false)
            .with_inner_size([1.0, 1.0]),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    }
}

/// The `eframe::App` of the hidden root window: it owns the whole `Explorer` and draws all windows each frame.
struct Host {
    ex: Explorer,
    /// Frames drawn so far: the root window is told to hide itself during the first few (eframe shows a new
    /// window after its first frame, whatever the builder said).
    frames: u8,
}

impl eframe::App for Host {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.frames < 5 {
            self.frames += 1;
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Visible(false));
        }
        self.ex.host_frame(&ctx);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    /// The host ended without the last window going through `Explorer::quit` (for example the OS is
    /// shutting down): run the same clean-quit sequence, so operations and the journal are treated exactly
    /// like on a normal quit.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.ex.shutdown();
    }
}
