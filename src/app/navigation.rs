//! Navigation, tab loading, quick access and drive refresh.

use crate::prelude::*;

impl Explorer {
    pub(crate) fn load_tab(&mut self, ti: usize) {
        let (show_hidden, fmt) = (self.core.settings.show_hidden, self.core.settings.date_format);
        let tab = &mut self.win.tabs[ti];
        tab.load_gen += 1;
        tab.loading = true;
        tab.notice = None;
        let (id, seq, dir) = (tab.id, tab.load_gen, tab.dir.clone());
        let (tx, ui) = (self.core.msg_tx.clone(), self.core.waker.clone());
        std::thread::spawn(move || {
            let result = read_listing(&dir, show_hidden, fmt);
            let _ = tx.send(AppMsg::Listing {
                tab: id,
                seq,
                result,
            });
            ui.wake();
        });
    }

    pub(crate) fn reload_all(&mut self) {
        for i in 0..self.win.tabs.len() {
            self.load_tab(i);
        }
    }

    pub(crate) fn navigate(&mut self, ti: usize, path: PathBuf) {
        self.navigate_selecting(ti, path, None);
    }

    /// Opens a folder in tab `ti` and, once it is shown, selects `select` in it. Nothing here waits for
    /// the disk: a network share or a WSL distribution that is still starting could take seconds, so the
    /// path is looked at on a worker thread and the tab changes when the answer arrives (`finish_navigate`).
    pub(crate) fn navigate_selecting(&mut self, ti: usize, path: PathBuf, select: Option<PathBuf>) {
        // A path through a .zip ("C:\\data\\a.zip\\docs") is opened like any folder: the zip is
        // unpacked once (in the background) and the tab then shows the unpacked folder.
        let path = match resolve_archive_path(&path) {
            ArchivePath::Real(p) => p,
            ArchivePath::Extract { zip, rel } => {
                self.open_archive(ti, zip, rel);
                return;
            }
        };
        let tab = &mut self.win.tabs[ti];
        tab.nav_seq += 1;
        tab.loading = true;
        let (id, seq) = (tab.id, tab.nav_seq);
        let (tx, ui) = (self.core.msg_tx.clone(), self.core.waker.clone());
        std::thread::spawn(move || {
            let result = resolve_folder(&path);
            let _ = tx.send(AppMsg::NavResolved { tab: id, seq, requested: path, select, result });
            ui.wake();
        });
    }

    /// The tab's target folder turned out to be `path` (see `navigate_selecting`): shows it.
    pub(crate) fn finish_navigate(&mut self, ti: usize, path: PathBuf, select: Option<PathBuf>) {
        let tab = &mut self.win.tabs[ti];
        if matches!(tab.page, Some(Page::Settings | Page::Licenses)) {
            tab.back.clear();
            tab.fwd.clear();
        } else if path == tab.dir && tab.trash.is_none() {
            // The Recycle Bin is a view *over* `tab.dir`, so asking for that same folder while in the
            // bin must leave the bin; otherwise the same folder is simply ignored.
            tab.loading = false;
            tab.address = display_path(&path).display().to_string();
            if let Some(p) = select {
                tab.select_only(p);
            }
            return;
        } else {
            let cur = tab.place();
            tab.back.push(cur);
            tab.fwd.clear();
        }
        self.set_dir(ti, path);
        if let Some(p) = select {
            self.win.tabs[ti].select_only(p);
        }
    }

    /// Shows `place` without touching history (Back/Forward).
    pub(crate) fn goto_place(&mut self, ti: usize, place: Place) {
        match place {
            Place::Folder(p) => self.set_dir(ti, p), // clears page and trash already
            Place::Page(page) => self.show_page(ti, page),
        }
    }

    /// User-initiated: sidebar, toolbar, address bar, Up. Always replaces what tab `ti` shows.
    pub(crate) fn open_place(&mut self, ti: usize, place: Place) {
        match place {
            Place::Folder(p) => self.navigate(ti, p), // async; finish_navigate records history
            Place::Page(page) => {
                let tab = &mut self.win.tabs[ti];
                if tab.page == Some(page) {
                    return;
                }
                let cur = tab.place();
                tab.back.push(cur);
                tab.fwd.clear();
                self.show_page(ti, page);
            }
        }
    }

    pub(crate) fn show_page(&mut self, ti: usize, page: Page) {
        let tab = &mut self.win.tabs[ti];
        tab.nav_seq += 1; // a folder lookup still in flight must not replace the page when it arrives
        tab.loading = false;
        tab.page = Some(page);
        tab.search.clear();
        tab.clear_selection();
        tab.rename = None;
        tab.editing_addr = false;
        tab.view_dirty = true;
        match page {
            Page::RecycleBin => {
                if tab.trash.is_none() {
                    tab.trash = Some(TrashState::new());
                }
                self.load_trash(ti);
            }
            Page::Settings => {
                tab.trash = None;
                self.core.default_on = default_app::is_default();
                self.core.default_error = None;
            }
            Page::Licenses => tab.trash = None,
        }
        if ti == self.win.active {
            self.on_active_changed();
        }
    }

    pub(crate) fn set_dir(&mut self, ti: usize, path: PathBuf) {
        let path = clean_path(&path);
        let tab = &mut self.win.tabs[ti];
        tab.address = display_path(&path).display().to_string();
        tab.dir = path;
        tab.search.clear();
        tab.clear_selection();
        tab.rename = None;
        tab.editing_addr = false;
        tab.trash = None;
        tab.page = None;
        tab.nav_seq += 1; // an answer still on its way for an earlier request is out of date now
        tab.entries.clear();
        tab.lookup.clear();
        tab.view.clear();
        tab.view_dirty = true;
        self.load_tab(ti);
        if ti == self.win.active {
            self.on_active_changed();
        }
    }

    pub(crate) fn on_active_changed(&mut self) {
        // The folder of the window that changed goes last: it counts as the current one.
        let serial = self.win.serial;
        let mut dirs: Vec<PathBuf> = Vec::new();
        for w in self.each_window().into_iter().filter(|w| w.serial != serial).chain(self.each_window().into_iter().filter(|w| w.serial == serial)) {
            let d = w.tabs[w.active].dir.clone();
            if !dirs.contains(&d) {
                dirs.push(d);
            }
        }
        if let Some(w) = &self.core.watcher {
            w.watch(dirs);
        }
        self.start_indexing(false);
    }

    /// Indices of the tabs currently on screen (all tiled tabs, or just the active one).
    pub(crate) fn visible_tabs(&self) -> Vec<usize> {
        self.win.visible_tabs()
    }

    /// Starts the folder-size scan for every visible tab of every window, as one scan: a folder two windows
    /// show is walked once, and one window navigating never cancels what another one waits for. A scan for
    /// exactly the same set of folders is not restarted unless `force` is set (restarting throws away the
    /// work already done).
    pub(crate) fn start_indexing(&mut self, force: bool) {
        // Prevent issues for compilation regarding the option state of the indexer.
        let Some(indexer) = self.core.indexer.as_ref() else {
            // Turning sizes back on always starts a fresh scan.
            self.core.index_sig = None;
            return;
        };
        let mut paths = Vec::new();
        let mut acc = 0u64;
        let mut missing = false; // some visible folder still has no size
        for w in windows_mut(self.cur, &mut self.win, &mut self.windows) {
          for ti in w.visible_tabs() {
            let tab = &mut w.tabs[ti];
            let mut h = DefaultHasher::new();
            tab.id.hash(&mut h);
            tab.dir.hash(&mut h);
            tab.trash.is_some().hash(&mut h);
            acc = acc.wrapping_add(h.finish());
            // Recycle Bin view: the sizes of the trashed folders, which are ordinary folders where
            // the system keeps them. These are its own rows, not the listing hidden underneath.
            if let Some(tr) = tab.trash.as_mut() {
                for r in tr.rows.iter_mut() {
                    let Some(p) = r.dir_path.clone() else { continue };
                    if is_network_path(&p) {
                        r.size_pending = false;
                        continue;
                    }
                    if r.size.is_none() {
                        match indexer.lookup(&p) {
                            Some(old) => r.set_dir_size(old, true),
                            None => missing = true,
                        }
                    }
                    let mut h = DefaultHasher::new();
                    p.hash(&mut h);
                    acc = acc.wrapping_add(h.finish().rotate_left(13));
                    paths.push((p, None)); // trashed items have no listing time to pass on
                }
                continue;
            }
            // Measuring folders over a network share would mean reading the whole share.
            if tab.page.is_some() || is_network_path(&tab.dir) {
                continue;
            }
            for e in tab.entries.iter_mut().filter(|e| e.is_dir && !e.is_link) {
                if e.dir_size.is_none() {
                    e.dir_size = indexer.lookup(&e.path);
                }
                if e.dir_size.is_none() {
                    missing = true;
                }
                let mut h = DefaultHasher::new();
                e.path.hash(&mut h);
                acc = acc.wrapping_add(h.finish().rotate_left(13));
                paths.push((e.path.clone(), e.modified));
            }
        }
        }
        // A folder two windows show is scanned once.
        let mut once = HashSet::new();
        paths.retain(|(p, _)| once.insert(p.clone()));
        let sig = (acc, paths.len());
        // The same set of folders is only skipped while a scan for it is still running, or when
        // every row already has a size. Otherwise (e.g. a folder was invalidated by the watcher and
        // its listing reloaded) rows would sit on "calculating" forever with no scan to fill them.
        if !force && self.core.index_sig == Some(sig) && (indexer.is_running() || !missing) {
            return;
        }
        self.core.index_sig = Some(sig);
        indexer.start_with(paths); // also cancels a scan that belonged to the previous folder
    }

    /// Rebuilds the Quick access list from the settings and tells the icon cache which folders
    /// have an icon of their own.
    pub(crate) fn rebuild_quick(&mut self) {
        // keep already-resolved kinds so the sidebar doesn't flicker on every rebuild
        let old: HashMap<PathBuf, bool> =
            self.core.quick.iter().map(|q| (q.path.clone(), q.is_dir)).collect();
        self.core.quick = build_quick(&self.core.settings);
        for q in &mut self.core.quick {
            if let Some(&d) = old.get(&q.path) {
                q.set_dir(d);
            }
        }
        if self.apply_quick_availability() {
            self.resolve_quick_kinds();
        }
        self.sync_special_icons();
        self.resolve_quick_kinds();
    }

    pub(crate) fn resolve_quick_kinds(&mut self) {
        for q in self.core.quick.iter().filter(|q| q.available) {
            if !self.core.probed_icons.insert(q.path.clone()) {
                continue; // already being probed
            }
            let (p, tx, ui) = (q.path.clone(), self.core.msg_tx.clone(), self.core.waker.clone());
            std::thread::spawn(move || {
                let is_dir = fs::metadata(&p).ok().map(|m| m.is_dir());
                let _ = tx.send(AppMsg::QuickKind { path: p, is_dir });
                ui.wake();
            });
        }
    }

    pub(crate) fn sync_special_icons(&mut self) {
        let mut special: Vec<PathBuf> = known_folders().into_iter().filter_map(|(_, _, p)| p).collect();
        special.extend(self.core.quick.iter().map(|q| q.path.clone()));
        #[cfg(windows)]
        set_special_folders(special);
        set_custom_folder_icons(
            self.core.settings.folder_icons.iter().map(|(k, v)| (PathBuf::from(k), PathBuf::from(v))),
        );
        // Listings that are already on screen were keyed before the change.
        for t in &mut self.win.tabs {
            for e in t.entries.iter_mut().chain(t.search_results.iter_mut()).filter(|e| e.is_dir) {
                e.icon_key = make_icon_key(true, None, &e.path);
            }
        }
    }

    pub(crate) fn quick_paths(&self) -> Vec<String> {
        self.core.quick.iter().map(|q| q.path.to_string_lossy().into_owned()).collect()
    }

    pub(crate) fn pin_quick(&mut self, p: PathBuf) {
        if self.core.quick.iter().any(|q| same_path(&q.path, &p)) || !p.exists() {
            return;
        }
        let mut list = self.quick_paths();
        list.push(p.to_string_lossy().into_owned());
        self.core.settings.quick_access = Some(list);
        self.rebuild_quick();
        save_settings(&self.core.settings);
    }

    pub(crate) fn unpin_quick(&mut self, p: &Path) {
        let list: Vec<String> = self
            .core.quick
            .iter()
            .filter(|q| !same_path(&q.path, p))
            .map(|q| q.path.to_string_lossy().into_owned())
            .collect();
        self.core.settings.quick_access = Some(list);
        self.rebuild_quick();
        save_settings(&self.core.settings);
    }

    /// Opens a folder in the active tab, a zip like a folder, any other file with its program. Looking at
    /// the path (and starting the program) happens on a worker thread, like `navigate_selecting`.
    pub(crate) fn open_path(&mut self, path: &Path) {
        let ti = self.win.active;
        let tab = &mut self.win.tabs[ti];
        tab.nav_seq += 1;
        tab.loading = true;
        let (id, seq) = (tab.id, tab.nav_seq);
        let (tx, ui, path) = (self.core.msg_tx.clone(), self.core.waker.clone(), path.to_path_buf());
        std::thread::spawn(move || {
            let fail = |msg: String, retry: Option<AdminRetry>| AppMsg::OpenFailed { tab: id, msg, retry };
            let msg = match fs::metadata(&path) {
                Ok(m) if m.is_dir() => {
                    let result = resolve_folder(&path);
                    AppMsg::NavResolved { tab: id, seq, requested: path, select: None, result }
                }
                Ok(m) if m.is_file() && has_archive_ext(&path) => AppMsg::OpenZip { tab: id, seq, path },
                Ok(_) => match open::that(&path) {
                    Ok(()) => AppMsg::OpenFailed { tab: id, msg: String::new(), retry: None }, // nothing to report
                    Err(e) => {
                        let msg = format!("Could not open file: {e}");
                        let retry = needs_elevation(&e).then(|| AdminRetry::RunFile(path.clone()));
                        fail(msg, retry)
                    }
                },
                Err(e) => fail(format!("Could not access item: {e}"), None),
            };
            let _ = tx.send(msg);
            ui.wake();
        });
    }

    // ------------------------------------------------------------------ zip archives

    /// Applies the network setting to the drive watcher and makes it poll right away.
    pub(crate) fn refresh_drives(&mut self) {
        self.core.drive_ctl.set_network(self.core.settings.network_drives);
        self.core.drive_ctl.poke();
    }

    /// Hides Quick access entries whose removable / network drive is not connected and shows them again
    /// when it is back. The pins themselves stay in the settings. Returns whether anything changed.
    pub(crate) fn apply_quick_availability(&mut self) -> bool {
        if !self.core.drives_ready {
            return false;
        }
        let network = self.core.settings.network_drives;
        let mut changed = false;
        for q in &mut self.core.quick {
            let a = path_available(&q.path, &self.core.drives, network);
            if q.available != a {
                q.available = a;
                changed = true;
            }
        }
        changed
    }
}

/// Looks at a folder path (this can take a while on a network share): the real, cleaned-up path, or the reason it
/// can't be opened. Runs on a worker thread.
fn resolve_folder(path: &Path) -> Result<PathBuf, NavError> {
    match dunce::canonicalize(path) {
        Ok(p) if p.is_dir() => Ok(clean_path(&p)), // no \\?\ / \\?\UNC\ prefix (breaks cargo, cmd, ...)
        Ok(_) => Err(NavError { msg: "That path is not a folder.".to_owned(), denied: false }),
        Err(e) => Err(NavError { msg: format!("Could not open folder: {e}"), denied: is_denied_io(&e) }),
    }
}
