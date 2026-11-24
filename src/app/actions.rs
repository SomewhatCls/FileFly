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
            self.tabs[ti].error(
                "A zip archive is read-only here. Extract it (right-click → Extract all) to change its contents."
                    .to_owned(),
            );
            return;
        }
        // Something that is being restored into place can't be changed until that is done.
        let into_restore = match &action {
            Action::Cut(p) | Action::Delete(p) | Action::Compress(p) => touches_locked(&self.locked_paths(), p),
            Action::Rename(p) | Action::CommitRename(p, _) => touches_locked(&self.locked_paths(), std::slice::from_ref(p)),
            _ => false,
        };
        if into_restore {
            self.tabs[ti].rename = None;
            self.tabs[ti].error("Not available while items are being restored here.".to_owned());
            return;
        }
        match action {
            Action::Navigate(p) => self.navigate_or_bin(ti, p),
            Action::NewTabAt(p) => {
                if trash_bin::SUPPORTED && Self::is_recycle_bin_dir(&p) {
                    // The raw folder is shown as the Recycle Bin view, in a tab of its own.
                    let home = dirs::home_dir().unwrap_or_else(|| self.tabs[ti].dir.clone());
                    self.new_tab(home);
                    let tab = self.active;
                    self.open_trash(tab);
                } else if has_zip_ext(&p) && p.is_file() {
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
                    self.tabs[ti].error(format!("Could not open a terminal: {e}"));
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
                    let cur = tab.place();
                    tab.fwd.push(cur);
                    self.goto_place(ti, p);
                }
            }
            Action::Forward => {
                let tab = &mut self.tabs[ti];
                if let Some(p) = tab.fwd.pop() {
                    let cur = tab.place();
                    tab.back.push(cur);
                    self.goto_place(ti, p);
                }
            }
            Action::Up => {
                // From the top of an opened zip, "Up" leaves the zip (its parent as shown, not the cache).
                if let Some(parent) = self.tabs[ti].shown_dir().parent().map(Path::to_path_buf) {
                    self.navigate_or_bin(ti, parent);
                }
            }
            Action::Refresh => {
                if self.tabs[ti].trash.is_some() {
                    self.load_trash(ti);
                } else if self.tabs[ti].page == Some(Page::Settings) {
                    self.refresh_drives();
                } else {
                    self.reindex_force = true; // F5 also recalculates folder sizes
                    self.load_tab(ti);
                }
            }
            Action::GoToAddress(s) => {
                let typed = expand_env(s.trim().trim_matches('"'));
                if !typed.is_empty() {
                    self.navigate_or_bin(ti, PathBuf::from(typed));
                }
            }
            Action::FocusAddress => {
                let tab = &mut self.tabs[ti];
                // A page's address bar starts empty: its label is not a path.
                tab.address = if tab.special() { String::new() } else { tab.shown_dir().display().to_string() };
                tab.editing_addr = true;
                tab.addr_focus = true;
            }
            Action::TypeAhead(s) => self.type_ahead(ti, &s),
            Action::FocusSearch => self
                .ui
                .memory_mut(|m| m.request_focus(Id::unique("search_box"))),
            Action::Open(p) => {
                if trash_bin::SUPPORTED && Self::is_recycle_bin_dir(&p) {
                    self.open_trash(ti);
                } else {
                    self.open_path(&p);
                }
            }
            Action::Compress(p) => self.compress(ti, p),
            Action::Extract(p) => self.extract(ti, p),
            Action::ExtractTo(p) => self.open_extract_to(ti, p),
            Action::Copy(p) => {
                if !p.is_empty() {
                    // DO NOT USE THE SYSTEM CLIPBOARD ON NON-WINDOWS
                    #[cfg(windows)]
                    let _ = set_clipboard(&p, false);
                    self.clipboard = Some((p, false));
                }
            }
            Action::Cut(p) => {
                if !p.is_empty() {
                    // DO NOT USE THE SYSTEM CLIPBOARD ON NON-WINDOWS
                    #[cfg(windows)]
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
                    self.tabs[ti].error(
                        "A name can't contain any of these characters: \\ / : * ? \" < > |",
                    );
                    return;
                }
                let Some(parent) = p.parent() else { return };
                let dest = parent.join(&new_name);
                // fs::rename silently replaces an existing file on Windows, so check first.
                // (Changing only the letter case is the same file and stays allowed: the file system
                // decides what "the same" is, not an ASCII-only comparison of the names.)
                if fs::symlink_metadata(&dest).is_ok() && !is_same_entry(&p, &dest) {
                    self.tabs[ti].error(format!("An item named “{new_name}” already exists here."));
                    return;
                }
                match fs::rename(&p, &dest) {
                    Ok(()) => {
                        if let Some(ix) = &self.indexer { ix.invalidate(&p); }
                        self.tabs[ti].select_only(dest);
                        self.load_tab(ti);
                        self.tabs[ti].search_query.clear(); // rerun an active search
                    }
                    // Protected location (Program Files, ...): ask for administrator rights right
                    // away (UAC prompt) instead of just reporting the failure. When this window is
                    // already elevated, "denied" is a real refusal and is reported as usual.
                    Err(e) if is_denied_io(&e) && !is_elevated() => {
                        let op = AdminOp::Rename { from: p.clone(), to: dest.clone() };
                        if let Some(ix) = &self.indexer { ix.invalidate(&p); }
                        self.tabs[ti].select_only(dest);
                        self.run_job("Waiting for administrator approval…", move || run_elevated(&op));
                    }
                    Err(e) => self.tabs[ti].error(format!("Could not rename item: {e}")),
                }
            }
            Action::Delete(p) => {
                if p.is_empty() {
                    return;
                }
                // Folders arrive in the bin with the size the list shows for them (a running total
                // if the scan wasn't finished), or else the indexer's stored one; the Recycle Bin
                // size in the sidebar starts from it. It is exact only if the indexer vouches for it.
                if let Some(ix) = &self.indexer {
                    let sel: HashSet<&PathBuf> = p.iter().collect();
                    let shown: HashMap<&PathBuf, u64> = self.tabs[ti]
                        .entries
                        .iter()
                        .filter(|e| e.is_dir && sel.contains(&e.path))
                        .filter_map(|e| Some((&e.path, e.dir_size?)))
                        .collect();
                    for x in &p {
                        let stored = ix.lookup_fresh(x);
                        if let Some(size) = shown.get(x).copied().or(stored.map(|(s, _)| s)) {
                            trash_bin::expect_trashed(x.clone(), size, stored.is_some_and(|(_, fresh)| fresh));
                        }
                    }
                }
                let (tx, ui) = (self.msg_tx.clone(), self.ui.clone());
                let spec = OpSpec {
                    title: "Moving to Recycle Bin".to_owned(),
                    from: p[0].parent().map(|d| d.display().to_string()).unwrap_or_default(),
                    to: String::new(),
                    cancelable: true,
                };
                let locked = p.clone();
                let op = self.run_op(spec, move |prog| {
                    use std::sync::atomic::Ordering::Relaxed;
                    let _settle = trash_bin::DeletionGuard;
                    // Batches: progress between them and Cancel in the gaps. One call per file would be much
                    // slower, because each starts its own shell operation. A running shell operation
                    // itself cannot be interrupted, so Cancel takes effect at the next gap.
                    const BATCH: usize = 32;
                    let name_of = |x: &Path| x.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    prog.unpausable.store(true, Relaxed); // a running batch cannot be paused
                    prog.total_items.store(p.len() as u64, Relaxed);

                    // Like Explorer, find out up front what the Recycle Bin can't take (no bin on this
                    // drive, bin turned off, single items bigger than its limit). Summing sizes can take a
                    // moment: the panel shows "Calculating…" with the item being measured.
                    prog.preparing.store(true, Relaxed);
                    let (bin_ok, forced, forced_reason) = crate::recycle_check::split(&p, &|| prog.cancelled(), &|x| prog.set_current(&name_of(x)));
                    prog.preparing.store(false, Relaxed);
                    if prog.cancelled() {
                        return Ok(String::new());
                    }
                    // The question is shown in this operation's panel in the ops window and this thread
                    // waits for the answer. Cancel there (or cancelling the operation) changes nothing.
                    if !forced.is_empty() && !prog.ask_permanent(forced.len(), name_of(&forced[0]), forced_reason) {
                        return Ok(String::new());
                    }

                    // Items the bin refused anyway (the check above is an approximation).
                    let mut denied: Vec<PathBuf> = Vec::new(); // no access rights -> offer administrator retry
                    let mut refused: Vec<PathBuf> = Vec::new(); // -> ask: delete permanently?
                    let mut denied_msg = String::new();

                    for batch in bin_ok.chunks(BATCH) {
                        if prog.cancelled() {
                            break; // what is already in the bin stays there
                        }
                        prog.set_current(&name_of(&batch[0]));
                        if trash::delete_all(batch).is_ok() {
                            prog.done_items.fetch_add(batch.len() as u64, Relaxed);
                            continue;
                        }
                        // A batch fails as a whole and may have been partly done, so find the culprit
                        // by going item by item. This is the only case that pays the per-item cost.
                        for path in batch {
                            if prog.cancelled() {
                                break;
                            }
                            prog.set_current(&name_of(path));
                            // Already in the bin (done by the failed batch before it stopped).
                            if fs::symlink_metadata(path).is_err() {
                                prog.done_items.fetch_add(1, Relaxed);
                                continue;
                            }
                            if let Err(e) = trash::delete(path) {
                                let msg = format!("Could not move item to the Recycle Bin: {e}"); // shown to the user only
                                if cfg!(debug_assertions) {
                                    eprintln!("trash::delete({path:?}) failed: {e:?}");
                                }
                                if is_denied_trash(&e) {
                                    denied.push(path.clone());
                                    denied_msg = msg;
                                } else {
                                    refused.push(path.clone());
                                }
                            }
                            prog.done_items.fetch_add(1, Relaxed);
                        }
                    }

                    // Whatever the user agreed to delete permanently: the items found up front, plus
                    // any the bin refused after all (those need their own question).
                    let mut for_good = forced;
                    if !refused.is_empty()
                        && !prog.cancelled()
                        && prog.ask_permanent(refused.len(), name_of(&refused[0]), String::new())
                    {
                        for_good.extend(refused);
                    }
                    // Permanent deletion counts bytes: measure first, then every deleted file adds its size.
                    if !for_good.is_empty() {
                        prog.preparing.store(true, Relaxed);
                        let total: u64 = for_good.iter().map(|x| size_up_to(x, u64::MAX, &|| prog.cancelled())).sum();
                        prog.total_bytes.store(total, Relaxed);
                        prog.preparing.store(false, Relaxed);
                    }
                    let mut failed = 0usize;
                    let mut failed_msg = String::new();
                    for path in &for_good {
                        if prog.cancelled() {
                            break;
                        }
                        prog.set_current(&name_of(path));
                        // File by file, so Cancel also stops in the middle of a big folder.
                        match delete_for_good(path, &|| prog.cancelled(), &|f, bytes| {
                            prog.set_current(&name_of(f));
                            prog.done_bytes.fetch_add(bytes, Relaxed);
                        }) {
                            Ok(true) => {}
                            Ok(false) => break, // cancelled: what is left of this folder stays as it is
                            Err(e) => {
                                failed += 1;
                                failed_msg = format!("Could not delete “{}”: {e}", name_of(path));
                            }
                        }
                        prog.done_items.fetch_add(1, Relaxed);
                    }

                    if !denied.is_empty() {
                        let retry = AdminRetry::Op(AdminOp::Delete { paths: denied });
                        let _ = tx.send(AppMsg::AdminOffer { msg: denied_msg.clone(), retry });
                        ui.request_repaint();
                        return Err(denied_msg);
                    }
                    if failed > 0 {
                        return Err(if failed == 1 { failed_msg } else { format!("{failed} items could not be deleted. Last error: {failed_msg}") });
                    }
                    Ok(String::new())
                });
                self.lock_op_paths(op, locked);
            }
            Action::RetryAsAdmin => {
                let Some((_, retry)) = self.admin_retry.take() else { return };
                self.tabs[ti].notice = None;
                match retry {
                    AdminRetry::Op(op) => {
                        self.run_job("Waiting for administrator approval…", move || run_elevated(&op))
                    }
                    AdminRetry::OpenFolder(p) => {
                        if let Err(e) = open_folder_elevated(&p) {
                            self.tabs[ti].error(e);
                        }
                    }
                    AdminRetry::RunFile(p) => {
                        if let Err(e) = run_file_elevated(&p) {
                            self.tabs[ti].error(e);
                        }
                    }
                }
            }
            Action::ChangeFolderIcon(p) => self.open_icon_picker(ti, p),
            Action::ResetFolderIcon(p) => self.set_folder_icon(&p, None),
            Action::RunAsAdmin(p) => {
                if let Err(e) = run_file_elevated(&p) {
                    self.tabs[ti].error(e);
                }
            }
            Action::OpenFolderAsAdmin(p) => {
                if let Err(e) = open_folder_elevated(&p) {
                    self.tabs[ti].error(e);
                }
            }
            Action::NewFolder => {
                let target = unique_folder_name(&self.tabs[ti].dir.join("New folder"));
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
                        self.tabs[ti].error(msg);
                    }
                }
            }
            // Attempts to create empty, extensionless file.
            Action::NewFile => {
                let target = unique_copy_name(&self.tabs[ti].dir.join("New File"));
                match File::create_new(&target) {
                    Ok(_) => {
                        self.tabs[ti].pending_rename = Some(target);
                        self.load_tab(ti);
                    }
                    Err(e) => {
                        let message = format!("Could not create file: {e}");

                        // Permission errors can be retried with elevated rights.
                        if is_denied_io(&e) {
                            let op = AdminOp::NewFile { path: target };
                            self.admin_retry = Some((message.clone(), AdminRetry::Op(op)));
                        }

                        self.tabs[ti].error(message);
                    }
                }
            }
            Action::Properties(p) => self.open_properties(p),
            Action::OpenContaining(p) => {
                if let Some(parent) = p.parent().map(Path::to_path_buf) {
                    self.navigate_or_bin(ti, parent);
                    self.tabs[ti].select_only(p);
                }
            }
            Action::OpenShortcutLocation(p) => self.open_shortcut_location(ti, p),
            Action::PeaOpen(p) => {
                if let Some(exe) = &self.peazip {
                    if let Err(e) = open_with_peazip(exe, &p) {
                        self.tabs[ti].error(format!("Could not start PeaZip: {e}"));
                    }
                }
            }
            Action::PeaAdd(p) => {
                if let Some(exe) = &self.peazip {
                    if let Err(e) = add_to_peazip(exe, &p) {
                        self.tabs[ti].error(format!("Could not start PeaZip: {e}"));
                    }
                }
            }
            Action::ToggleHidden => {
                self.settings.show_hidden = !self.settings.show_hidden;
                self.settings_dirty = true;
                self.reload_all();
            }
            Action::OpenSettings => self.open_settings(),
            Action::OpenWhatsNew => self.open_whats_new(),
            Action::PinQuick(p) => self.pin_quick(p),
            Action::UnpinQuick(p) => self.unpin_quick(&p),
            Action::OpenRecycleBin => {
                if trash_bin::SUPPORTED {
                    self.open_trash(ti);
                } else if let Err(e) = open_recycle_bin() {
                    self.tabs[ti].error(format!("Could not open the Recycle Bin: {e}"));
                }
            }
            Action::TrashRestoreSelected => {
                let items = self.trash_items(ti, false);
                if !items.is_empty() {
                    let spec = OpSpec {
                        title: "Restoring".to_owned(),
                        from: String::new(),
                        to: String::new(),
                        cancelable: false,
                    };
                    // The items stay busy (greyed out, can't be deleted) until the job is over.
                    let busy = self.mark_trash_busy(&items);
                    let origins: Vec<PathBuf> = items.iter().map(trash_bin::original).collect();
                    let op = self.run_op(spec, move |progress| {
                        let _busy = busy;
                        trash_bin::restore(items, &progress).map(|_| String::new())
                    });
                    self.lock_op_paths(op, origins);
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
                    let guard = self.mark_trash_busy(&items);
                    self.confirm = Some(ConfirmPurge { items, text, guard });
                }
            }
            Action::TrashEmpty => {
                let items = self.trash_items(ti, true);
                // Not while items are being worked on: "all items" would not be true.
                if !items.is_empty() && self.trash_busy_ids().is_empty() {
                    let n = items.len();
                    let text = format!("All {n} item{} in the Recycle Bin will be deleted permanently. This can't be undone.", if n == 1 { "" } else { "s" });
                    let guard = self.mark_trash_busy(&items);
                    self.confirm = Some(ConfirmPurge { items, text, guard });
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
            Action::CheckForUpdate => self.check_for_update(false),
            Action::DownloadUpdate => self.open_update_download(),
        }
    }
}

/// Whether `a` and `b` are the same directory entry, which is what a rename that only changes the
/// letter case of a name is. Never a text comparison: that would be wrong for every non-ASCII letter
/// and for case-sensitive file systems.
fn is_same_entry(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        matches!(
            (fs::symlink_metadata(a), fs::symlink_metadata(b)),
            (Ok(x), Ok(y)) if x.dev() == y.dev() && x.ino() == y.ino()
        )
    }
    #[cfg(not(unix))]
    {
        // Both names resolve to the one stored path (with the case it is really stored in). Links are
        // left out: a link and its target resolve to the same path but are two entries.
        let is_link = |p: &Path| fs::symlink_metadata(p).map(|m| m.file_type().is_symlink()).unwrap_or(true);
        !is_link(a)
            && !is_link(b)
            && matches!((dunce::canonicalize(a), dunce::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
    }
}
