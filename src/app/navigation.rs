//! Navigation, tab loading, quick access and drive refresh.

use crate::prelude::*;

impl Explorer {
    pub(crate) fn load_tab(&mut self, ti: usize) {
        let (show_hidden, fmt) = (self.settings.show_hidden, self.settings.date_format);
        let tab = &mut self.tabs[ti];
        tab.load_gen += 1;
        tab.loading = true;
        tab.error = None;
        let (id, seq, dir) = (tab.id, tab.load_gen, tab.dir.clone());
        let (tx, ui) = (self.msg_tx.clone(), self.ui.clone());
        std::thread::spawn(move || {
            let result = read_listing(&dir, show_hidden, fmt);
            let _ = tx.send(AppMsg::Listing {
                tab: id,
                seq,
                result,
            });
            ui.request_repaint();
        });
    }

    pub(crate) fn reload_all(&mut self) {
        for i in 0..self.tabs.len() {
            self.load_tab(i);
        }
    }

    pub(crate) fn navigate(&mut self, ti: usize, path: PathBuf) {
        // A path through a .zip ("C:\\data\\a.zip\\docs") is opened like any folder: the zip is
        // unpacked once (in the background) and the tab then shows the unpacked folder.
        let path = match resolve_archive_path(&path) {
            ArchivePath::Real(p) => p,
            ArchivePath::Extract { zip, rel } => {
                self.open_archive(ti, zip, rel);
                return;
            }
        };
        let path = match dunce::canonicalize(&path) {
            Ok(p) if p.is_dir() => clean_path(&p), // no \\?\ / \\?\UNC\ prefix (breaks cargo, cmd, ...)
            Ok(_) => {
                self.tabs[ti].error = Some("That path is not a folder.".to_owned());
                return;
            }
            Err(e) => {
                let msg = format!("Could not open folder: {e}");
                if is_denied_io(&e) {
                    self.admin_retry = Some((msg.clone(), AdminRetry::OpenFolder(path.clone())));
                }
                self.tabs[ti].error = Some(msg);
                return;
            }
        };
        let tab = &mut self.tabs[ti];
        let in_trash = tab.trash.is_some();
        if tab.settings {
            // Leaving the Settings page: the tab turns into a normal folder tab.
            tab.settings = false;
            tab.back.clear();
            tab.fwd.clear();
            self.set_dir(ti, path);
            return;
        }
        if path == tab.dir {
            if in_trash {
                // The Recycle Bin is a view *over* `tab.dir`, so asking for that same folder
                // (e.g. Home right after opening the bin from Home) must leave the bin instead of
                // being ignored. open_trash() already pushed `dir` onto the back stack; undo that.
                if tab.back.last() == Some(&path) {
                    tab.back.pop();
                }
                tab.fwd.clear();
                self.set_dir(ti, path);
            } else {
                tab.address = display_path(&path).display().to_string();
            }
            return;
        }
        if !in_trash {
            // (in the bin, open_trash() has already recorded the folder we came from)
            let prev = tab.dir.clone();
            tab.back.push(prev);
        }
        tab.fwd.clear();
        self.set_dir(ti, path);
    }

    pub(crate) fn set_dir(&mut self, ti: usize, path: PathBuf) {
        let path = clean_path(&path);
        let tab = &mut self.tabs[ti];
        tab.address = display_path(&path).display().to_string();
        tab.dir = path;
        tab.search.clear();
        tab.clear_selection();
        tab.rename = None;
        tab.editing_addr = false;
        tab.trash = None;
        tab.settings = false;
        tab.entries.clear();
        tab.lookup.clear();
        tab.view.clear();
        tab.view_dirty = true;
        self.load_tab(ti);
        if ti == self.active {
            self.on_active_changed();
        }
    }

    pub(crate) fn on_active_changed(&mut self) {
        let dir = self.tabs[self.active].dir.clone();
        if let Some(w) = &mut self.watcher {
            w.watch(&dir);
        }
        self.start_indexing(false);
    }

    /// Indices of the tabs currently on screen (all tiled tabs, or just the active one).
    pub(crate) fn visible_tabs(&self) -> Vec<usize> {
        if let Some(t) = &self.tiles {
            let mut ids = Vec::new();
            t.leaf_ids(&mut ids);
            let v: Vec<usize> = ids
                .iter()
                .filter_map(|id| self.tabs.iter().position(|x| x.id == *id))
                .collect();
            if !v.is_empty() {
                return v;
            }
        }
        vec![self.active]
    }

    /// Starts the folder-size scan for every visible tab. A scan for exactly the same set of
    /// folders is not restarted unless `force` is set (restarting throws away the work already done).
    pub(crate) fn start_indexing(&mut self, force: bool) {
        let mut paths = Vec::new();
        let mut acc = 0u64;
        let mut missing = false; // some visible folder still has no size
        for ti in self.visible_tabs() {
            let tab = &mut self.tabs[ti];
            let mut h = DefaultHasher::new();
            tab.id.hash(&mut h);
            tab.dir.hash(&mut h);
            acc ^= h.finish();
            // Measuring folders over a network share would mean reading the whole share.
            if tab.settings || is_network_path(&tab.dir) {
                continue;
            }
            for e in tab.entries.iter_mut().filter(|e| e.is_dir && !e.is_link) {
                if e.dir_size.is_none() {
                    e.dir_size = self.indexer.lookup(&e.path);
                }
                if e.dir_size.is_none() {
                    missing = true;
                }
                let mut h = DefaultHasher::new();
                e.path.hash(&mut h);
                acc ^= h.finish().rotate_left(13);
                paths.push(e.path.clone());
            }
        }
        let sig = (acc, paths.len());
        // The same set of folders is only skipped while a scan for it is still running, or when
        // every row already has a size. Otherwise (e.g. a folder was invalidated by the watcher and
        // its listing reloaded) rows would sit on "calculating" forever with no scan to fill them.
        if !force && self.index_sig == Some(sig) && (self.indexer.is_running() || !missing) {
            return;
        }
        self.index_sig = Some(sig);
        self.indexer.start(paths); // also cancels a scan that belonged to the previous folder
    }

    /// Rebuilds the Quick access list from the settings and tells the icon cache which folders
    /// have an icon of their own.
    pub(crate) fn rebuild_quick(&mut self) {
        self.quick = build_quick(&self.settings);
        self.apply_quick_availability();
        self.sync_special_icons();
    }

    pub(crate) fn sync_special_icons(&mut self) {
        let mut special: Vec<PathBuf> = known_folders().into_iter().filter_map(|(_, _, p)| p).collect();
        special.extend(self.quick.iter().map(|q| q.path.clone()));
        #[cfg(windows)]
        set_special_folders(special);
        set_custom_folder_icons(
            self.settings.folder_icons.iter().map(|(k, v)| (PathBuf::from(k), PathBuf::from(v))),
        );
        // Listings that are already on screen were keyed before the change.
        for t in &mut self.tabs {
            for e in t.entries.iter_mut().chain(t.search_results.iter_mut()).filter(|e| e.is_dir) {
                e.icon_key = make_icon_key(true, None, &e.path);
            }
        }
    }

    pub(crate) fn quick_paths(&self) -> Vec<String> {
        self.quick.iter().map(|q| q.path.to_string_lossy().into_owned()).collect()
    }

    pub(crate) fn pin_quick(&mut self, p: PathBuf) {
        if self.quick.iter().any(|q| same_path(&q.path, &p)) || !p.is_dir() {
            return;
        }
        let mut list = self.quick_paths();
        list.push(p.to_string_lossy().into_owned());
        self.settings.quick_access = Some(list);
        self.rebuild_quick();
        save_settings(&self.settings);
    }

    pub(crate) fn unpin_quick(&mut self, p: &Path) {
        let list: Vec<String> = self
            .quick
            .iter()
            .filter(|q| !same_path(&q.path, p))
            .map(|q| q.path.to_string_lossy().into_owned())
            .collect();
        self.settings.quick_access = Some(list);
        self.rebuild_quick();
        save_settings(&self.settings);
    }

    pub(crate) fn open_path(&mut self, path: &Path) {
        // A .zip opens like a folder: its contents are shown in the same tab.
        if has_zip_ext(path) && path.is_file() {
            self.navigate(self.active, path.to_path_buf());
            return;
        }
        match fs::metadata(path) {
            Ok(m) if m.is_dir() => self.navigate(self.active, path.to_path_buf()),
            Ok(_) => {
                if let Err(e) = open::that(path) {
                    let msg = format!("Could not open file: {e}");
                    if needs_elevation(&e) {
                        self.admin_retry = Some((msg.clone(), AdminRetry::RunFile(path.to_path_buf())));
                    }
                    self.tabs[self.active].error = Some(msg);
                }
            }
            Err(e) => self.tabs[self.active].error = Some(format!("Could not access item: {e}")),
        }
    }

    // ------------------------------------------------------------------ zip archives

    /// Applies the network setting to the drive watcher and makes it poll right away.
    pub(crate) fn refresh_drives(&mut self) {
        self.drive_ctl.set_network(self.settings.network_drives);
        self.drive_ctl.poke();
    }

    /// Hides Quick access entries whose removable / network drive is not connected and shows them again
    /// when it is back. The pins themselves stay in the settings. Returns whether anything changed.
    pub(crate) fn apply_quick_availability(&mut self) -> bool {
        if !self.drives_ready {
            return false;
        }
        let network = self.settings.network_drives;
        let mut changed = false;
        for q in &mut self.quick {
            let a = path_available(&q.path, &self.drives, network);
            if q.available != a {
                q.available = a;
                changed = true;
            }
        }
        changed
    }
}
