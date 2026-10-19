//! Creating, activating, tiling and detaching tabs.

use crate::prelude::*;

impl Explorer {
    pub(crate) fn new_tab(&mut self, dir: PathBuf) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        if let Some(tree) = self.tiles.as_mut() {
            // In tiled mode the new tab takes over the focused pane.
            let cur = self.tabs[self.active].id;
            tree.replace(cur, id);
        }
        self.tabs.push(Tab::new(id, dir));
        self.active = self.tabs.len() - 1;
        self.load_tab(self.active);
        self.on_active_changed();
    }

    /// Makes tab `i` the focused one. In tiled mode a tab that is not on screen replaces the focused pane.
    pub(crate) fn activate_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        let new_id = self.tabs[i].id;
        let mut shown = false;
        if let Some(tree) = self.tiles.as_mut() {
            if !tree.contains(new_id) {
                let cur = self.tabs[self.active].id;
                tree.replace(cur, new_id);
                shown = true;
            }
        }
        if i != self.active || shown {
            self.active = i;
            self.on_active_changed();
            if shown {
                self.load_tab(i);
            }
        }
    }

    /// Keeps the tile tree consistent: drops panes whose tab is gone, turns a lone pane back into the
    /// normal view, and makes sure the focused tab is one of the visible ones.
    pub(crate) fn fix_tiles(&mut self) {
        let Some(tree) = self.tiles.take() else {
            return;
        };
        let mut ids = Vec::new();
        tree.leaf_ids(&mut ids);
        let mut tree = Some(tree);
        for id in ids {
            if !self.tabs.iter().any(|t| t.id == id) {
                tree = tree.and_then(|t| remove_leaf(t, id));
            }
        }
        let Some(tree) = tree else {
            return;
        };
        if let Tile::Leaf(id) = &tree {
            if let Some(i) = self.tabs.iter().position(|t| t.id == *id) {
                if i != self.active {
                    self.active = i;
                    self.on_active_changed();
                }
            }
            return; // a single pane is just the normal view
        }
        let active_id = self.tabs[self.active].id;
        let first = tree.first_leaf();
        let has_active = tree.contains(active_id);
        self.tiles = Some(tree);
        if !has_active {
            if let Some(i) = self.tabs.iter().position(|t| t.id == first) {
                self.active = i;
                self.on_active_changed();
            }
        }
    }

    /// A tab was dropped on a pane: on its edge it splits the pane, in the middle it takes the pane's place.
    pub(crate) fn drop_tab(&mut self, dragged: u64, mut target: u64, zone: DropZone) {
        let Some(di) = self.tabs.iter().position(|t| t.id == dragged) else {
            return;
        };
        if dragged == target && zone != DropZone::Center && self.tiles.is_some() {
            // Tiled mode: dragging a tab onto an edge of its own pane duplicates it into a new pane
            // (the same thing the single-pane branch below does), as long as there is room.
            if self.tabs[di].special() {
                return;
            }
            let mut tree = self.tiles.take().unwrap();
            let mut ids = Vec::new();
            tree.leaf_ids(&mut ids);
            if ids.len() < 4 {
                let id = self.next_tab_id;
                self.next_tab_id += 1;
                let dir = self.tabs[di].dir.clone();
                self.tabs.push(Tab::new(id, dir));
                let last = self.tabs.len() - 1;
                self.load_tab(last);
                let (new_first, side_by_side) = match zone {
                    DropZone::Left => (true, true),
                    DropZone::Right => (false, true),
                    DropZone::Top => (true, false),
                    _ => (false, false),
                };
                tree.split_leaf(target, id, new_first, side_by_side);
                self.active = last;
                self.on_active_changed();
            }
            self.tiles = Some(tree);
            self.fix_tiles();
            return;
        }
        if dragged == target && zone != DropZone::Center && self.tiles.is_none() {
            // Dragging the tab you are looking at onto the edge of the view: it becomes the new
            // pane, and the pane it leaves behind shows another tab (a fresh one if there is none).
            let other = self
                .tabs
                .iter()
                .skip(di + 1)
                .chain(self.tabs.iter().take(di))
                .next()
                .map(|t| t.id);
            target = match other {
                Some(id) => id,
                None => {
                    let id = self.next_tab_id;
                    self.next_tab_id += 1;
                    let dir = self.tabs[di].dir.clone();
                    self.tabs.push(Tab::new(id, dir));
                    let last = self.tabs.len() - 1;
                    self.load_tab(last);
                    id
                }
            };
        }
        let mut tree = self.tiles.take().unwrap_or(Tile::Leaf(target));
        if dragged != target {
            if zone == DropZone::Center {
                if tree.contains(dragged) {
                    tree.swap(dragged, target);
                } else {
                    tree.replace(target, dragged);
                }
            } else {
                let mut ids = Vec::new();
                tree.leaf_ids(&mut ids);
                let already = ids.contains(&dragged);
                if already || ids.len() < 4 {
                    let base = if already {
                        remove_leaf(tree, dragged)
                    } else {
                        Some(tree)
                    };
                    let Some(mut base) = base else {
                        return;
                    };
                    let (new_first, side_by_side) = match zone {
                        DropZone::Left => (true, true),
                        DropZone::Right => (false, true),
                        DropZone::Top => (true, false),
                        _ => (false, false),
                    };
                    base.split_leaf(target, dragged, new_first, side_by_side);
                    tree = base;
                }
            }
        }
        self.tiles = Some(tree);
        if di != self.active {
            self.active = di;
            self.on_active_changed();
        }
        self.load_tab(di);
        self.fix_tiles();
    }

    /// Ctrl+S: splits the focused pane and opens a copy of its folder in the new pane. The split
    /// cuts across the longer side, so both halves end up closer to square: a wide pane is split
    /// left/right, a tall one top/bottom. Does nothing at the 4-pane limit.
    pub(crate) fn split_focused(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        if tab.special() {
            return;
        }
        let (cur, dir) = (tab.id, tab.dir.clone());
        let mut ids = Vec::new();
        match &self.tiles {
            Some(t) => t.leaf_ids(&mut ids),
            None => ids.push(cur),
        }
        if ids.len() >= 4 {
            return;
        }
        let Some(rect) = self.pane_rects.iter().find(|(id, _)| *id == cur).map(|(_, r)| *r) else {
            return;
        };
        // Flip `>=` to `<` to cut along the other axis instead.
        let side_by_side = rect.width() >= rect.height();
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(Tab::new(id, dir));
        let last = self.tabs.len() - 1;
        self.load_tab(last);
        let mut tree = self.tiles.take().unwrap_or(Tile::Leaf(cur));
        tree.split_leaf(cur, id, false, side_by_side);
        self.tiles = Some(tree);
        self.active = last;
        self.on_active_changed();
        self.fix_tiles();
    }

    /// A tab was released outside the window: open it in another FileFly window under the
    /// pointer, or in a new window of its own.
    pub(crate) fn detach_tab(&mut self, dragged: u64) {
        let Some(ti) = self.tabs.iter().position(|t| t.id == dragged) else {
            return;
        };
        if self.tabs[ti].special() {
            return;
        }
        let Some((cx, cy)) = cursor_screen_pos() else {
            return;
        };
        let only = self.tabs.len() <= 1;
        let everything = self.ui.input(|i| i.modifiers.shift);
        match self.peers.window_at(cx, cy) {
            Some(pid) => {
                if only || everything {
                    // Merge this whole window into the other one (the dragged tab goes last, so it
                    // ends up as the active tab there), then close this window.
                    let mut dirs: Vec<PathBuf> = self
                        .tabs
                        .iter()
                        .filter(|t| t.id != dragged && !t.special())
                        .map(|t| t.dir.clone())
                        .collect();
                    dirs.push(self.tabs[ti].dir.clone());
                    if dirs.iter().all(|d| self.peers.send_tab(pid, d)) {
                        self.ui.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                } else {
                    let dir = self.tabs[ti].dir.clone();
                    if self.peers.send_tab(pid, &dir) {
                        self.apply(Action::CloseTab(ti));
                    }
                }
            }
            None => {
                if only {
                    return; // the only tab already is a window of its own
                }
                let dir = self.tabs[ti].dir.clone();
                let ppp = self.ui.pixels_per_point().max(0.1);
                spawn_window(
                    &dir,
                    Some(((cx / ppp - 80.0).max(0.0), (cy / ppp - 20.0).max(0.0))),
                );
                self.apply(Action::CloseTab(ti));
            }
        }
    }
}
