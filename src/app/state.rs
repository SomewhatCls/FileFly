//! The `Explorer` application state and its construction / session handling.

use crate::prelude::*;

pub(crate) struct Explorer {
    pub(crate) ui: egui::Context,
    pub(crate) settings: Settings,
    pub(crate) tabs: Vec<Tab>,
    pub(crate) active: usize,
    pub(crate) next_tab_id: u64,

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

    pub(crate) msg_tx: Sender<AppMsg>,
    pub(crate) msg_rx: Receiver<AppMsg>,
    pub(crate) idx_rx: Receiver<IndexMsg>,
    pub(crate) indexer: Indexer,
    pub(crate) watcher: Option<DirWatcher>,
    pub(crate) list_debounce: Debounce,
    pub(crate) size_debounce: Debounce,
    pub(crate) index_dir: PathBuf,

    pub(crate) jobs_running: usize,
    pub(crate) job_label: String,
    /// Background file operations with a progress dialog (see `ui_ops`).
    pub(crate) ops: Vec<ActiveOp>,
    pub(crate) next_op: u64,
    pub(crate) conflict: Option<PendingPaste>,
    pub(crate) props: Option<Props>,
    pub(crate) props_gen: Arc<AtomicU64>,
    pub(crate) settings_dirty: bool,
    /// FileFly is registered as the default folder handler (cached; read when Settings opens).
    pub(crate) default_on: bool,
    /// Last error from changing the default file manager, shown in the Settings page.
    pub(crate) default_error: Option<String>,
    /// The "Licenses" popup (opened from Settings) is showing.
    pub(crate) licenses_open: bool,

    pub(crate) sys_accent: Option<Accent>,
    pub(crate) text_scale: f32,
    pub(crate) last_poll: Instant,
    pub(crate) applied_style: Option<(f32, Accent)>,
    /// Error text + what "Try as administrator" does for it (only offered while that error shows).
    pub(crate) admin_retry: Option<(String, AdminRetry)>,
    pub(crate) m: Metrics,
    pub(crate) pal: Palette,
    pub(crate) last_title: String,
    pub(crate) last_save: Instant,
    pub(crate) first_anim: f32,
    pub(crate) font_edit: Option<f32>,
    pub(crate) confirm: Option<ConfirmPurge>,
    /// The "Select a destination" dialog is showing.
    pub(crate) picker: Option<picker::FolderPicker>,
    pub(crate) sidebar_rect: Option<Rect>,
    pub(crate) key_state: Option<device_query::DeviceState>,
    pub(crate) prev_paste_key: Cell<bool>,
    pub(crate) last_paste: Cell<Instant>,

    /// None: the normal single-tab view. Some: up to four tabs tiled in the file area.
    pub(crate) tiles: Option<Tile>,
    /// Tab (by id) currently being dragged out of the tab strip.
    pub(crate) tab_drag: Option<u64>,
    /// What the folder-size indexer was last started for; avoids restarting an identical scan.
    pub(crate) index_sig: Option<(u64, usize)>,
    pub(crate) reindex_force: bool,
    pub(crate) cache_size: Option<usize>,
    pub(crate) unlimited_cache: bool,

    /// Other FileFly windows (tab hand-over).
    pub(crate) peers: Peers,
    pub(crate) own_info: Option<WinInfo>,
    pub(crate) info_stamp: Instant,
    /// Windows transparency effects available (off in Battery Saver / when disabled in Settings).
    pub(crate) transparency: bool,
    pub(crate) backdrop_transp: Option<TransparencyEffect>,
    pub(crate) backdrop_start: Instant,
    pub(crate) backdrop_retries: usize,
    pub(crate) text_scale_checked: Instant,
    /// (when checked, whether another FileFly window is under the pointer) while dragging a tab.
    pub(crate) peer_under: (Instant, bool),
    /// Pane rectangles (tab id, screen rect) from the last frame: drop targets.
    pub(crate) pane_rects: Vec<(u64, Rect)>,
    /// Set after an OS-level drag-out; ignores the (possibly stale) pointer state until a fresh click.
    pub(crate) drag_lock: bool,
}

impl Explorer {
    pub(crate) fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ui = cc.egui_ctx.clone();
        egui_extras::install_image_loaders(&ui);
        install_fonts(&ui);

        let settings = load_settings();
        ui.set_theme(match settings.theme {
            ThemeChoice::System => egui::ThemePreference::System,
            ThemeChoice::Light => egui::ThemePreference::Light,
            ThemeChoice::Dark => egui::ThemePreference::Dark,
        });

        let (msg_tx, msg_rx) = mpsc::channel();
        let (idx_tx, idx_rx) = mpsc::channel();
        let index_dir = settings
            .index_dir
            .clone()
            .filter(|p| p.is_dir())
            .unwrap_or_else(app_dir);
        let indexer = Indexer::new(ui.clone(), idx_tx, &index_dir, settings.index_ram);
        let watcher =    DirWatcher::new(ui.clone(), indexer.change_sink());
        std::thread::spawn(|| {
            // Leftovers of a crashed run: unpacked zips, and half-finished copy/move/zip temp items.
            sweep_old_archive_caches();
            sweep_leftover_temps();
        });

        let start = START_DIR
            .get()
            .cloned()
            .filter(|p| p.is_dir())
            .or_else(|| dirs::home_dir().filter(|p| p.is_dir()))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));

        let quick = build_quick(&settings);

        // Live drive polling: USB sticks / network shares appearing, vanishing or getting their label.
        let drive_ctl = {
            let (tx, repaint) = (msg_tx.clone(), ui.clone());
            spawn_watcher(settings.network_drives, move |d| {
                let alive = tx.send(AppMsg::Drives(d)).is_ok();
                repaint.request_repaint();
                alive
            })
        };

        let mut app = Self {
            ui: ui.clone(),
            settings,
            tabs: vec![Tab::new(1, start)],
            active: 0,
            next_tab_id: 2,
            drives: Vec::new(),
            drive_ctl,
            drives_ready: false,
            quick,
            clipboard: None,
            peazip: find_peazip(),
            extracting: HashSet::new(),
            msg_tx,
            msg_rx,
            idx_rx,
            indexer,
            watcher,
            list_debounce: Debounce::default(),
            size_debounce: Debounce::default(),
            index_dir,
            jobs_running: 0,
            job_label: String::new(),
            ops: Vec::new(),
            next_op: 0,
            conflict: None,
            props: None,
            props_gen: Arc::new(AtomicU64::new(0)),
            settings_dirty: false,
            default_on: default_app::is_default(),
            default_error: None,
            licenses_open: false,
            sys_accent: system_accent(),
            text_scale: system_text_scale(),
            last_poll: Instant::now(),
            applied_style: None,
            admin_retry: None,
            m: Metrics::new(14.0, 1.0),
            pal: Palette::new(egui::Theme::Dark, Accent::fallback(), TransparencyEffect::None),
            last_title: String::new(),
            last_save: Instant::now(),
            first_anim: 1.0,
            font_edit: None,
            confirm: None,
            picker: None,
            sidebar_rect: None,
            tiles: None,
            tab_drag: None,
            index_sig: None,
            reindex_force: false,
            cache_size: None,
            unlimited_cache: false,
            peers: Peers::new(ui.clone(), &app_dir()),
            own_info: None,
            info_stamp: Instant::now(),
            transparency: system_transparency(),
            backdrop_transp: None,
            backdrop_start: Instant::now(),
            backdrop_retries: 0,
            text_scale_checked: Instant::now(),
            peer_under: (Instant::now(), false),
            pane_rects: Vec::new(),
            drag_lock: false,
            key_state: std::panic::catch_unwind(device_query::DeviceState::new).ok(),
            prev_paste_key: Cell::new(false),
            last_paste: Cell::new(
                Instant::now()
                    .checked_sub(Duration::from_secs(1))
                    .unwrap_or_else(Instant::now),
            ),
        };
        app.sync_special_icons();
        if START_DIR.get().is_none() {
            app.restore_session();
        }
        // Tabs load when they first become visible (see `ui`), so restoring many tabs
        // doesn't read every folder and hold every listing in memory at once.
        app.on_active_changed();
        app
    }

    //paste key detection workaround
    pub(crate) fn paste_key_down(&self) -> bool {
        use device_query::{DeviceQuery, Keycode};
        let Some(state) = &self.key_state else { return false };
        let keys = state.get_keys();
        #[cfg(target_os = "macos")]
        let modifier = keys.contains(&Keycode::Command) || keys.contains(&Keycode::RCommand);
        #[cfg(not(target_os = "macos"))]
        let modifier = keys.contains(&Keycode::LControl) || keys.contains(&Keycode::RControl);
        modifier && keys.contains(&Keycode::V)
    }
    // ------------------------------------------------------------------ session

    pub(crate) fn session_snapshot(&self) -> Session {
        // The Settings tab is not part of the session (it would come back as a plain folder tab).
        let kept: Vec<&Tab> = self.tabs.iter().filter(|t| !t.settings).collect();
        let active_id = self.tabs.get(self.active).map(|t| t.id);
        Session {
            tabs: kept.iter().map(|t| folder_outside(&t.dir).to_string_lossy().into_owned()).collect(),
            active: kept.iter().position(|t| Some(t.id) == active_id).unwrap_or(0),
            tiles: self
                .tiles
                .as_ref()
                .and_then(|t| SavedTile::from_tile(t, &kept)),
        }
    }

    /// Re-opens the tabs and split layout of the last session. Folders that no longer exist
    /// are skipped; if none are left the normal start folder stays.
    pub(crate) fn restore_session(&mut self) {
        let ses = self.settings.session.clone();
        let mut ids: Vec<Option<u64>> = Vec::new();
        let mut tabs: Vec<Tab> = Vec::new();
        for p in &ses.tabs {
            let dir = PathBuf::from(p);
            if dir.is_dir() {
                let id = tabs.len() as u64 + 1;
                tabs.push(Tab::new(id, dir));
                ids.push(Some(id));
            } else {
                ids.push(None);
            }
        }
        if tabs.is_empty() {
            return;
        }
        self.next_tab_id = tabs.len() as u64 + 1;
        self.tabs = tabs;
        self.active = ids
            .get(ses.active)
            .copied()
            .flatten()
            .and_then(|id| self.tabs.iter().position(|t| t.id == id))
            .unwrap_or(0);
        // A tree that ends up as a single pane is just the normal view (fix_tiles handles it).
        self.tiles = ses.tiles.and_then(|t| t.to_tile(&ids));
    }
}
