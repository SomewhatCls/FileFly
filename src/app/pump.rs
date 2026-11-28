//! Per-frame processing of background results and system state.

use crate::peers::Handover;
use crate::prelude::*;

impl Explorer {
    /// Starts/cancels the recursive search whenever a tab's search text changes (this also
    /// covers Escape, the clear button and navigation, which all just clear `tab.search`),
    /// and drains results from the running job.
    pub(crate) fn pump_search(&mut self) {
        let (show_hidden, fmt) = (self.core.settings.show_hidden, self.core.settings.date_format);
        for t in &mut self.win.tabs {
            let q = if t.special() {
                String::new()
            } else {
                t.search.trim().to_lowercase()
            };
            if q != t.search_query || t.chosen_search_scope !=  t.current_search_scope{
                t.search_query = q;
                t.current_search_scope = t.chosen_search_scope;
                on_search_changed(t, show_hidden, fmt, &self.core.waker);
            }
            poll_search(t, self.core.indexer.as_ref());
        }
    }

    /// Handles one background message in the window that owns its tab (or in the focused window when it
    /// belongs to no tab). `self.win` is that window.
    fn handle_msg(&mut self, msg: AppMsg) {
        // Every pane on screen needs sizes, not just the active tab.
        let visible_ids: Vec<_> = self.visible_tabs().into_iter().map(|i| self.win.tabs[i].id).collect();
            match msg {
                AppMsg::Listing { tab, seq, result } => {
                    if let Some(t) = self
                        .win.tabs
                        .iter_mut()
                        .find(|t| t.id == tab && t.load_gen == seq)
                    {
                        t.loading = false;
                        match result {
                            Ok(mut entries) => {
                                for e in entries.iter_mut().filter(|e| e.is_dir && !e.is_link) {
                                    e.dir_size = self.core.indexer.as_ref().and_then(|ix| ix.lookup(&e.path));
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
                                    self.win.admin_retry = Some((e.clone(), AdminRetry::OpenFolder(t.dir.clone())));
                                }
                                t.error(e);
                            }
                        }
                        if visible_ids.contains(&t.id) {
                            self.core.reindex_wanted = true;
                        }
                    }
                }
                AppMsg::Trash { tab, seq, result } => {
                    if let Some(t) = self.win.tabs.iter_mut().find(|t| t.id == tab) {
                        if let Some(tr) = t.trash.as_mut() {
                            if tr.seq == seq {
                                tr.loading = false;
                                tr.selected.clear();
                                tr.anchor = None;
                                match result {
                                    Ok(rows) => {
                                        tr.rows = rows;
                                        index_trash_dirs(tr);
                                        // Folder sizes: the old value from the index at once, with a
                                        // spinner; the scan (see `start_indexing`) refreshes it.
                                        if let Some(ix) = self.core.indexer.as_ref() {
                                            for r in tr.rows.iter_mut().filter(|r| r.dir_path.is_some()) {
                                                match r.dir_path.as_deref().and_then(|p| ix.lookup(p)) {
                                                    Some(old) => r.set_dir_size(old, true),
                                                    None => r.size_pending = true,
                                                }
                                            }
                                            if visible_ids.contains(&t.id) {
                                                self.core.reindex_wanted = true;
                                            }
                                        }
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
                    if !self.core.settings.network_drives {
                        d.retain(|x| x.kind != DriveKind::Network);
                    }
                    #[cfg(windows)]
                    set_special_drives(d.iter().map(|x| x.mount_point.clone()));
                    self.core.drives = d;
                    self.core.drives_ready = true;
                    if self.apply_quick_availability() {
                        self.resolve_quick_kinds();
                    }
                }
                AppMsg::Job(id, result) => {
                    self.core.ops.retain(|o| o.id != id);
                    let active = self.win.active;
                    if let Err(e) = result {
                        match self.win.tabs[active].trash.as_mut() {
                            Some(tr) => tr.error = Some(e),
                            None => self.win.tabs[active].error(e),
                        }
                    }
                    // Refresh every pane on screen: a paste may have landed in a neighbouring pane.
                    for ti in self.visible_tabs() {
                        if self.win.tabs[ti].trash.is_some() {
                            self.load_trash(ti);
                        } else {
                            self.load_tab(ti);
                            // forces pump_search to rerun an active search (results may be stale)
                            self.win.tabs[ti].search_query.clear();
                        }
                    }
                }
                AppMsg::NavResolved { tab, seq, requested, select, result } => {
                    let Some(ti) = self.win.tabs.iter().position(|t| t.id == tab && t.nav_seq == seq) else { return };
                    match result {
                        Ok(path) => self.finish_navigate(ti, path, select),
                        Err(e) => {
                            self.win.tabs[ti].loading = false;
                            if e.denied {
                                self.win.admin_retry = Some((e.msg.clone(), AdminRetry::OpenFolder(requested)));
                            }
                            self.win.tabs[ti].error(e.msg);
                        }
                    }
                }
                AppMsg::OpenZip { tab, seq, path } => {
                    if let Some(ti) = self.win.tabs.iter().position(|t| t.id == tab && t.nav_seq == seq) {
                        self.navigate(ti, path);
                    }
                }
                AppMsg::OpenFailed { tab, msg, retry } => {
                    if let Some(ti) = self.win.tabs.iter().position(|t| t.id == tab) {
                        self.win.tabs[ti].loading = false;
                        if !msg.is_empty() {
                            if let Some(retry) = retry {
                                self.win.admin_retry = Some((msg.clone(), retry));
                            }
                            self.win.tabs[ti].error(msg);
                        }
                    }
                }
                AppMsg::ArchiveReady { tab, zip, target, result } => {
                    self.core.extracting.remove(&zip);
                    self.job_finished();
                    if let Some(ti) = self.win.tabs.iter().position(|t| t.id == tab) {
                        match result {
                            Ok(()) => self.navigate(ti, target),
                            Err(e) => self.win.tabs[ti].error(e),
                        }
                    }
                }
                AppMsg::Made { op, tab, created, errors, rename, open } => {
                    self.core.ops.retain(|o| o.id != op);
                    // "Extract all" from inside an opened zip: show the result (the tab only shows a
                    // temporary unpacked copy, so refreshing it would tell the user nothing).
                    let inside_zip = self
                        .win.tabs
                        .iter()
                        .position(|t| t.id == tab)
                        .filter(|&ti| archive_of(&self.win.tabs[ti].dir).is_some());
                    if let (Some(ti), Some(first)) = (inside_zip, created.first()) {
                        self.navigate(ti, first.clone());
                        if !errors.is_empty() {
                            self.win.tabs[ti].error(errors.join("\n"));
                        }
                        return;
                    }
                    if let Some(t) = self.win.tabs.iter_mut().find(|t| t.id == tab) {
                        if rename {
                            t.pending_rename = created.first().cloned();
                        } else if !created.is_empty() {
                            t.selected = created.iter().cloned().collect();
                            t.anchor = created.first().cloned();
                        }
                    }
                    for ti in self.visible_tabs() {
                        if self.win.tabs[ti].trash.is_none() {
                            self.load_tab(ti);
                            self.win.tabs[ti].search_query.clear(); // rerun an active search
                        }
                    }
                    // (after load_tab, which clears the tab's error)
                    if !errors.is_empty() {
                        if let Some(t) = self.win.tabs.iter_mut().find(|t| t.id == tab) {
                            t.error(errors.join("\n"));
                        }
                    }
                    if let (true, Some(first)) = (open, created.first()) {
                        self.new_tab(Place::from_path(first.clone()));
                    }
                }
                AppMsg::ExtractStaged { op, tab, plan, stagings, errors, open } => {
                    self.extract_staged(op, tab, plan, stagings, errors, open);
                }
                AppMsg::AdminOffer { msg, retry } => self.win.admin_retry = Some((msg, retry)),
                AppMsg::Stats(seq, stats) => {
                    if let Some(p) = &mut self.win.props {
                        if p.stats_gen == seq {
                            p.stats = Some(stats);
                        }
                    }
                }
                AppMsg::PropsMeta { id, meta } => self.props_meta_arrived(id, meta),
                AppMsg::UpdateChecked { announce, result } => self.update_checked(announce, result),
                AppMsg::ReleaseNotes(result) => {
                    // (ignored when the popup was closed while loading)
                    if self.win.whats_new.is_some() {
                        self.win.whats_new = Some(match result {
                            Ok((version, notes)) => WhatsNew::Ready { version, notes },
                            Err(e) => WhatsNew::Failed(e),
                        });
                    }
                }
                AppMsg::ShortcutTarget { tab, result } => self.shortcut_resolved(tab, result),
                AppMsg::QuickKind { path, is_dir } => {
                    self.core.probed_icons.remove(&path);
                    if let Some(d) = is_dir {
                        let mut changed = false;
                        for q in &mut self.core.quick {
                            if same_path(&q.path, &path) && q.is_dir != d {
                                q.set_dir(d);
                                changed = true;
                            }
                        }
                        if changed {
                            self.sync_special_icons();
                        }
                    }
                }
            }
    }

    /// Hands a message to the window that owns its tab. A job that ended may have changed what other
    /// windows show too, so they reload their panes as well.
    fn route_msg(&mut self, msg: AppMsg) {
        let everywhere = matches!(msg, AppMsg::Job(..) | AppMsg::Made { .. });
        let owner = msg.tab().and_then(|t| self.tab_owner(t));
        let i = owner.or_else(|| self.window_index(self.focus)).unwrap_or(0);
        self.with_window(i, |ex| ex.handle_msg(msg));
        if everywhere {
            for j in (0..self.windows.len()).filter(|&j| j != i) {
                self.with_window(j, |ex| ex.refresh_visible_tabs());
            }
        }
    }

    /// Reloads every pane on screen of the window (a paste may have landed in a neighbouring pane).
    fn refresh_visible_tabs(&mut self) {
        for ti in self.visible_tabs() {
            if self.win.tabs[ti].trash.is_some() {
                self.load_trash(ti);
            } else {
                self.load_tab(ti);
                // forces pump_search to rerun an active search (results may be stale)
                self.win.tabs[ti].search_query.clear();
            }
        }
    }

    /// Per-frame processing of everything background threads reported, for all windows: messages go to the
    /// window that owns the tab, folder sizes to every tab that shows them, file changes to every window that
    /// shows the folder. Runs once per frame, outside any window (`cur` is None).
    pub(crate) fn pump_core(&mut self) {
        while let Ok(msg) = self.core.msg_rx.try_recv() {
            self.route_msg(msg);
        }
        let focus = self.window_index(self.focus).unwrap_or(0);
        self.with_window(focus, |ex| ex.pump_staging());

        let sizes: Vec<IndexMsg> = std::iter::from_fn(|| self.core.idx_rx.try_recv().ok()).collect();
        let changed = self.core.watcher.as_ref().map(|w| w.take()).unwrap_or_default();
        // The index forgets what changed once, however many windows show it.
        for p in &changed {
            let Some(ix) = &self.core.indexer else { break };
            if p.as_os_str().is_empty() {
                let dirs: Vec<PathBuf> = self.each_window().iter().map(|w| w.tabs[w.active].dir.clone()).collect();
                dirs.iter().for_each(|d| ix.invalidate_tree(d));
            } else if !p.starts_with(&self.core.index_dir) {
                ix.invalidate(p);
            }
        }
        for i in 0..self.windows.len() {
            self.with_window(i, |ex| {
                ex.pump_search();
                ex.apply_index_msgs(&sizes);
                ex.apply_changes(&changed);
            });
        }

        // What other processes handed over: a tab dragged from one of their windows arrives as a new tab, a folder
        // opened from outside (see `peers::forward_open`) as a window of its own.
        for handover in self.core.peers.take_inbox() {
            match handover {
                Handover::Tab(dir) if dir.is_dir() => {
                    let i = self.window_index(self.focus).unwrap_or(0);
                    self.with_window(i, |ex| {
                        ex.new_tab(Place::from_path(dir));
                        ex.win.ui.send_viewport_cmd_to(ex.win.vid, egui::ViewportCommand::Focus);
                    });
                }
                Handover::Window(dir) if dir.is_dir() => self.open_window(dir, None),
                _ => {}
            }
        }
        let mut reindex_force = false;
        for i in 0..self.windows.len() {
            self.with_window(i, |ex| {
                if ex.win.list_debounce.ready(Duration::from_millis(250), Duration::from_millis(1500)) {
                    ex.load_tab(ex.win.active);
                }
                // Long quiet period for sizes: a download writing to a folder must not trigger a rescan every few ms.
                if ex.win.size_debounce.ready(Duration::from_millis(1500), Duration::from_secs(10)) {
                    if ex.core.indexer.as_ref().is_some_and(|ix| ix.is_running()) {
                        // Restarting now would cancel the running scan and, under constant file activity,
                        // none would ever finish. Look again once it has had some time.
                        ex.win.size_debounce.poke();
                    } else {
                        ex.core.reindex_wanted = true;
                        reindex_force = true; // something below the folder changed: sizes really are stale
                    }
                }
                if ex.win.list_debounce.pending() || ex.win.size_debounce.pending() {
                    ex.win.ui.request_repaint_after(Duration::from_millis(250));
                }
            });
        }
        // The background threads poll on their own; they only need to know whether any window is in use.
        let focused = self.each_window().iter().any(|w| self.ctx.input_for(w.vid, |i| i.focused));
        self.core.drive_ctl.set_focused(focused);
        if let Some(ix) = &self.core.indexer {
            ix.set_focused(focused);
        }
        if self.core.indexer.as_ref().is_some_and(|ix| ix.is_dirty()) {
            if self.core.last_save.elapsed() > Duration::from_secs(15) {
                if let Some(ix) = &self.core.indexer {
                    ix.save_if_dirty(false);
                }
                self.core.last_save = Instant::now();
            } else {
                self.ctx.request_repaint_after(Duration::from_secs(16));
            }
        }
        if std::mem::take(&mut self.core.reindex_wanted) {
            let force = reindex_force || std::mem::take(&mut self.core.reindex_force);
            self.start_indexing(force);
        }
    }

    /// Folder sizes from the indexer, for the tabs of this window that show them.
    fn apply_index_msgs(&mut self, msgs: &[IndexMsg]) {
        for msg in msgs {
            // `Partial` is a running total of a scan that is still going; `Size` is the final value.
            let (path, size, partial) = match msg {
                IndexMsg::Size(p, s) => (p.clone(), *s, false),
                IndexMsg::Partial(p, s) => (p.clone(), *s, true),
            };
            let Some(parent) = path.parent() else {
                continue;
            };
            for t in &mut self.win.tabs {
                // A trashed folder in the Recycle Bin view.
                let mut in_trash = false;
                if let Some(tr) = t.trash.as_mut() {
                    if let Some(&i) = tr.dir_rows.get(&path) {
                        if let Some(r) = tr.rows.get_mut(i) {
                            // as in a normal listing: a running total never replaces a larger old value
                            let bytes = match r.size {
                                Some(old) if partial => old.max(size),
                                _ => size,
                            };
                            r.set_dir_size(bytes, partial);
                            if tr.sort_col == TrashSort::Size {
                                tr.needs_sort = true;
                            }
                            in_trash = true;
                        }
                    }
                }
                if in_trash {
                    continue;
                }
                if t.dir == parent {
                    if let Some(&i) = t.lookup.get(&path) {
                        // A running total never replaces a larger value that is already shown (the old
                        // size from the index): the row keeps it, with a spinner, until the final size
                        // arrives.
                        let shown = t.entries[i].dir_size;
                        let new = match shown {
                            Some(old) if partial => old.max(size),
                            _ => size,
                        };
                        t.entries[i].dir_size = Some(new);
                        t.entries[i].dir_partial = partial;
                        // Sorting by size: the order may have changed with this size.
                        if shown != Some(new) && t.sort_col == SortColumn::Size {
                            t.view_dirty = true;
                        }
                    }
                }
                if partial {
                    continue; // search results only show final sizes
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
    }

    /// Live updates from the file watcher: a change in the folder this window shows refreshes its listing,
    /// a change deeper below it only its folder sizes.
    fn apply_changes(&mut self, changed: &[PathBuf]) {
        let dir = self.win.tabs[self.win.active].dir.clone();
        for p in changed {
            if p.as_os_str().is_empty() {
                // Too many changes to list: refresh everything.
                self.win.list_debounce.poke();
                self.win.size_debounce.poke();
                continue;
            }
            if p.starts_with(&self.core.index_dir) {
                continue; // our own cache file
            }
            if p.parent() == Some(dir.as_path()) || *p == dir {
                self.win.list_debounce.poke();
            } else if p.starts_with(&dir) {
                self.win.size_debounce.poke();
            }
        }
    }

    // ------------------------------------------------------------------ system integration

    pub(crate) fn sync_system(&mut self, ctx: &egui::Context, native: Option<&NativeWindow>) {
        if self.core.last_poll.elapsed() > Duration::from_secs(2) && ctx.input(|i| i.focused) {
            self.core.last_poll = Instant::now();
            self.core.sys_accent = system_accent();
            self.core.transparency = system_transparency();
        }

        let accent = self.get_accent();

        // If the font size was changed or compact mode was toggled,
        // the metrics and style scaling have to be re-calculated.
        // This also happens when the accessibility scale in windows is changed,
        // which affects the pixels_per_point() return value.
        let ppp = ctx.pixels_per_point();
        let font = self.core.settings.effective_font_size(self.win.text_scale);
        self.win.m = Metrics::new(font, ppp, self.core.settings.compact);
        if self.win.applied_style != Some((font, accent, self.core.settings.compact, ppp)) {
            apply_style(ctx, &self.win.m, accent);
            self.win.applied_style = Some((font, accent, self.core.settings.compact, ppp));
        }

        // Checks and changes the font size based on system accessibility settings if activated.
        if self.win.text_scale_checked.elapsed() >= Duration::from_secs(1) {
            self.win.text_scale_checked = Instant::now();
            let s = system_text_scale();
            if (s - self.win.text_scale).abs() > f32::EPSILON {
                self.win.text_scale = s;
                ctx.request_repaint();
            }
        }

        // Schedules restart for settings requiring it.
        if std::mem::take(&mut self.core.refresh_ui) {
            self.refresh_windows();
        }

        let theme = ctx.theme();

        // wanted_transparency is the transparency set by the user in the settings.
        // If transparency is disabled or None, wanted_transparency is None.
        let wanted_transparency = (self.core.transparency
            && self.core.settings.transparency_effect.or_supported() != TransparencyEffect::None)
            .then(|| self.core.settings.transparency_effect.or_supported());

        // True if the theme was changed.
        let changed_theme = std::mem::replace(&mut self.win.theme_choice, self.core.settings.theme) != self.core.settings.theme;

        let changed_backdrop = self.win.backdrop_transparency != wanted_transparency;
        self.win.backdrop_transparency = wanted_transparency;
        self.win.pal = Palette::new(theme, accent, wanted_transparency.unwrap_or(TransparencyEffect::None));

        // DWM can drop the effect while the window is still being set up, so it is applied again a few
        // times shortly after startup.
        const RETRY_AT_MS: [u64; 4] = [150, 600, 1500, 3000];
        let mut redo = changed_backdrop || changed_theme;
        if wanted_transparency.is_some() {
            if let Some(&due) = RETRY_AT_MS.get(self.win.backdrop_retries) {
                let elapsed = self.win.backdrop_start.elapsed().as_millis() as u64;
                if elapsed >= due {
                    self.win.backdrop_retries += 1;
                    redo = true;
                } else {
                    // egui is reactive: without this, no frame runs to hit the next deadline
                    ctx.request_repaint_after(Duration::from_millis(due - elapsed));
                }
            }
        }

        if redo {
            match native {
                Some(native) => {
                    clear_window_transparency_effects(native);
                    if let Some(effect) = wanted_transparency {
                        apply_backdrop(native, self.win.pal.dark, effect);
                        nudge_frame(native);
                    }
                }
                // The OS window is not known yet: forget what was applied, so the next frame tries again.
                None => self.win.backdrop_transparency = None,
            }
            ctx.request_repaint();
        }
    }

    pub(crate) fn get_accent(&self) -> Accent {
        if self.core.settings.use_system_accent {
            self.core.sys_accent.unwrap_or_else(Accent::fallback)
        } else {
            let [r, g, b] = self.core.settings.custom_accent;
            Accent::from_base(Color32::from_rgb(r, g, b))
        }
    }
}
