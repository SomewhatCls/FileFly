//! Applies `Action`s produced by the UI.

use crate::prelude::*;

impl Explorer {
    pub(crate) fn apply(&mut self, action: Action) {
        let ti = self.active;
        if self.tabs[ti].special()
            && matches!(
                action,
                Action::NewFolder | Action::Paste | Action::Rename(_) | Action::CommitRename(..)
            )
        {
            return;
        }
        // An opened zip is shown from a temporary unpacked copy, so changes made there would
        // silently vanish: it is read-only (copying out of it is fine).
        let touches_archive = match &action {
            Action::NewFolder | Action::Paste => archive_of(&self.tabs[ti].dir).is_some(),
            Action::DropCopy(_, dest) => archive_of(dest).is_some(),
            Action::Cut(p) | Action::Delete(p) | Action::Compress(p) => {
                p.iter().any(|x| archive_of(x).is_some())
            }
            Action::Rename(p) | Action::CommitRename(p, _) => archive_of(p).is_some(),
            _ => false,
        };
        if touches_archive {
            self.tabs[ti].rename = None;
            self.tabs[ti].error = Some(
                "A zip archive is read-only here. Extract it (right-click → Extract all) to change its contents."
                    .to_owned(),
            );
            return;
        }
        match action {
            Action::Navigate(p) => self.navigate(ti, p),
            Action::NewTabAt(p) => {
                if has_zip_ext(&p) && p.is_file() {
                    // A zip gets its own tab next to its folder, then opens like a folder.
                    let home = p.parent().map(Path::to_path_buf).unwrap_or_else(|| p.clone());
                    self.new_tab(home);
                    let tab = self.active;
                    self.navigate(tab, p);
                } else {
                    self.new_tab(p);
                }
            }
            Action::NewTab => {
                let home = dirs::home_dir().unwrap_or_else(|| self.tabs[ti].dir.clone());
                self.new_tab(home);
            }
            Action::CloseTab(i) => {
                if self.tabs.len() <= 1 {
                    self.ui.send_viewport_cmd(egui::ViewportCommand::Close);
                } else if i < self.tabs.len() {
                    let closed_id = self.tabs[i].id;
                    self.tabs.remove(i);
                    if self.active >= self.tabs.len() {
                        self.active = self.tabs.len() - 1;
                    } else if i < self.active {
                        self.active -= 1;
                    }
                    if let Some(t) = self.tiles.take() {
                        self.tiles = remove_leaf(t, closed_id);
                    }
                    self.on_active_changed();
                    self.fix_tiles();
                }
            }
            Action::ClosePane(id) => {
                if let Some(t) = self.tiles.take() {
                    self.tiles = remove_leaf(t, id);
                }
                self.fix_tiles();
            }
            Action::OpenTerminal(dir) => {
                if let Err(e) = open_terminal(&dir) {
                    self.tabs[ti].error = Some(format!("Could not open a terminal: {e}"));
                }
            }
            Action::SwitchTab(i) => self.activate_tab(i),
            Action::StepTab(d) => {
                let n = self.tabs.len() as i32;
                let next = (self.active as i32 + d).rem_euclid(n) as usize;
                self.activate_tab(next);
            }
            Action::Back => {
                let tab = &mut self.tabs[ti];
                if let Some(p) = tab.back.pop() {
                    let cur = tab.dir.clone();
                    tab.fwd.push(cur);
                    self.set_dir(ti, p);
                }
            }
            Action::Forward => {
                let tab = &mut self.tabs[ti];
                if let Some(p) = tab.fwd.pop() {
                    let cur = tab.dir.clone();
                    tab.back.push(cur);
                    self.set_dir(ti, p);
                }
            }
            Action::Up => {
                // From the top of an opened zip, "Up" leaves the zip (its parent as shown, not the cache).
                if let Some(parent) = self.tabs[ti].shown_dir().parent().map(Path::to_path_buf) {
                    self.navigate(ti, parent);
                }
            }
            Action::Refresh => {
                if self.tabs[ti].trash.is_some() {
                    self.load_trash(ti);
                } else if self.tabs[ti].settings {
                    self.refresh_drives();
                } else {
                    self.reindex_force = true; // F5 also recalculates folder sizes
                    self.load_tab(ti);
                }
            }
            Action::GoToAddress(s) => {
                let typed = expand_env(s.trim().trim_matches('"'));
                if !typed.is_empty() {
                    self.navigate(ti, PathBuf::from(typed));
                }
            }
            Action::FocusAddress => {
                let tab = &mut self.tabs[ti];
                tab.address = tab.shown_dir().display().to_string();
                tab.editing_addr = true;
                tab.addr_focus = true;
            }
            Action::FocusSearch => self
                .ui
                .memory_mut(|m| m.request_focus(Id::new("search_box"))),
            Action::Open(p) => self.open_path(&p),
            Action::Compress(p) => self.compress(ti, p),
            Action::Extract(p) => self.extract(ti, p),
            Action::ExtractTo(p) => self.open_extract_to(ti, p),
            Action::Copy(p) => {
                if !p.is_empty() {
                    let _ = set_clipboard(&p, false); // `true` in the Cut arm
                    self.clipboard = Some((p, false));
                }
            }
            Action::Cut(p) => {
                if !p.is_empty() {
                    let _ = set_clipboard(&p, true);
                    self.clipboard = Some((p, true));
                }
            }
            Action::DropCopy(srcs, dest) => self.paste_into(dest, srcs, false),
            Action::CopyPath(p) => {
                let text = p
                    .iter()
                    .map(|x| x.display().to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                self.ui.copy_text(text);
            }
            Action::Paste => self.paste(),
            Action::Rename(p) => {
                let tab = &mut self.tabs[ti];
                if let Some(i) = tab.index_of(&p) {
                    let (name, is_dir) = {
                        let e = &tab.source()[i];
                        (e.name.clone(), e.is_dir)
                    };
                    tab.rename = Some(RenameState {
                        text: name,
                        is_dir,
                        path: p.clone(),
                        focus_pending: true,
                    });
                    tab.select_only(p);
                    tab.scroll_to = tab.view.iter().position(|&v| v == i);
                }
            }
            Action::CancelRename => self.tabs[ti].rename = None,
            Action::CommitRename(p, new_name) => {
                self.tabs[ti].rename = None;
                let new_name = new_name.trim().to_owned();
                let old_name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if new_name.is_empty() || new_name == old_name {
                    return;
                }
                if new_name.contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|']) {
                    self.tabs[ti].error = Some(
                        "A name can't contain any of these characters: \\ / : * ? \" < > |".into(),
                    );
                    return;
                }
                let Some(parent) = p.parent() else { return };
                let dest = parent.join(&new_name);
                // fs::rename silently replaces an existing file on Windows, so check first.
                // (Changing only the letter case is the same file and stays allowed.)
                if fs::symlink_metadata(&dest).is_ok()
                    && !p.as_os_str().eq_ignore_ascii_case(dest.as_os_str())
                {
                    self.tabs[ti].error =
                        Some(format!("An item named “{new_name}” already exists here."));
                    return;
                }
                match fs::rename(&p, &dest) {
                    Ok(()) => {
                        self.indexer.invalidate(&p);
                        self.tabs[ti].select_only(dest);
                        self.load_tab(ti);
                        self.tabs[ti].search_query.clear(); // rerun an active search
                    }
                    // Protected location (Program Files, ...): ask for administrator rights right
                    // away (UAC prompt) instead of just reporting the failure. When this window is
                    // already elevated, "denied" is a real refusal and is reported as usual.
                    Err(e) if is_denied_io(&e) && !is_elevated() => {
                        let op = AdminOp::Rename { from: p.clone(), to: dest.clone() };
                        self.indexer.invalidate(&p);
                        self.tabs[ti].select_only(dest);
                        self.run_job("Waiting for administrator approval…", move || run_elevated(&op));
                    }
                    Err(e) => self.tabs[ti].error = Some(format!("Could not rename item: {e}")),
                }
            }
            Action::Delete(p) => {
                if p.is_empty() {
                    return;
                }
                let (tx, ui) = (self.msg_tx.clone(), self.ui.clone());
                self.run_job("Moving to Recycle Bin…", move || match trash::delete_all(&p) {
                    Ok(_) => Ok(String::new()),
                    Err(e) => {
                        let msg = format!("Could not move item to the Recycle Bin: {e}");
                        if is_denied_text(&msg) {
                            let retry = AdminRetry::Op(AdminOp::Delete { paths: p.clone() });
                            let _ = tx.send(AppMsg::AdminOffer { msg: msg.clone(), retry });
                            ui.request_repaint();
                        }
                        Err(msg)
                    }
                });
            }
            Action::RetryAsAdmin => {
                let Some((_, retry)) = self.admin_retry.take() else { return };
                self.tabs[ti].error = None;
                match retry {
                    AdminRetry::Op(op) => {
                        self.run_job("Waiting for administrator approval…", move || run_elevated(&op))
                    }
                    AdminRetry::OpenFolder(p) => {
                        if let Err(e) = open_folder_elevated(&p) {
                            self.tabs[ti].error = Some(e);
                        }
                    }
                    AdminRetry::RunFile(p) => {
                        if let Err(e) = run_file_elevated(&p) {
                            self.tabs[ti].error = Some(e);
                        }
                    }
                }
            }
            Action::RunAsAdmin(p) => {
                if let Err(e) = run_file_elevated(&p) {
                    self.tabs[ti].error = Some(e);
                }
            }
            Action::OpenFolderAsAdmin(p) => {
                if let Err(e) = open_folder_elevated(&p) {
                    self.tabs[ti].error = Some(e);
                }
            }
            Action::NewFolder => {
                let target = unique_copy_name(&self.tabs[ti].dir.join("New folder"));
                match fs::create_dir(&target) {
                    Ok(()) => {
                        self.tabs[ti].pending_rename = Some(target);
                        self.load_tab(ti);
                    }
                    Err(e) => {
                        let msg = format!("Could not create folder: {e}");
                        if is_denied_io(&e) {
                            let op = AdminOp::NewFolder { path: target.clone() };
                            self.admin_retry = Some((msg.clone(), AdminRetry::Op(op)));
                        }
                        self.tabs[ti].error = Some(msg);
                    }
                }
            }
            Action::Properties(p) => {
                let seq = self.props_gen.fetch_add(1, AO::SeqCst) + 1;
                let mut props = build_props(&p);
                props.stats_gen = seq;
                if props.is_dir {
                    let (tx, ui, g) = (
                        self.msg_tx.clone(),
                        self.ui.clone(),
                        Arc::clone(&self.props_gen),
                    );
                    let root = p.clone();
                    std::thread::spawn(move || {
                        let stats = folder_stats(&root, &|| g.load(AO::Relaxed) != seq);
                        if g.load(AO::Relaxed) == seq {
                            let _ = tx.send(AppMsg::Stats(seq, stats));
                            ui.request_repaint();
                        }
                    });
                }
                self.props = Some(props);
            }
            Action::OpenContaining(p) => {
                if let Some(parent) = p.parent().map(Path::to_path_buf) {
                    self.navigate(ti, parent);
                    self.tabs[ti].select_only(p);
                }
            }
            Action::PeaOpen(p) => {
                if let Some(exe) = &self.peazip {
                    if let Err(e) = open_with_peazip(exe, &p) {
                        self.tabs[ti].error = Some(format!("Could not start PeaZip: {e}"));
                    }
                }
            }
            Action::PeaAdd(p) => {
                if let Some(exe) = &self.peazip {
                    if let Err(e) = add_to_peazip(exe, &p) {
                        self.tabs[ti].error = Some(format!("Could not start PeaZip: {e}"));
                    }
                }
            }
            Action::ToggleHidden => {
                self.settings.show_hidden = !self.settings.show_hidden;
                self.settings_dirty = true;
                self.reload_all();
            }
            Action::OpenSettings => self.open_settings(),
            Action::PinQuick(p) => self.pin_quick(p),
            Action::UnpinQuick(p) => self.unpin_quick(&p),
            Action::OpenRecycleBin => {
                if trashbin::SUPPORTED {
                    self.open_trash(ti);
                } else if let Err(e) = open_recycle_bin() {
                    self.tabs[ti].error = Some(format!("Could not open the Recycle Bin: {e}"));
                }
            }
            Action::TrashRestoreSelected => {
                let items = self.trash_items(ti, false);
                if !items.is_empty() {
                    self.run_job("Restoring…", move || {
                        trashbin::restore(items).map(|_| String::new())
                    });
                }
            }
            Action::TrashPurgeSelected => {
                let items = self.trash_items(ti, false);
                if !items.is_empty() {
                    let n = items.len();
                    let text = format!(
                        "{n} item{} will be deleted permanently. This can't be undone.",
                        if n == 1 { "" } else { "s" }
                    );
                    self.confirm = Some(ConfirmPurge { items, text });
                }
            }
            Action::TrashEmpty => {
                let items = self.trash_items(ti, true);
                if !items.is_empty() {
                    let n = items.len();
                    let text = format!("All {n} item{} in the Recycle Bin will be deleted permanently. This can't be undone.", if n == 1 { "" } else { "s" });
                    self.confirm = Some(ConfirmPurge { items, text });
                }
            }
            Action::TrashSelectAll => {
                if let Some(tr) = self.tabs[ti].trash.as_mut() {
                    tr.selected = tr.view.iter().copied().collect();
                }
            }
            Action::SelectAll => {
                let tab = &mut self.tabs[ti];
                if tab.trash.is_some() {
                    return;
                }
                let all: HashSet<PathBuf> = {
                    let src = tab.source();
                    tab.view
                        .iter()
                        .filter_map(|&i| src.get(i))
                        .map(|e| e.path.clone())
                        .collect()
                };
                tab.selected = all;
            }
            Action::MoveSelection(delta) => {
                let tab = &mut self.tabs[ti];
                if let Some(tr) = tab.trash.as_mut() {
                    trash_move(tr, delta);
                    return;
                }
                if tab.view.is_empty() {
                    return;
                }
                let cur = tab
                    .anchor
                    .as_ref()
                    .and_then(|s| tab.index_of(s))
                    .and_then(|i| tab.view.iter().position(|&v| v == i));
                let next = match cur {
                    Some(c) => (c as i32 + delta).clamp(0, tab.view.len() as i32 - 1) as usize,
                    None => 0,
                };
                let path = tab.source()[tab.view[next]].path.clone();
                tab.select_only(path);
                tab.scroll_to = Some(next);
            }
            Action::SelectEdge(last) => {
                let tab = &mut self.tabs[ti];
                if let Some(tr) = tab.trash.as_mut() {
                    trash_edge(tr, last);
                    return;
                }
                if tab.view.is_empty() {
                    return;
                }
                let i = if last { tab.view.len() - 1 } else { 0 };
                let path = tab.source()[tab.view[i]].path.clone();
                tab.select_only(path);
                tab.scroll_to = Some(i);
            }
        }
    }
}
