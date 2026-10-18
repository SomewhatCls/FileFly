//! Persistent settings and the saved session (settings.json).

use crate::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub(crate) enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
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
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Settings {
    pub(crate) font_size: f32,
    pub(crate) use_mica: bool,
    pub(crate) follow_text_scale: bool,
    pub(crate) use_system_accent: bool,
    pub(crate) custom_accent: [u8; 3],
    pub(crate) theme: ThemeChoice,
    pub(crate) show_hidden: bool,
    /// List mapped network drives / network mounts in the sidebar.
    pub(crate) network_drives: bool,
    /// Folders pinned to Quick access. `None`: the default list (Home, Desktop, Downloads, ...).
    pub(crate) quick_access: Option<Vec<String>>,
    pub(crate) date_format: DateFormat,
    pub(crate) search_threads: usize,
    pub(crate) max_search_results: usize,
    pub(crate) index_dir: Option<PathBuf>,
    /// Widths (at 14 pt text) of Date modified, Type, Size.
    pub(crate) col_widths: [f32; 3],
    /// Inner size of the window when it was last closed (points), and whether it was maximised.
    pub(crate) window_size: Option<[f32; 2]>,
    /// Top-left of the window (points, screen coordinates) when it was last not maximised.
    pub(crate) window_pos: Option<[f32; 2]>,
    pub(crate) maximized: bool,
    /// Open tabs and split layout from the last session.
    pub(crate) session: Session,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_size: 14.0,
            follow_text_scale: true,
            use_system_accent: true,
            custom_accent: [0x00, 0x78, 0xD4],
            theme: ThemeChoice::System,
            show_hidden: false,
            network_drives: true,
            quick_access: None,
            date_format: DateFormat::default(),
            search_threads: 4_usize,
            max_search_results: 100_000_usize,
            index_dir: None,
            col_widths: [150.0, 110.0, 90.0],
            window_size: None,
            window_pos: None,
            session: Session::default(),
            maximized: false,
            use_mica: true,
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
