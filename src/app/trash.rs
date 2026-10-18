//! Recycle Bin loading.

use crate::prelude::*;

impl Explorer {
    pub(crate) fn open_trash(&mut self, ti: usize) {
        let tab = &mut self.tabs[ti];
        if tab.trash.is_none() {
            let cur = tab.dir.clone();
            tab.back.push(cur);
            tab.fwd.clear();
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
            let result = trashbin::load(fmt);
            let _ = tx.send(AppMsg::Trash {
                tab: id,
                seq,
                result,
            });
            ui.request_repaint();
        });
    }

    pub(crate) fn trash_items(&self, ti: usize, all: bool) -> Vec<trashbin::Item> {
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
