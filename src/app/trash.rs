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
        if tab.trash.is_none() {
            let cur = tab.place();
            tab.back.push(cur);
            tab.fwd.clear();
        }
        self.show_trash(ti);
    }

    /// Switches the tab to the Recycle Bin view (no history change) and (re)loads it.
    pub(crate) fn show_trash(&mut self, ti: usize) {
        let tab = &mut self.tabs[ti];
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

    pub(crate) fn trash_items(&self, ti: usize, all: bool) -> Vec<trash_bin::Item> {
        self.tabs[ti]
            .trash
            .as_ref()
            .map(|tr| {
                if all {
                    tr.rows.iter().map(|r| r.item.clone()).collect()
                } else {
                    tr.selected
                        .iter()
                        .filter_map(|&i| tr.rows.get(i))
                        .map(|r| r.item.clone())
                        .collect()
                }
            })
            .unwrap_or_default()
    }
}
