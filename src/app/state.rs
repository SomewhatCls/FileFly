//! The `Explorer` application state and its construction / session handling.

use crate::prelude::*;

/// Everything that is shared by all windows: settings, operations and their locks, services, clipboard.
/// It holds no window-bound value and outlives every window (`startup::run` re-creates the window around
/// the same core, see `attach` / `detach`). Background threads wake the UI only through `waker`.
pub(crate) struct Core {
    /// Repaint hook for every background thread; points at `ui` while a window exists.
    pub(crate) waker: Waker,
    pub(crate) settings: Settings,
    pub(crate) next_tab_id: u64,
    /// The session cache for git repository checks.
    pub(crate) git: Option<GitState>,
    pub(crate) drives: Vec<DriveInfo>,
    /// Background thread that polls the drives live (see drives.rs).
    pub(crate) drive_ctl: Arc<DriveWatchCtl>,
    /// True once the first drive list has arrived (Quick access availability is only judged after that).
    pub(crate) drives_ready: bool,
    pub(crate) quick: Vec<QuickItem>,
    pub(crate) clipboard: Option<(Vec<PathBuf>, bool)>,
    pub(crate) peazip: Option<PathBuf>,
    /// Zip files that are being unpacked right now so they can be browsed.
    pub(crate) extracting: HashSet<PathBuf>,
    pub(crate) trash_busy: Arc<std::sync::Mutex<HashSet<std::ffi::OsString>>>,
    pub(crate) msg_tx: Sender<AppMsg>,
    pub(crate) msg_rx: Receiver<AppMsg>,
    pub(crate) idx_rx: Receiver<IndexMsg>,
    /// Kept so an `Indexer` can be created later, when "Calculate folder sizes" is turned on at runtime.
    pub(crate) idx_tx: Sender<IndexMsg>,
    /// None while "Calculate folder sizes" is off: no index file, no scan threads, no live updater.
    pub(crate) indexer: Option<Indexer>,
    /// Also runs without the indexer, then only for the live listing refresh (see `DirWatcher::ui_only`).
    pub(crate) watcher: Option<DirWatcher>,
    pub(crate) index_dir: PathBuf,
    /// What `can_paste` last found, with the clipboard's change counter at that time.
    pub(crate) clip_probe: std::cell::Cell<Option<(u32, bool)>>,
    pub(crate) jobs_running: usize,
    pub(crate) job_label: String,
    /// Background file operations with a progress dialog (see `ui_ops`).
    pub(crate) ops: Vec<ActiveOp>,
    pub(crate) next_op: u64,
    pub(crate) conflict: Option<PendingPaste>,
    pub(crate) restore_conflict: Option<PendingRestore>,
    pub(crate) props_gen: Arc<AtomicU64>,
    pub(crate) settings_dirty: bool,
    /// FileFly is registered as the default folder handler (cached; read when Settings opens).
    pub(crate) default_on: bool,
    /// Last error from changing the default file manager, shown in the Settings page.
    pub(crate) default_error: Option<String>,
    pub(crate) sys_accent: Option<Accent>,
    pub(crate) last_poll: Instant,
    pub(crate) last_save: Instant,
    pub(crate) key_state: Option<device_query::DeviceState>,
    /// What the folder-size indexer was last started for; avoids restarting an identical scan.
    pub(crate) index_sig: Option<(u64, usize)>,
    pub(crate) reindex_force: bool,
    /// Set by anything that may have changed what the windows show: the next `pump_core` restarts the scan
    /// for the union of all windows' visible folders.
    pub(crate) reindex_wanted: bool,
    /// Other FileFly windows (tab hand-over).
    pub(crate) peers: Peers,
    /// Windows transparency effects available (off in Battery Saver / when disabled in Settings).
    pub(crate) transparency: bool,
    pub(crate) refresh_ui: bool,
    /// Current state of the update request.
    pub(crate) update: UpdateState,
    /// This stores which entry icons are currently being probed, to prevent
    /// threads from probing the same ones.
    pub(crate) probed_icons: HashSet<PathBuf>,
    /// Holds staged files for extract operations.
    pub(crate) extract_staging: Vec<PathBuf>,
    /// Holds staged files whose extraction was deferred because the target folder holds
    /// files with the same name.
    pub(crate) deferred_extracts: Vec<PastePlan>,
    pub(crate) paste_reg: PasteRegistry,
}

/// Progress of the resize that makes eframe refit its surface after a live frame change.
pub(crate) enum Reshape {
    /// Waiting for the new client size to show up (`before` is the size when the change was made).
    Wait { before: Option<egui::Vec2>, frames: u8 },
    /// The window was grown by one point: put it back to this size.
    Restore(egui::Vec2),
}

/// State of one window: its tabs, layout, dialogs and everything bound to its egui context.
pub(crate) struct Window {
    /// Unique for the life of the process, never reused. Part of every id and title that must not collide
    /// between windows (egui's temp data and viewport ids are shared by all of them).
    pub(crate) serial: u64,
    /// This window's viewport.
    pub(crate) vid: egui::ViewportId,
    /// What the OS window is created with. Only `refresh_shell` changes it (the decorations flag): egui patches the
    /// live window to match, so nothing else may differ from what the window was created with.
    pub(crate) builder: egui::ViewportBuilder,
    /// Handle of the OS window, found by its unique start title once it exists (see `track_native`).
    pub(crate) native: Option<NativeWindow>,
    /// Created this frame: it still has to come to the front.
    pub(crate) fresh: bool,
    /// The shared egui context (every window draws into the same one). Threads must use `Core::waker`.
    pub(crate) ui: egui::Context,
    /// Debounce of live listing refreshes and of folder-size rescans caused by file changes.
    pub(crate) list_debounce: Debounce,
    pub(crate) size_debounce: Debounce,
    pub(crate) tabs: Vec<Tab>,
    pub(crate) active: usize,
    /// True when the last tab was closed, thereby finishing the session.
    pub(crate) session_ended: bool,
    /// Ids of Recycle Bin items that a running operation is working on (restoring, deleting): they can't be
    /// started on a second time. Shared with the worker threads, which release them when they end.
    pub(crate) typeahead: TypeAhead,
    pub(crate) props: Option<Props>,
    pub(crate) text_scale: f32,
    pub(crate) applied_style: Option<(f32, Accent, bool, f32)>,
    /// Error text + what "Try as administrator" does for it (only offered while that error shows).
    pub(crate) admin_retry: Option<(String, AdminRetry)>,
    pub(crate) m: Metrics,
    pub(crate) pal: Palette,
    pub(crate) last_title: String,
    pub(crate) first_anim: f32,
    pub(crate) font_edit: Option<f32>,
    pub(crate) confirm: Option<ConfirmPurge>,
    /// The "Select a destination" dialog is showing.
    pub(crate) picker: Option<picker::FolderPicker>,
    pub(crate) sidebar_rect: Option<Rect>,
    pub(crate) prev_paste_key: Cell<bool>,
    pub(crate) last_paste: Cell<Instant>,
    /// None: the normal single-tab view. Some: up to four tabs tiled in the file area.
    pub(crate) tiles: Option<Tile>,
    /// Tab (by id) currently being dragged out of the tab strip.
    pub(crate) tab_drag: Option<u64>,
    pub(crate) own_info: Option<WinInfo>,
    pub(crate) info_stamp: Instant,
    pub(crate) backdrop_transparency: Option<TransparencyEffect>,
    pub(crate) backdrop_start: Instant,
    pub(crate) backdrop_retries: usize,
    pub(crate) is_backdrop: bool,
    pub(crate) use_custom_window_frame: bool,
    /// Set when the frame style was changed on the live OS window (see `Window::settle_surface`).
    pub(crate) reshape: Option<Reshape>,
    pub(crate) text_scale_checked: Instant,
    /// The last known theme.
    pub(crate) theme_choice: ThemeChoice,
    /// (when checked, whether another FileFly window is under the pointer) while dragging a tab.
    pub(crate) peer_under: (Instant, bool),
    /// Pane rectangles (tab id, screen rect) from the last frame: drop targets.
    pub(crate) pane_rects: Vec<(u64, Rect)>,
    /// Set after an OS-level drag-out; ignores the (possibly stale) pointer state until a fresh click.
    pub(crate) drag_lock: bool,
    /// The "What's new" popup, while it is open.
    pub(crate) whats_new: Option<WhatsNew>,
    /// Whether the sidebar is compacted.
    pub(crate) sidebar_collapsed: bool,
    /// Width (points) of the expanded sidebar: restored from the session, kept current by
    /// `ui_sidebar`, and written back with the session. None until it is known.
    pub(crate) sidebar_width: Option<f32>,
}

/// The shared core plus every window. Methods live here because they mix both halves.
///
/// `win` is the window being processed: `with_window(i, ..)` swaps `windows[i]` into it for the duration of
/// the call and back out afterwards, so all window code reads `self.win` whichever window it works for.
/// Outside `with_window` it is an empty placeholder, and `windows[i]` holds the real window. Code that
/// needs every window (indexer union, message routing) goes through `each_window` / `each_window_mut`.
pub(crate) struct Explorer {
    pub(crate) core: Core,
    pub(crate) win: Window,
    pub(crate) windows: Vec<Window>,
    /// Index in `windows` that is currently swapped into `win`.
    pub(crate) cur: Option<usize>,
    /// Serial of the window that was focused last: it hosts the dialogs that belong to no window (operations,
    /// conflicts) and shows hidden operations in its status bar.
    pub(crate) focus: u64,
    pub(crate) next_window: u64,
    /// Work that touches another window than the one running; done by the host between window frames.
    pub(crate) pending: Vec<WinCmd>,
    /// The shared egui context, once the host window exists (a placeholder before).
    pub(crate) ctx: egui::Context,
    pub(crate) icon: Option<Arc<egui::IconData>>,
    /// Draws the blurred backdrop behind tab paning overlays (owns GL resources of the host window).
    pub(crate) blur: Option<BackdropBlur>,
    /// The host window's GL handle, kept so the blur can be built again when its setting is switched.
    pub(crate) gl: Option<Arc<eframe::glow::Context>>,
    /// A setting that needs the windows to start over was changed (see `refresh_windows`); handled at the start of
    /// the next host frame.
    pub(crate) refresh_pending: bool,
}

/// A request that involves a window other than the one it was made in (see `Explorer::pending`).
pub(crate) enum WinCmd {
    /// Moves tab `tab` out of window `from`: into window `to`, or into a new window at `at` (screen points).
    MoveTab { from: u64, tab: u64, to: Option<u64>, at: Option<(f32, f32)> },
    /// Moves every tab of window `from` into window `to`, then closes `from`.
    MergeWindow { from: u64, to: u64 },
}

impl Window {
    /// A window with one tab, drawn into the shared context `ui`. `builder` is what its OS window is created
    /// with (see `window_builder`).
    pub(crate) fn new(
        serial: u64,
        tab: Tab,
        settings: &Settings,
        ui: egui::Context,
        builder: egui::ViewportBuilder,
    ) -> Self {
        Self {
            serial,
            vid: egui::ViewportId::from_hash_of(("window", serial)),
            builder,
            native: None,
            fresh: true,
            ui,
            list_debounce: Debounce::default(),
            size_debounce: Debounce::default(),
            tabs: vec![tab],
            active: 0,
            session_ended: false,
            typeahead: TypeAhead::default(),
            props: None,
            text_scale: system_text_scale(),
            applied_style: None,
            admin_retry: None,
            m: Metrics::new(14.0, 1.0, settings.compact),
            pal: Palette::new(egui::Theme::Dark, Accent::fallback(), TransparencyEffect::None),
            last_title: String::new(),
            first_anim: 1.0,
            font_edit: None,
            confirm: None,
            picker: None,
            sidebar_rect: None,
            tiles: None,
            tab_drag: None,
            own_info: None,
            info_stamp: Instant::now(),
            backdrop_transparency: None,
            backdrop_start: Instant::now(),
            backdrop_retries: 0,
            is_backdrop: false,
            use_custom_window_frame: settings.custom_frame,
            reshape: None,
            theme_choice: ThemeChoice::default(),
            text_scale_checked: Instant::now(),
            peer_under: (Instant::now(), false),
            pane_rects: Vec::new(),
            drag_lock: false,
            prev_paste_key: Cell::new(false),
            last_paste: Cell::new(Instant::now().checked_sub(Duration::from_secs(1)).unwrap_or_else(Instant::now)),
            whats_new: None,
            sidebar_collapsed: false,
            sidebar_width: settings.session.sidebar_width,
        }
    }

    /// Indices of the tabs currently on screen (all tiled tabs, or just the active one).
    pub(crate) fn visible_tabs(&self) -> Vec<usize> {
        if let Some(t) = &self.tiles {
            let mut ids = Vec::new();
            t.leaf_ids(&mut ids);
            let v: Vec<usize> = ids.iter().filter_map(|id| self.tabs.iter().position(|x| x.id == *id)).collect();
            if !v.is_empty() {
                return v;
            }
        }
        vec![self.active]
    }

    /// What `Explorer::win` holds while no window is swapped in. Never drawn.
    pub(crate) fn placeholder(settings: &Settings) -> Self {
        Self::new(0, Tab::new(0, PathBuf::new()), settings, egui::Context::default(), egui::ViewportBuilder::default())
    }

    /// Starts the window's own state over for changed settings (frame style, backdrop, dialogs) while the OS
    /// window itself lives on: destroying it and creating it again within one egui frame cannot be done (eframe
    /// panics: "the user callback was never called"), and a window needs no new OS window for these settings, egui
    /// patches the decorations of the live one.
    pub(crate) fn refresh_shell(&mut self, settings: &Settings) {
        if self.use_custom_window_frame != settings.custom_frame {
            self.reshape = Some(Reshape::Wait { before: None, frames: 0 });
        }
        self.builder = self.builder.clone().with_decorations(!settings.custom_frame);
        self.use_custom_window_frame = settings.custom_frame;
        self.applied_style = None;
        self.sidebar_rect = None;
        self.tab_drag = None;
        self.pane_rects.clear();
        self.is_backdrop = false;
        self.backdrop_transparency = None;
        self.backdrop_start = Instant::now();
        self.backdrop_retries = 0;
        self.theme_choice = ThemeChoice::default();
        self.own_info = None;
        self.info_stamp = Instant::now();
        self.peer_under = (Instant::now(), false);
        self.drag_lock = false;
        self.first_anim = 1.0;
        // Its search threads and dialog window belong to the old OS window.
        self.picker = None;
    }
}

/// The special tabs in a saved session: (its page, None for the Recycle Bin) and the marker written instead of
/// a folder. Index 0 is the Recycle Bin.
const SPECIAL_TABS: [(Option<Page>, &str); 3] = [
    (Some(Page::RecycleBin), "::recycle-bin"),
    (Some(Page::Settings), "::settings"),
    (Some(Page::Licenses), "::licenses"),
];

impl Explorer {
    /// Builds the core and the first window (no OS window yet: the host shows it). `open_dir` is the folder given
    /// with `--open` (another process, e.g. an elevated one): it replaces the session and makes this process
    /// leave the saved session alone. `pos` (`--pos`) overrides the first window's saved position.
    pub(crate) fn new(open_dir: Option<PathBuf>, pos: Option<[f32; 2]>, icon: Arc<egui::IconData>) -> Self {
        let waker = Waker::default();
        let mut settings = load_settings();
        settings.session_readonly = open_dir.is_some();
        let git_activated = settings.git_activated;
        let check_for_update = settings.startup_update_check;

        let (msg_tx, msg_rx) = mpsc::channel();
        let (idx_tx, idx_rx) = mpsc::channel();
        let index_dir = settings
            .index_dir
            .clone()
            .filter(|p| p.is_dir())
            .unwrap_or_else(app_dir);
        trash_bin::init_index(&index_dir);

        let indexer = settings
            .calculate_folder_sizes
            .then(|| Indexer::new(waker.clone(), idx_tx.clone(), &index_dir));
        // Without the indexer the watcher only refreshes the listing.
        let watcher = match &indexer {
            Some(ix) => DirWatcher::new(waker.clone(), ix.change_sink()),
            None => DirWatcher::ui_only(waker.clone()),
        };
        trash_bin::set_index_enabled(indexer.is_some());
        std::thread::spawn(|| {
            // Leftovers of a crashed run: unpacked zips, and half-finished copy/move/zip temp items.
            sweep_old_archive_caches();
            sweep_leftover_temps();
        });

        let start = open_dir
            .clone()
            .filter(|p| p.is_dir())
            .or_else(|| dirs::home_dir().filter(|p| p.is_dir()))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));

        let quick = build_quick(&settings);

        // Live drive polling: USB sticks / network shares appearing, vanishing or getting their label.
        let drive_ctl = {
            let (tx, repaint) = (msg_tx.clone(), waker.clone());
            spawn_watcher(settings.network_drives, move |d| {
                let alive = tx.send(AppMsg::Drives(d)).is_ok();
                repaint.wake();
                alive
            })
        };

        let core = Core {
            waker: waker.clone(),
            settings,
            next_tab_id: 2,
            drives: Vec::new(),
            drive_ctl,
            drives_ready: false,
            quick,
            clipboard: None,
            peazip: find_peazip(),
            extracting: HashSet::new(),
            trash_busy: Arc::default(),
            msg_tx,
            msg_rx,
            idx_rx,
            idx_tx,
            indexer,
            git: (git_available() && git_activated).then(|| GitState::new(waker.clone())),
            watcher,
            index_dir,
            clip_probe: std::cell::Cell::new(None),
            jobs_running: 0,
            job_label: String::new(),
            ops: Vec::new(),
            next_op: 0,
            conflict: None,
            restore_conflict: None,
            props_gen: Arc::new(AtomicU64::new(0)),
            settings_dirty: false,
            default_on: default_app::is_default(),
            default_error: None,
            sys_accent: system_accent(),
            last_poll: Instant::now(),
            last_save: Instant::now(),
            index_sig: None,
            reindex_force: false,
            reindex_wanted: false,
            peers: Peers::new(waker.clone(), &app_dir()),
            transparency: system_transparency(),
            refresh_ui: false,
            key_state: std::panic::catch_unwind(device_query::DeviceState::new).ok(),
            update: UpdateState::Idle,
            probed_icons: HashSet::new(),
            extract_staging: Vec::new(),
            deferred_extracts: Vec::new(),
            paste_reg: PasteRegistry::default(),
        };
        let first = Window::new(
            1,
            Tab::new(1, start),
            &core.settings,
            egui::Context::default(),
            window_builder(&core.settings, pos, icon.clone(), true, 1),
        );
        let placeholder = Window::placeholder(&core.settings);
        let mut app = Self {
            core,
            win: first,
            windows: vec![placeholder],
            cur: Some(0),
            focus: 1,
            next_window: 2,
            pending: Vec::new(),
            ctx: egui::Context::default(),
            icon: Some(icon),
            blur: None,
            gl: None,
            refresh_pending: false,
        };
        // Independent of the tabs, so it also applies when FileFly is started on a given folder.
        app.win.sidebar_width = app.core.settings.session.sidebar_width;
        app.sync_special_icons();
        if open_dir.is_none() && app.core.settings.restore_tabs {
            app.restore_session();
        }
        #[cfg(feature = "github-updater")]
        if check_for_update {
            app.check_for_update(true);
        }
        // Tabs load when they first become visible (see `ui`), so restoring many tabs
        // doesn't read every folder and hold every listing in memory at once.
        app.on_active_changed();
        app.end_window();
        app
    }

    /// Binds the core to the host window's egui context, once: fonts, icons, theme, the repaint hook of every
    /// background thread, and the GL resources of the blurred drag backdrop. Never repeated: all windows share
    /// this context, so a settings restart only resets windows (see `refresh_windows`).
    pub(crate) fn attach(&mut self, cc: &eframe::CreationContext<'_>) {
        let ui = cc.egui_ctx.clone();
        egui_extras::install_image_loaders(&ui);
        install_fonts(&ui);
        ui.set_theme(match self.core.settings.theme {
            ThemeChoice::System => egui::ThemePreference::System,
            ThemeChoice::Light => egui::ThemePreference::Light,
            ThemeChoice::Dark => egui::ThemePreference::Dark,
        });
        let repaint = ui.clone();
        self.core.waker.set(move |d| {
            if d.is_zero() {
                repaint.request_repaint();
            } else {
                repaint.request_repaint_after(d);
            }
        });
        self.ctx = ui.clone();
        for w in self.windows.iter_mut().chain(std::iter::once(&mut self.win)) {
            w.ui = ui.clone();
        }
        icons_reset();
        self.gl = cc.gl.clone();
        self.apply_git_setting();
        self.apply_render_settings();
    }

    /// Switches the git toolbar state on or off. Needs no restart: the toolbar reads it every frame.
    pub(crate) fn apply_git_setting(&mut self) {
        let on = self.core.settings.git_activated && git_available();
        if on != self.core.git.is_some() {
            self.core.git = on.then(|| GitState::new(self.core.waker.clone()));
        }
    }

    /// (Re)builds what the "restart" settings switch on or off and the windows cannot patch live: the blurred
    /// drag backdrop (it changes the render pipeline). Runs at start and whenever that setting changed. Nothing
    /// here touches operations, the indexer or any OS window.
    pub(crate) fn apply_render_settings(&mut self) {
        let settings = &self.core.settings;
        // A second context draws the app twice for the blurred backdrop of tab paning drags.
        if !settings.blur_paning {
            if let Some(mut b) = self.blur.take() {
                b.release(); // no Drop: frees its textures
            }
        } else if self.blur.is_none() {
            self.blur = self.gl.clone().and_then(|gl| {
                BackdropBlur::new(gl, |ctx2| {
                    egui_extras::install_image_loaders(ctx2);
                    install_fonts(ctx2);
                })
            });
        }
    }

    /// Starts or stops folder-size indexing while the app runs ("Calculate folder sizes" in Settings).
    pub(crate) fn set_folder_sizes(&mut self, on: bool) {
        if on == self.core.indexer.is_some() {
            return;
        }
        trash_bin::set_index_enabled(on);
        if on {
            let ix = Indexer::new(self.core.waker.clone(), self.core.idx_tx.clone(), &self.core.index_dir);
            self.core.watcher = DirWatcher::new(self.core.waker.clone(), ix.change_sink());
            self.core.indexer = Some(ix);
        } else {
            self.core.watcher = DirWatcher::ui_only(self.core.waker.clone());
            if let Some(ix) = self.core.indexer.take() {
                // Make the last writes permanent; dropping the indexer then cancels the scans
                // and retires the live updater.
                ix.save_if_dirty(true);
            }
            // Sizes that were still on their way no longer belong to anything.
            while self.core.idx_rx.try_recv().is_ok() {}
            for w in self.each_window_mut() {
                for t in &mut w.tabs {
                    for e in t.entries.iter_mut().filter(|e| e.is_dir) {
                        e.dir_size = None;
                    }
                }
            }
        }
        self.core.index_sig = None;
        // The new watcher doesn't know any folder yet: this points it at the active one.
        self.on_active_changed();
        if on {
            self.start_indexing(true);
        }
    }

    /// Returns true if the Window is maximized.
    pub(crate) fn is_maximized(ctx: &egui::Context) -> bool {
        ctx.input(|i| i.viewport().maximized) == Some(true)
    }

    //paste key detection workaround
    pub(crate) fn paste_key_down(&self) -> bool {
        use device_query::{DeviceQuery, Keycode};
        let Some(state) = &self.core.key_state else { return false };
        let keys = state.get_keys();
        #[cfg(target_os = "macos")]
        let modifier = keys.contains(&Keycode::Command) || keys.contains(&Keycode::RCommand);
        #[cfg(not(target_os = "macos"))]
        let modifier = keys.contains(&Keycode::LControl) || keys.contains(&Keycode::RControl);
        modifier && keys.contains(&Keycode::V)
    }
    // ------------------------------------------------------------------ session

    /// How a tab is written in the session: its folder, or for a special tab a marker that no path can
    /// look like (`SPECIAL_TABS`), which an older version just skips.
    fn saved_tab(t: &Tab) -> String {
        match t.page {
            Some(Page::RecycleBin) => SPECIAL_TABS[0].1.to_owned(),
            Some(Page::Settings) => SPECIAL_TABS[1].1.to_owned(),
            Some(Page::Licenses) => SPECIAL_TABS[2].1.to_owned(),
            None => folder_outside(&t.dir).to_string_lossy().into_owned(),
        }
    }

    pub(crate) fn session_snapshot(&self) -> Session {
        let all: Vec<&Tab> = self.win.tabs.iter().collect();
        Session {
            tabs: all.iter().map(|t| Self::saved_tab(t)).collect(),
            active: self.win.active,
            tiles: self.win.tiles.as_ref().and_then(|t| SavedTile::from_tile(t, &all)),
            sidebar_width: self.win.sidebar_width,
        }
    }

    /// Re-opens the tabs and split layout of the last session. Folders that no longer exist
    /// are skipped; if none are left the normal start folder stays.
    pub(crate) fn restore_session(&mut self) {
        let ses = self.core.settings.session.clone();
        // A special tab sits on top of some folder, like when it is opened.
        let base = self.win.tabs.get(self.win.active).map(|t| t.dir.clone()).or_else(dirs::home_dir).unwrap_or_default();
        let mut ids: Vec<Option<u64>> = Vec::new();
        let mut tabs: Vec<Tab> = Vec::new();
        let mut bin_tabs: Vec<usize> = Vec::new();
        for p in &ses.tabs {
            let special = SPECIAL_TABS.iter().position(|(_, marker)| marker == p);
            let dir = PathBuf::from(p);
            // (A network folder is not looked at here: that could hold up the start for seconds. If it is gone,
            // its listing says so.)
            if special.is_none() && !is_network_path(&dir) && !dir.is_dir() {
                ids.push(None);
                continue;
            }
            let id = tabs.len() as u64 + 1;
            let mut tab = Tab::new(id, if special.is_some() { base.clone() } else { dir });
            match special {
                Some(0) if !trash_bin::SUPPORTED => {
                    ids.push(None);
                    continue;
                }
                Some(0) => bin_tabs.push(tabs.len()),
                Some(i) => {
                    tab.page = SPECIAL_TABS[i].0;
                }
                None => {}
            }
            tabs.push(tab);
            ids.push(Some(id));
        }
        if tabs.is_empty() {
            return;
        }
        self.core.next_tab_id = tabs.len() as u64 + 1;
        self.win.tabs = tabs;
        for ti in bin_tabs {
            self.show_trash(ti);
            self.win.tabs[ti].load_gen = 1;
        }
        self.win.active = ids
            .get(ses.active)
            .copied()
            .flatten()
            .and_then(|id| self.win.tabs.iter().position(|t| t.id == id))
            .unwrap_or(0);
        // A tree that ends up as a single pane is just the normal view (fix_tiles handles it).
        self.win.tiles = ses.tiles.and_then(|t| t.to_tile(&ids));
    }
}
