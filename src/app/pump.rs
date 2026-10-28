//! Per-frame processing of background results and system state.

use crate::prelude::*;

impl Explorer {
    /// Starts/cancels the recursive search whenever a tab's search text changes (this also
    /// covers Escape, the clear button and navigation, which all just clear `tab.search`),
    /// and drains results from the running job.
    pub(crate) fn pump_search(&mut self, ctx: &egui::Context) {
        let (show_hidden, fmt) = (self.settings.show_hidden, self.settings.date_format);
        for t in &mut self.tabs {
            let q = if t.special() {
                String::new()
            } else {
                t.search.trim().to_lowercase()
            };
            if q != t.search_query || t.chosen_search_scope !=  t.current_search_scope{
                t.search_query = q;
                t.current_search_scope = t.chosen_search_scope;
                on_search_changed(t, show_hidden, fmt, ctx);
            }
            poll_search(t, &self.indexer);
        }
    }

    pub(crate) fn pump_messages(&mut self) {
        // Every pane on screen needs sizes, not just the active tab.
        let visible_ids: Vec<_> = self.visible_tabs().into_iter().map(|i| self.tabs[i].id).collect();
        let mut reindex = false;
        let mut reindex_force = false;

        while let Ok(msg) = self.msg_rx.try_recv() {
            match msg {
                AppMsg::Listing { tab, seq, result } => {
                    if let Some(t) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.id == tab && t.load_gen == seq)
                    {
                        t.loading = false;
                        match result {
                            Ok(mut entries) => {
                                for e in entries.iter_mut().filter(|e| e.is_dir && !e.is_link) {
                                    e.dir_size = self.indexer.lookup(&e.path);
                                }
                                t.lookup = entries
                                    .iter()
                                    .enumerate()
                                    .map(|(i, e)| (e.path.clone(), i))
                                    .collect();
                                t.entries = entries;
                                t.view_dirty = true;
                                if let Some(p) = t.pending_rename.take() {
                                    if let Some(&i) = t.lookup.get(&p) {
                                        t.rename = Some(RenameState {
                                            text: t.entries[i].name.clone(),
                                            is_dir: t.entries[i].is_dir,
                                            path: p.clone(),
                                            focus_pending: true,
                                        });
                                        t.select_only(p);
                                    }
                                }
                                if !t.searching() {
                                    let lookup = &t.lookup;
                                    t.selected.retain(|p| lookup.contains_key(p));
                                    if t.anchor.as_ref().is_some_and(|a| !lookup.contains_key(a)) {
                                        t.anchor = None;
                                    }
                                }
                                // `view` holds indices into `entries`; they are stale as soon as the
                                // list changes. Rebuild now, before anything else touches them
                                // (deleting a file used to leave an out-of-range index behind).
                                rebuild_view(t);
                            }
                            Err(e) => {
                                t.entries.clear();
                                t.lookup.clear();
                                t.view.clear();
                                // A folder that needs administrator rights can be opened in an
                                // elevated window.
                                if fs::read_dir(&t.dir).is_err_and(|x| is_denied_io(&x)) {
                                    self.admin_retry = Some((e.clone(), AdminRetry::OpenFolder(t.dir.clone())));
                                }
                                t.error = Some(e);
                            }
                        }
                        if visible_ids.contains(&t.id) {
                            reindex = true;
                        }
                    }
                }
                AppMsg::Trash { tab, seq, result } => {
                    if let Some(t) = self.tabs.iter_mut().find(|t| t.id == tab) {
                        if let Some(tr) = t.trash.as_mut() {
                            if tr.seq == seq {
                                tr.loading = false;
                                tr.selected.clear();
                                tr.anchor = None;
                                match result {
                                    Ok(rows) => {
                                        tr.rows = rows;
                                        rebuild_trash_view(tr, &t.search);
                                        t.view_dirty = false;
                                    }
                                    Err(e) => {
                                        tr.rows.clear();
                                        tr.view.clear();
                                        tr.error = Some(e);
                                    }
                                }
                            }
                        }
                    }
                }
                AppMsg::Drives(mut d) => {
                    // A scan that started before network drives were switched off may still deliver them.
                    if !self.settings.network_drives {
                        d.retain(|x| x.kind != DriveKind::Network);
                    }
                    #[cfg(windows)]
                    set_special_drives(d.iter().map(|x| x.mount_point.clone()));
                    self.drives = d;
                    self.drives_ready = true;
                    self.apply_quick_availability();
                }
                AppMsg::Job(id, result) => {
                    self.ops.retain(|o| o.id != id);
                    let active = self.active;
                    if let Err(e) = result {
                        match self.tabs[active].trash.as_mut() {
                            Some(tr) => tr.error = Some(e),
                            None => self.tabs[active].error = Some(e),
                        }
                    }
                    // Refresh every pane on screen: a paste may have landed in a neighbouring pane.
                    for ti in self.visible_tabs() {
                        if self.tabs[ti].trash.is_some() {
                            self.load_trash(ti);
                        } else {
                            self.load_tab(ti);
                            // forces pump_search to rerun an active search (results may be stale)
                            self.tabs[ti].search_query.clear();
                        }
                    }
                }
                AppMsg::ArchiveReady { tab, zip, target, result } => {
                    self.extracting.remove(&zip);
                    self.job_finished();
                    if let Some(ti) = self.tabs.iter().position(|t| t.id == tab) {
                        match result {
                            Ok(()) => self.navigate(ti, target),
                            Err(e) => self.tabs[ti].error = Some(e),
                        }
                    }
                }
                AppMsg::Made { tab, created, errors, rename, open } => {
                    self.job_finished();
                    // "Extract all" from inside an opened zip: show the result (the tab only shows a
                    // temporary unpacked copy, so refreshing it would tell the user nothing).
                    let inside_zip = self
                        .tabs
                        .iter()
                        .position(|t| t.id == tab)
                        .filter(|&ti| archive_of(&self.tabs[ti].dir).is_some());
                    if let (Some(ti), Some(first)) = (inside_zip, created.first()) {
                        self.navigate(ti, first.clone());
                        if !errors.is_empty() {
                            self.tabs[ti].error = Some(errors.join("\n"));
                        }
                        continue;
                    }
                    if let Some(t) = self.tabs.iter_mut().find(|t| t.id == tab) {
                        if rename {
                            t.pending_rename = created.first().cloned();
                        } else if !created.is_empty() {
                            t.selected = created.iter().cloned().collect();
                            t.anchor = created.first().cloned();
                        }
                    }
                    for ti in self.visible_tabs() {
                        if self.tabs[ti].trash.is_none() {
                            self.load_tab(ti);
                            self.tabs[ti].search_query.clear(); // rerun an active search
                        }
                    }
                    // (after load_tab, which clears the tab's error)
                    if !errors.is_empty() {
                        if let Some(t) = self.tabs.iter_mut().find(|t| t.id == tab) {
                            t.error = Some(errors.join("\n"));
                        }
                    }
                    if let (true, Some(first)) = (open, created.first()) {
                        self.new_tab(first.clone());
                    }
                }
                AppMsg::AdminOffer { msg, retry } => self.admin_retry = Some((msg, retry)),
                AppMsg::Stats(seq, stats) => {
                    if let Some(p) = &mut self.props {
                        if p.stats_gen == seq {
                            p.stats = Some(stats);
                        }
                    }
                }
            }
        }

        while let Ok(IndexMsg::Size(path, size)) = self.idx_rx.try_recv() {
            let Some(parent) = path.parent() else {
                continue;
            };
            for t in &mut self.tabs {
                if t.dir == parent {
                    if let Some(&i) = t.lookup.get(&path) {
                        t.entries[i].dir_size = Some(size);
                        if t.sort_col == SortColumn::Size {
                            t.view_dirty = true;
                        }
                    }
                }
                if let Some(i) = t.search_index(&path) {
                    if let Some(e) = t.search_results.get_mut(i) {
                        e.dir_size = Some(size);
                    }
                    if t.searching() && t.sort_col == SortColumn::Size {
                        t.view_dirty = true;
                    }
                }
            }
        }

        // Live updates from the file watcher.
        let changed = self.watcher.as_ref().map(|w| w.take()).unwrap_or_default();
        if !changed.is_empty() {
            let dir = self.tabs[self.active].dir.clone();
            for p in changed {
                if p.as_os_str().is_empty() {
                    // Too many changes to list: refresh everything, and forget the cached sizes
                    // of this folder tree (a forced rescan would otherwise just reuse them).
                    self.indexer.invalidate_tree(&dir);
                    self.list_debounce.poke();
                    self.size_debounce.poke();
                    continue;
                }
                if p.starts_with(&self.index_dir) {
                    continue; // our own cache file
                }
                self.indexer.invalidate(&p);
                if p.parent() == Some(dir.as_path()) || p == dir {
                    self.list_debounce.poke();
                } else if p.starts_with(&dir) {
                    self.size_debounce.poke();
                }
            }
        }

        // Folders other windows dragged over to us arrive as new tabs.
        for dir in self.peers.take_inbox() {
            if dir.is_dir() {
                self.new_tab(dir);
                self.ui.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
        }
        if self
            .list_debounce
            .ready(Duration::from_millis(250), Duration::from_millis(1500))
        {
            self.load_tab(self.active);
        }
        // Long quiet period for sizes: a download writing to a folder must not trigger a rescan every few ms.
        if self
            .size_debounce
            .ready(Duration::from_millis(1500), Duration::from_secs(10))
        {
            if self.indexer.is_running() {
                // Restarting now would cancel the running scan and, under constant file activity,
                // none would ever finish. Look again once it has had some time.
                self.size_debounce.poke();
            } else {
                reindex = true;
                reindex_force = true; // something below the folder changed: sizes really are stale
            }
        }
        if self.list_debounce.pending() || self.size_debounce.pending() {
            self.ui.request_repaint_after(Duration::from_millis(250));
        }
        // The drive watcher thread polls on its own; it only needs to know whether the window is in use.
        let focused = self.ui.input(|i| i.focused);
        self.drive_ctl.set_focused(focused);
        if self.indexer.is_dirty() {
            if self.last_save.elapsed() > Duration::from_secs(15) {
                self.indexer.save_if_dirty(&self.index_dir, false);
                self.last_save = Instant::now();
            } else {
                self.ui.request_repaint_after(Duration::from_secs(16));
            }
        }
        if reindex {
            let force = reindex_force || std::mem::take(&mut self.reindex_force);
            self.start_indexing(force);
        }
    }

    // ------------------------------------------------------------------ system integration

    pub(crate) fn sync_system(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        if self.last_poll.elapsed() > Duration::from_secs(2) && ctx.input(|i| i.focused) {
            self.last_poll = Instant::now();
            self.text_scale = system_text_scale();
            self.sys_accent = system_accent();
            self.transparency = system_transparency();
        }

        let accent = self.get_accent();

        //Calls function to determine font size.
        let font = self.settings.effective_font_size(self.text_scale);
        self.m = Metrics::new(font, ctx.pixels_per_point());
        if self.applied_style != Some((font, accent)) {
            apply_style(ctx, &self.m, accent);
            self.applied_style = Some((font, accent));
        }

        // Checks and changes the font size based on system accessibility settings if activated.
        if self.text_scale_checked.elapsed() >= Duration::from_secs(1) {
            self.text_scale_checked = Instant::now();
            let s = system_text_scale();
            if (s - self.text_scale).abs() > f32::EPSILON {
                self.text_scale = s;
                ctx.request_repaint();
            }
        }

        // Schedules restart if custom frame has been toggled on/off
        if self.require_restart{
            self.require_restart = false;
            if let Some(rect) = ctx.input(|i| i.viewport().outer_rect) {
                let pos: egui::Pos2 = rect.min;
                let (x, y) = (pos.x, pos.y);
                spawn_window(&self.tabs[self.active].dir, Some((x, y)));
            }else{
                spawn_window(&self.tabs[self.active].dir, Some((0.0, 0.0)));
            }
            hide_window(frame);
            save_settings(&self.settings);
            self.peers.shutdown();
            self.indexer.save_if_dirty(&self.index_dir, true);
            remove_archive_cache();
            finish_journal();
            terminate_now();
        }

        let theme = ctx.theme();

        // wanted_transparency is the transparency set by the user in the settings.
        // If transparency is disabled or None, wanted_transparency is None.
        let wanted_transparency: Option<TransparencyEffect> = if self.transparency {
            match self.settings.transparency_effect {
                TransparencyEffect::Mica => Some(TransparencyEffect::Mica),
                TransparencyEffect::Acrylic => Some(TransparencyEffect::Acrylic),
                _ => None,
            }
        } else {
            None
        };

        // True if the theme was changed.
        let changed_theme = if self.settings.theme != self.theme_choice {
            self.theme_choice = self.settings.theme;
            true
        } else {
            false
        };

        // Sets the cache size for folder indexing to the new value, if it was changed
        // If cache size hasn't been set yet, set it to the settings value.
        let cache_change = Some(self.settings.index_ram) != self.cache_size;
        let cache_unlimited_change = self.unlimited_cache != self.settings.unlimited_cache;
        if  cache_change || cache_unlimited_change {
            if cache_change {
                self.cache_size = Some(self.settings.index_ram);
            }
            if cache_unlimited_change {
                self.unlimited_cache = self.settings.unlimited_cache;
            }
            self.indexer.set_limit_mb(self.cache_size.expect("The cache size will always be set."), self.unlimited_cache);
        }

        let changed_backdrop = if self.backdrop_transparency != wanted_transparency {
            self.backdrop_transparency = wanted_transparency;
            true
        }else {
            false
        };
        self.pal = Palette::new(
            theme,
            accent,
            wanted_transparency.unwrap_or(TransparencyEffect::None),
        );
        // Re-apply a few times shortly after startup to beat the DWM race.
        const RETRY_AT_MS: [u64; 4] = [150, 600, 1500, 3000];

        let mut force = false;
        if let Some(_effect) = wanted_transparency {
            if let Some(&t) = RETRY_AT_MS.get(self.backdrop_retries) {
                let elapsed = self.backdrop_start.elapsed().as_millis() as u64;
                if elapsed >= t {
                    self.backdrop_retries += 1;
                    force = true;
                } else {
                    // egui is reactive: without this, no frame runs to hit the next deadline
                    ctx.request_repaint_after(Duration::from_millis(t - elapsed));
                }
            }
        }

        // Redraw the backdrop both when it or the theme changes.
        if changed_backdrop || changed_theme {
            clear_window_transparency_effects(frame);
            if let Some(effect) = wanted_transparency {
                apply_backdrop(frame, self.pal.dark, effect);
                nudge_frame(frame);
            }
            ctx.request_repaint();
        } else if force {
            clear_window_transparency_effects(frame);
            if let Some(effect) = wanted_transparency {
                apply_backdrop(frame, self.pal.dark, effect);
                nudge_frame(frame);
            }
            ctx.request_repaint();
        }
    }

    pub(crate) fn get_accent(&mut self, ) -> Accent{
        let accent = if self.settings.use_system_accent {
        self.sys_accent.unwrap_or_else(Accent::fallback)
        } else {
        let c = self.settings.custom_accent;
        Accent::from_base(Color32::from_rgb(c[0], c[1], c[2]))
        };
        accent
    }
}
