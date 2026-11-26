//! Persistent settings and the saved session (settings.json).

use crate::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub(crate) enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub(crate) enum TransparencyEffect {
    Acrylic,
    Mica,
    /// Last, so that `serde(other)` can sit here: an unknown name in settings.json becomes None.
    #[default]
    #[serde(other)]
    None,
}

impl TransparencyEffect {
    /// The effects this system supports, in the order the Settings page lists them.
    /// Mica needs Windows 11 (build 22000+), Acrylic any Windows 10 1803+; elsewhere there are none.
    pub(crate) fn available() -> &'static [TransparencyEffect] {
        static LIST: std::sync::OnceLock<&'static [TransparencyEffect]> = std::sync::OnceLock::new();
        LIST.get_or_init(|| -> &'static [TransparencyEffect] {
            if !cfg!(windows) {
                return &[TransparencyEffect::None];
            }
            let build = System::kernel_version().ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
            if build >= 22000 {
                &[TransparencyEffect::Mica, TransparencyEffect::Acrylic, TransparencyEffect::None]
            } else {
                &[TransparencyEffect::Acrylic, TransparencyEffect::None]
            }
        })
    }

    /// Mica on Windows 11, otherwise None.
    pub(crate) fn system_default() -> TransparencyEffect {
        if Self::available().contains(&TransparencyEffect::Mica) { TransparencyEffect::Mica } else { TransparencyEffect::None }
    }

    /// This effect if the system supports it, else the system default.
    pub(crate) fn or_supported(self) -> TransparencyEffect {
        if Self::available().contains(&self) { self } else { Self::system_default() }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            TransparencyEffect::Mica => "Mica",
            TransparencyEffect::Acrylic => "Acrylic",
            TransparencyEffect::None => "None",
        }
    }
}

/// A split layout in a form that can be stored: leaves are indices into `Session::tabs`.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum SavedTile {
    Leaf(usize),
    Split {
        side_by_side: bool,
        ratio: f32,
        a: Box<SavedTile>,
        b: Box<SavedTile>,
    },
}

impl SavedTile {
    /// Panes whose tab can't be found are dropped; their sibling takes over.
    pub(crate) fn from_tile(t: &Tile, tabs: &[&Tab]) -> Option<SavedTile> {
        match t {
            Tile::Leaf(id) => tabs.iter().position(|x| x.id == *id).map(SavedTile::Leaf),
            Tile::Split { side_by_side, ratio, a, b } => {
                match (Self::from_tile(a, tabs), Self::from_tile(b, tabs)) {
                    (Some(a), Some(b)) => Some(SavedTile::Split {
                        side_by_side: *side_by_side,
                        ratio: *ratio,
                        a: Box::new(a),
                        b: Box::new(b),
                    }),
                    (x, None) => x,
                    (None, y) => y,
                }
            }
        }
    }

    /// `ids[i]` is the id of the restored tab `i`, or `None` if that folder is gone.
    pub(crate) fn to_tile(&self, ids: &[Option<u64>]) -> Option<Tile> {
        match self {
            SavedTile::Leaf(i) => ids.get(*i).copied().flatten().map(Tile::Leaf),
            SavedTile::Split { side_by_side, ratio, a, b } => {
                match (a.to_tile(ids), b.to_tile(ids)) {
                    (Some(a), Some(b)) => Some(Tile::Split {
                        side_by_side: *side_by_side,
                        ratio: if ratio.is_finite() { ratio.clamp(0.1, 0.9) } else { 0.5 },
                        a: Box::new(a),
                        b: Box::new(b),
                    }),
                    (x, None) => x,
                    (None, y) => y,
                }
            }
        }
    }
}

/// Which folders were open, in which tabs, and how they were arranged.
/// Folders are stored as text: a path that is not valid Unicode would otherwise make
/// serialising (and so saving *all* settings) fail.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Session {
    pub(crate) tabs: Vec<String>,
    pub(crate) active: usize,
    /// `None` when not split.
    pub(crate) tiles: Option<SavedTile>,
    pub sidebar_width: Option<f32>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Settings {
    pub(crate) font_size: f32,
    pub(crate) transparency_effect: TransparencyEffect,
    pub(crate) follow_text_scale: bool,
    pub(crate) use_system_accent: bool,
    pub(crate) custom_accent: [u8; 3],
    pub(crate) theme: ThemeChoice,
    pub(crate) show_hidden: bool,
    /// Determines whether window will have a drag space
    pub(crate) left_drag_space: bool,
    /// List mapped network drives / network mounts in the sidebar.
    pub(crate) network_drives: bool,
    /// Folders pinned to Quick access. `None`: the default list (Home, Desktop, Downloads, ...).
    pub(crate) quick_access: Option<Vec<String>>,
    pub(crate) date_format: DateFormat,
    pub(crate) search_threads: usize,
    pub(crate) max_search_results: usize,
    pub(crate) calculate_folder_sizes: bool,
    pub(crate) index_dir: Option<PathBuf>,
    pub(crate) git_activated: bool,
    /// Widths (at 14 pt text) of Date modified, Type, Size.
    pub(crate) col_widths: [f32; 3],
    /// Inner size of the window when it was last closed (points), and whether it was maximised.
    pub(crate) window_size: Option<[f32; 2]>,
    /// Top-left of the window (points, screen coordinates) when it was last not maximised.
    pub(crate) window_pos: Option<[f32; 2]>,
    pub(crate) maximized: bool,
    pub(crate) custom_frame: bool,
    pub(crate) blur_paning: bool,
    pub(crate) compact: bool,
    /// Folders with a custom icon chosen by the user: folder path -> `.ico` file.
    pub(crate) folder_icons: HashMap<String, String>,
    /// Open tabs and split layout from the last session.
    pub(crate) session: Session,
    pub(crate) startup_update_check: bool,
    /// Reopen the tabs of the previous session on start.
    pub(crate) restore_tabs: bool,
    /// Show the "What's new" button until the release notes of the running version were read.
    pub(crate) show_whats_new: bool,
    /// The version whose release notes the user has already seen.
    pub(crate) whats_new_seen: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_size: 14.0,
            follow_text_scale: if cfg!(windows) {true} else {false},
            use_system_accent: if cfg!(windows) {true} else {false},
            custom_accent: [0x00, 0x78, 0xD4],
            theme: ThemeChoice::System,
            show_hidden: false,
            network_drives: true,
            quick_access: None,
            date_format: DateFormat::default(),
            search_threads: 4_usize,
            max_search_results: 100_000_usize,
            calculate_folder_sizes: true,
            index_dir: None,
            git_activated: false,
            col_widths: [150.0, 110.0, 120.0],
            left_drag_space: true,
            window_size: None,
            window_pos: None,
            custom_frame: true,
            compact: false,
            blur_paning: true,
            session: Session::default(),
            maximized: false,
            folder_icons: HashMap::new(),
            transparency_effect: TransparencyEffect::system_default(),
            startup_update_check: false,
            restore_tabs: true,
            show_whats_new: true,
            whats_new_seen: String::new(),
        }
    }
}

/// Generates `copy_page_fields_from` / `page_fields_eq` for the fields the Settings page edits
/// (window geometry, columns, the session and pins belong to the app and are left alone).
macro_rules! page_fields {
    ($($f:ident),* $(,)?) => {
        /// Copies the Settings-page fields from `o`.
        pub(crate) fn copy_page_fields_from(&mut self, o: &Settings) {
            $(self.$f = o.$f;)*
            #[cfg(not(windows))]
            { self.follow_text_scale = false; }
        }
        /// Whether all Settings-page fields equal those of `o`.
        pub(crate) fn page_fields_eq(&self, o: &Settings) -> bool {
            true $(&& self.$f == o.$f)*
        }
    };
}

///Implements the Settings functions.
impl Settings {
    page_fields!(
        theme, font_size, follow_text_scale, left_drag_space, custom_frame, use_system_accent,
        custom_accent, transparency_effect, show_hidden, date_format, network_drives,
        max_search_results, search_threads, calculate_folder_sizes,
        git_activated, compact, startup_update_check, restore_tabs, show_whats_new, blur_paning,
    );

    /// Size the UI actually uses. Following the system replaces the custom size.
    pub(crate) fn effective_font_size(&mut self, system_scale: f32) -> f32 {
        #[cfg(windows)]
        let scale = self.follow_text_scale;
        #[cfg(not(windows))]
        let scale = false;
        self.follow_text_scale = scale;
        if scale{
            14.0 * system_scale
        } else {
            self.font_size
        }
    }
}

pub(crate) fn app_dir() -> PathBuf {
    dirs::data_local_dir()
        .or_else(dirs::data_dir)
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("FileFly")
}

/// Set once this window has resized a column. Other FileFly windows are separate processes
/// that all write settings.json when they close; a window that never touched the column
/// widths must not overwrite what another window saved.
pub(crate) static COLS_TOUCHED: AtomicBool = AtomicBool::new(false);

/// Windows started with `--open` (a tab dragged out) neither restore nor save the session:
/// they would overwrite the main window's tabs with their single one.
pub(crate) static SESSION_READONLY: AtomicBool = AtomicBool::new(false);

pub(crate) fn load_settings() -> Settings {
    let mut s: Settings = fs::read_to_string(app_dir().join("settings.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    // A setting saved on another system (or hand-edited) may name an unsupported effect.
    s.transparency_effect = s.transparency_effect.or_supported();
    // A hand-edited or damaged file must not hide columns or make them unusable.
    let def = Settings::default().col_widths;
    for (w, d) in s.col_widths.iter_mut().zip(def) {
        *w = if w.is_finite() { w.clamp(50.0, 600.0) } else { d };
    }
    s
}

pub(crate) fn save_settings(s: &Settings) {
    let mut s = s.clone();
    if !COLS_TOUCHED.load(AtomicOrdering::Relaxed) {
        s.col_widths = load_settings().col_widths;
    }
    if SESSION_READONLY.load(AtomicOrdering::Relaxed) {
        s.session = load_settings().session;
    }
    let s = &s;
    let dir = app_dir();
    let _ = fs::create_dir_all(&dir);
    if let Ok(t) = serde_json::to_string_pretty(s) {
        let _ = fs::write(dir.join("settings.json"), t);
    }
}
