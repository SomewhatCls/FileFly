//! Creating, activating, tiling and detaching tabs.

use crate::prelude::*;

impl Explorer {
    pub(crate) fn new_tab(&mut self, place: Place) {
        let (dir, page) = match place {
            Place::Folder(p) => (p, None),
            Place::Page(pg) => (self.win.tabs[self.win.active].dir.clone(), Some(pg)),
        };
        let id = self.core.next_tab_id;
        self.core.next_tab_id += 1;
        self.win.tabs.push(Tab::new(id, dir));
        self.win.active = self.win.tabs.len() - 1;
        self.load_tab(self.win.active);
        self.on_active_changed();
        match page {
            None => { self.load_tab(self.win.active); self.on_active_changed(); }
            Some(pg) => self.show_page(self.win.active, pg), // calls on_active_changed itself
        }
    }

    /// Makes tab `i` the focused one. In tiled mode a tab that is not on screen replaces the focused pane.
    pub(crate) fn activate_tab(&mut self, i: usize) {
        if i >= self.win.tabs.len() {
            return;
        }
        let new_id = self.win.tabs[i].id;
        let mut shown = false;
        if let Some(tree) = self.win.tiles.as_mut() {
            if !tree.contains(new_id) {
                let cur = self.win.tabs[self.win.active].id;
                tree.replace(cur, new_id);
                shown = true;
            }
        }
        if i != self.win.active || shown {
            self.win.active = i;
            self.on_active_changed();
            if shown {
                self.load_tab(i);
            }
        }
    }

    /// Keeps the tile tree consistent: drops panes whose tab is gone, turns a lone pane back into the
    /// normal view, and makes sure the focused tab is one of the visible ones.
    pub(crate) fn fix_tiles(&mut self) {
        let Some(tree) = self.win.tiles.take() else {
            return;
        };
        let mut ids = Vec::new();
        tree.leaf_ids(&mut ids);
        let mut tree = Some(tree);
        for id in ids {
            if !self.win.tabs.iter().any(|t| t.id == id) {
                tree = tree.and_then(|t| remove_leaf(t, id));
            }
        }
        let Some(tree) = tree else {
            return;
        };
        if let Tile::Leaf(id) = &tree {
            if let Some(i) = self.win.tabs.iter().position(|t| t.id == *id) {
                if i != self.win.active {
                    self.win.active = i;
                    self.on_active_changed();
                }
            }
            return; // a single pane is just the normal view
        }
        let active_id = self.win.tabs[self.win.active].id;
        let first = tree.first_leaf();
        let has_active = tree.contains(active_id);
        self.win.tiles = Some(tree);
        if !has_active {
            if let Some(i) = self.win.tabs.iter().position(|t| t.id == first) {
                self.win.active = i;
                self.on_active_changed();
            }
        }
    }

    /// A tab was dropped on a pane (`Tile::drop_onto` decides what that means). Dropping a tab on its own pane
    /// does nothing; splitting a pane is the shortcut's job (`split_focused`).
    pub(crate) fn drop_tab(&mut self, dragged: u64, target: u64, zone: DropZone) {
        let Some(di) = self.win.tabs.iter().position(|t| t.id == dragged) else {
            return;
        };
        let mut target = target;
        if dragged == target {
            if zone == DropZone::Center || self.win.tiles.is_some() {
                return;
            }
            // Single pane: the tab being looked at is dragged onto an edge of the view. It becomes the new pane
            // and the pane it leaves behind shows another tab (a fresh one at the same folder if there is none).
            let tabs = &self.win.tabs;
            let other = tabs.iter().skip(di + 1).chain(tabs.iter().take(di)).next().map(|t| t.id);
            target = match other {
                Some(id) => id,
                None => {
                    let id = self.core.next_tab_id;
                    self.core.next_tab_id += 1;
                    let dir = self.win.tabs[di].dir.clone();
                    self.win.tabs.push(Tab::new(id, dir));
                    let last = self.win.tabs.len() - 1;
                    self.load_tab(last);
                    id
                }
            };
        }
        let tree = self.win.tiles.take().unwrap_or(Tile::Leaf(target)).drop_onto(dragged, target, zone);
        self.win.tiles = Some(tree);
        if di != self.win.active {
            self.win.active = di;
            self.on_active_changed();
        }
        self.load_tab(di);
        self.fix_tiles();
    }


    /// Ctrl+S: splits the focused pane and opens a copy of its folder in the new pane. The split
    /// cuts across the longer side, so both halves end up closer to square: a wide pane is split
    /// left/right, a tall one top/bottom. Does nothing at the 4-pane limit.
    pub(crate) fn split_focused(&mut self) {
        let Some(tab) = self.win.tabs.get(self.win.active) else {
            return;
        };
        if tab.special() {
            return;
        }
        let (cur, dir) = (tab.id, tab.dir.clone());
        let mut ids = Vec::new();
        match &self.win.tiles {
            Some(t) => t.leaf_ids(&mut ids),
            None => ids.push(cur),
        }
        if ids.len() >= MAX_PANES {
            return;
        }
        let Some(rect) = self.win.pane_rects.iter().find(|(id, _)| *id == cur).map(|(_, r)| *r) else {
            return;
        };
        // Flip `>=` to `<` to cut along the other axis instead.
        let side_by_side = rect.width() >= rect.height();
        let id = self.core.next_tab_id;
        self.core.next_tab_id += 1;
        self.win.tabs.push(Tab::new(id, dir));
        let last = self.win.tabs.len() - 1;
        self.load_tab(last);
        let mut tree = self.win.tiles.take().unwrap_or(Tile::Leaf(cur));
        tree.split_leaf(cur, id, false, side_by_side);
        self.win.tiles = Some(tree);
        self.win.active = last;
        self.on_active_changed();
        self.fix_tiles();
    }

    /// The window of this process (other than `except`) under the screen point, the most recently focused one
    /// when they overlap.
    pub(crate) fn window_under(&self, cx: f32, cy: f32, except: u64) -> Option<u64> {
        self.each_window()
            .into_iter()
            .filter(|w| w.serial != except)
            .filter_map(|w| w.own_info.map(|o| (w.serial, o)))
            .filter(|(_, o)| cx >= o.rect[0] && cx < o.rect[2] && cy >= o.rect[1] && cy < o.rect[3])
            .max_by_key(|(_, o)| o.focus_ms)
            .map(|(s, _)| s)
    }

    /// A tab was released outside the window: move it into another window under the pointer (a window of
    /// this process in memory, an elevated one through `peers`), or into a new window of its own.
    pub(crate) fn detach_tab(&mut self, dragged: u64) {
        let Some(ti) = self.win.tabs.iter().position(|t| t.id == dragged) else {
            return;
        };
        if self.win.tabs[ti].special() {
            return;
        }
        let Some((cx, cy)) = cursor_screen_pos() else {
            return;
        };
        let only = self.win.tabs.len() <= 1;
        let everything = self.win.ui.input(|i| i.modifiers.shift);
        // A window of this process: the tab (with all its state) moves in memory, done between frames.
        if let Some(to) = self.window_under(cx, cy, self.win.serial) {
            let from = self.win.serial;
            self.pending.push(if only || everything {
                WinCmd::MergeWindow { from, to }
            } else {
                WinCmd::MoveTab { from, tab: dragged, to: Some(to), at: None }
            });
            return;
        }
        // Another process (an elevated window can't share memory with us): hand the folder over.
        match self.core.peers.window_at(cx, cy) {
            Some(pid) => {
                if only || everything {
                    // Merge this whole window into the other one (the dragged tab goes last, so it
                    // ends up as the active tab there), then close this window.
                    let mut dirs: Vec<PathBuf> = self
                        .win.tabs
                        .iter()
                        .filter(|t| t.id != dragged && !t.special())
                        .map(|t| t.dir.clone())
                        .collect();
                    dirs.push(self.win.tabs[ti].dir.clone());
                    if dirs.iter().all(|d| self.core.peers.send_tab(pid, d)) {
                        self.win.ui.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                } else {
                    let dir = self.win.tabs[ti].dir.clone();
                    if self.core.peers.send_tab(pid, &dir) {
                        self.apply(Action::CloseTab(ti));
                    }
                }
            }
            None => {
                if only {
                    return; // the only tab already is a window of its own
                }
                let ppp = self.win.ui.pixels_per_point().max(0.1);
                let at = ((cx / ppp - 80.0).max(0.0), (cy / ppp - 20.0).max(0.0));
                self.pending.push(WinCmd::MoveTab { from: self.win.serial, tab: dragged, to: None, at: Some(at) });
            }
        }
    }
}
