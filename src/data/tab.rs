//! Per-tab state (current folder, listing, selection, history, search).

use crate::prelude::*;

pub(crate) struct RenameState {
    pub(crate) path: PathBuf,
    pub(crate) text: String,
    pub(crate) focus_pending: bool,
    pub(crate) is_dir: bool,
}

pub(crate) struct Tab {
    pub(crate) id: u64,
    pub(crate) dir: PathBuf,
    pub(crate) back: Vec<PathBuf>,
    pub(crate) fwd: Vec<PathBuf>,
    pub(crate) entries: Vec<Entry>,
    pub(crate) lookup: HashMap<PathBuf, usize>,
    pub(crate) view: Vec<usize>,
    pub(crate) view_dirty: bool,
    pub(crate) search: String,
    pub(crate) selected: HashSet<PathBuf>,
    /// Last clicked row; start of shift-ranges and origin of keyboard navigation.
    pub(crate) anchor: Option<PathBuf>,
    pub(crate) error: Option<String>,
    pub(crate) git_error: Option<String>,
    pub(crate) loading: bool,
    pub(crate) load_gen: u64,
    pub(crate) sort_col: SortColumn,
    pub(crate) sort_asc: bool,
    pub(crate) address: String,
    pub(crate) editing_addr: bool,
    pub(crate) addr_focus: bool,
    pub(crate) scroll_to: Option<usize>,
    pub(crate) rename: Option<RenameState>,
    pub(crate) pending_rename: Option<PathBuf>,
    pub(crate) trash: Option<TrashState>,
    /// True: this tab shows the Settings page instead of a folder.
    pub(crate) settings: bool,
    pub(crate) search_job: Option<SearchJob>,
    pub(crate) search_results: Vec<Entry>,
    /// hash of a path -> index into `search_results`. Keeps only 8 bytes per hit instead of a second copy of
    /// every path (up to 100,000 hits); `search_index` double-checks the hit, so a collision is just a miss.
    pub(crate) search_lookup: HashMap<u64, usize>,
    /// Normalized query the current search job was started for (see `Explorer::pump_search`).
    pub(crate) search_query: String,
    pub(crate) chosen_search_scope: SearchScope,
    pub(crate) current_search_scope: SearchScope
}

/// Stable 64-bit key of a path for `Tab::search_lookup`.
pub(crate) fn path_key(p: &Path) -> u64 {
    let mut h = DefaultHasher::new();
    p.hash(&mut h);
    h.finish()
}

impl Tab {
    pub(crate) fn new(id: u64, dir: PathBuf) -> Self {
        // Restored sessions / dragged-out tabs may still carry a \\?\ verbatim path.
        let dir = clean_path(&dir);
        Self {
            id,
            address: dir.display().to_string(),
            dir,
            back: Vec::new(),
            fwd: Vec::new(),
            entries: Vec::new(),
            lookup: HashMap::new(),
            view: Vec::new(),
            view_dirty: true,
            search: String::new(),
            selected: HashSet::new(),
            anchor: None,
            error: None,
            git_error: None,
            loading: false,
            load_gen: 0,
            sort_col: SortColumn::Name,
            sort_asc: true,
            editing_addr: false,
            addr_focus: false,
            scroll_to: None,
            rename: None,
            pending_rename: None,
            trash: None,
            settings: false,
            search_job: None,
            search_results: Vec::new(),
            search_lookup: HashMap::new(),
            search_query: String::new(),
            chosen_search_scope: SearchScope::CurrentDir,
            current_search_scope: SearchScope::CurrentDir,
        }
    }

    /// The folder as the user should see it: inside an opened zip that is "<zip>\\inner\\path"
    /// (`dir` itself is the real, unpacked folder behind it).
    pub(crate) fn shown_dir(&self) -> PathBuf {
        display_path(&self.dir)
    }

    pub(crate) fn select_only(&mut self, p: PathBuf) {
        self.selected.clear();
        self.selected.insert(p.clone());
        self.anchor = Some(p);
    }

    pub(crate) fn clear_selection(&mut self) {
        self.selected.clear();
        self.anchor = None;
    }

    /// Selected items that are currently visible, in on-screen order.
    pub(crate) fn selected_paths(&self) -> Vec<PathBuf> {
        // `get`, not indexing: right after a listing refresh `view` may briefly be stale.
        self.view
            .iter()
            .filter_map(|&i| self.source().get(i))
            .map(|e| &e.path)
            .filter(|p| self.selected.contains(*p))
            .cloned()
            .collect()
    }

    pub(crate) fn title(&self) -> String {
        if self.trash.is_some() {
            return "Recycle Bin".to_owned();
        }
        if self.settings {
            return "Settings".to_owned();
        }
        let dir = self.shown_dir();
        dir.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.display().to_string())
    }
    pub(crate) fn searching(&self) -> bool {
        !self.search.trim().is_empty()
    }
    /// Recycle Bin or Settings: the tab shows a page of its own, not the files of `dir`.
    pub(crate) fn special(&self) -> bool {
        self.trash.is_some() || self.settings
    }
    /// Everything that indexes into `view` (the table, selection, context menu) must read from this instead of `self.entries`.
    pub(crate) fn source(&self) -> &[Entry] {
        if self.search.trim().is_empty() { &self.entries } else { &self.search_results }
    }

    /// Index of `p` in whichever list `view` currently points into.
    pub(crate) fn search_index(&self, p: &Path) -> Option<usize> {
        let i = *self.search_lookup.get(&path_key(p))?;
        (self.search_results.get(i)?.path == p).then_some(i)
    }

    pub(crate) fn index_of(&self, p: &Path) -> Option<usize> {
        if self.searching() {
            self.search_index(p)
        } else {
            self.lookup.get(p).copied()
        }
    }

    pub(crate) fn find(&self, p: &Path) -> Option<&Entry> {
        if self.search.trim().is_empty() {
            self.lookup.get(p).map(|&i| &self.entries[i])
        } else {
            self.search_index(p).map(|i| &self.search_results[i])
        }
    }
}
