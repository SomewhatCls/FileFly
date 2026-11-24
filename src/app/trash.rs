//! Recycle Bin loading.

use crate::prelude::*;

impl Explorer {
    /// `X:\$Recycle.Bin` (and anything inside it): the raw storage of a drive's Recycle Bin, whose
    /// `$I…`/`$R…` files mean nothing to a user. Those open the Recycle Bin view instead.
    /// Only drive-letter paths on Windows can match; elsewhere this is always false.
    pub(crate) fn is_recycle_bin_dir(p: &Path) -> bool {
        use std::path::Component;
        let mut c = p.components();
        matches!(c.next(), Some(Component::Prefix(_)))
            && matches!(c.next(), Some(Component::RootDir))
            && matches!(c.next(), Some(Component::Normal(n)) if n.to_string_lossy().eq_ignore_ascii_case("$Recycle.Bin"))
    }

    /// `navigate`, except that a drive's `$Recycle.Bin` folder opens the Recycle Bin view
    /// (so the sidebar entry and the Recycle Bin toolbar show up, like when clicking the sidebar entry).
    pub(crate) fn navigate_or_bin(&mut self, ti: usize, path: PathBuf) {
        if trash_bin::SUPPORTED && Self::is_recycle_bin_dir(&path) {
            self.open_trash(ti);
        } else {
            self.navigate(ti, path);
        }
    }

    /// Opens the Recycle Bin view as a new history step.
    pub(crate) fn open_trash(&mut self, ti: usize) {
        let tab = &mut self.tabs[ti];
        if tab.page.take().is_some() {
            // Leaving a page (Settings, Licenses): like `navigate`, the tab starts a fresh history.
            tab.back.clear();
            tab.fwd.clear();
        } else if tab.trash.is_none() {
            let cur = tab.place();
            tab.back.push(cur);
            tab.fwd.clear();
        }
        self.show_trash(ti);
    }

    /// Switches the tab to the Recycle Bin view (no history change) and (re)loads it.
    pub(crate) fn show_trash(&mut self, ti: usize) {
        let tab = &mut self.tabs[ti];
        tab.page = None;
        if tab.trash.is_none() {
            tab.trash = Some(TrashState::new());
            tab.search.clear();
            tab.clear_selection();
            tab.rename = None;
            tab.editing_addr = false;
            tab.view_dirty = true;
        }
        self.load_trash(ti);
    }

    pub(crate) fn load_trash(&mut self, ti: usize) {
        let fmt = self.settings.date_format;
        let tab = &mut self.tabs[ti];
        let Some(tr) = tab.trash.as_mut() else { return };
        tr.seq += 1;
        tr.loading = true;
        let (id, seq) = (tab.id, tr.seq);
        let (tx, ui) = (self.msg_tx.clone(), self.ui.clone());
        std::thread::spawn(move || {
            let result = trash_bin::load(fmt);
            let _ = tx.send(AppMsg::Trash {
                tab: id,
                seq,
                result,
            });
            ui.request_repaint();
        });
    }

    /// Starts the background upkeep of the Recycle Bin size shown in the sidebar (only the first call does anything).
    pub(crate) fn start_trash_index(&self) {
        let ui = self.ui.clone();
        trash_bin::start_index(move || ui.request_repaint());
    }

    /// The items of the Recycle Bin view (all, or the selected ones), without those that a running
    /// operation is already working on (`trash_busy_ids`): they can't be restored or deleted again.
    pub(crate) fn trash_items(&self, ti: usize, all: bool) -> Vec<trash_bin::Item> {
        let busy = self.trash_busy_ids();
        let free = |r: &&trash_bin::TrashRow| !busy.contains(&r.item.id);
        self.tabs[ti]
            .trash
            .as_ref()
            .map(|tr| {
                if all {
                    tr.rows.iter().filter(free).map(|r| r.item.clone()).collect()
                } else {
                    tr.selected.iter().filter_map(|&i| tr.rows.get(i)).filter(free).map(|r| r.item.clone()).collect()
                }
            })
            .unwrap_or_default()
    }

    /// Marks `items` as busy until the returned guard is dropped: move the guard into the job that
    /// works on them.
    pub(crate) fn mark_trash_busy(&self, items: &[trash_bin::Item]) -> BusyGuard {
        BusyGuard::new(self.trash_busy.clone(), items.iter().map(|i| i.id.clone()).collect())
    }

    /// Everything running operations are working on (see `ActiveOp::paths`).
    pub(crate) fn locked_paths(&self) -> Vec<PathBuf> {
        self.ops.iter().flat_map(|o| o.paths.iter().cloned()).collect()
    }

    /// Records what operation `id` works on, until it is over.
    pub(crate) fn lock_op_paths(&mut self, id: u64, paths: impl IntoIterator<Item = PathBuf>) {
        if let Some(op) = self.ops.iter_mut().find(|o| o.id == id) {
            op.paths.extend(paths);
        }
    }

    /// Ids of the Recycle Bin items that can't be restored or deleted right now: those a Recycle Bin
    /// operation works on, and those that came from a place a running operation is working on (for
    /// example the folder that is still being moved into the bin).
    pub(crate) fn trash_busy_ids(&self) -> HashSet<std::ffi::OsString> {
        let mut ids = self.trash_busy.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let locked = self.locked_paths();
        if !locked.is_empty() {
            for tr in self.tabs.iter().filter_map(|t| t.trash.as_ref()) {
                for r in &tr.rows {
                    if touches_locked(&locked, &[trash_bin::original(&r.item)]) {
                        ids.insert(r.item.id.clone());
                    }
                }
            }
        }
        ids
    }
}
