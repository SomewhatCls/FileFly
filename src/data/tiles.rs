//! The tiling tree that arranges tabs side by side, and drag-and-drop zones.

use crate::prelude::*;

/// Most panes that can be open side by side.
pub(crate) const MAX_PANES: usize = 4;

#[derive(Clone)]
pub(crate) enum Tile {
    Leaf(u64),
    Split {
        /// true: a | b (left/right), false: a over b (top/bottom).
        side_by_side: bool,
        ratio: f32,
        a: Box<Tile>,
        b: Box<Tile>,
    },
}

/// Where a split inside `rect` puts its two halves and its divider handle: `(a, b, handle, span)`, where `span`
/// is the length the ratio applies to.
pub(crate) fn split_rects(side_by_side: bool, ratio: f32, rect: Rect, gap: f32) -> (Rect, Rect, Rect, f32) {
    let span = (if side_by_side { rect.width() } else { rect.height() } - gap).max(1.0);
    let split = (span * ratio).round();
    if side_by_side {
        let x = rect.left() + split;
        (
            Rect::from_min_max(rect.min, pos2(x, rect.bottom())),
            Rect::from_min_max(pos2(x + gap, rect.top()), rect.max),
            Rect::from_min_max(pos2(x, rect.top()), pos2(x + gap, rect.bottom())),
            span,
        )
    } else {
        let y = rect.top() + split;
        (
            Rect::from_min_max(rect.min, pos2(rect.right(), y)),
            Rect::from_min_max(pos2(rect.left(), y + gap), rect.max),
            Rect::from_min_max(pos2(rect.left(), y), pos2(rect.right(), y + gap)),
            span,
        )
    }
}

impl Tile {
    /// The pane rectangles this tree gets inside `rect` (the same geometry `layout_tiles` uses).
    pub(crate) fn leaf_rects(&self, rect: Rect, gap: f32, out: &mut Vec<(u64, Rect)>) {
        match self {
            Tile::Leaf(id) => out.push((*id, rect)),
            Tile::Split { side_by_side, ratio, a, b } => {
                let (ra, rb, _, _) = split_rects(*side_by_side, *ratio, rect, gap);
                a.leaf_rects(ra, gap, out);
                b.leaf_rects(rb, gap, out);
            }
        }
    }

    pub(crate) fn leaf_ids(&self, out: &mut Vec<u64>) {
        match self {
            Tile::Leaf(id) => out.push(*id),
            Tile::Split { a, b, .. } => {
                a.leaf_ids(out);
                b.leaf_ids(out);
            }
        }
    }

    pub(crate) fn contains(&self, id: u64) -> bool {
        let mut v = Vec::new();
        self.leaf_ids(&mut v);
        v.contains(&id)
    }

    pub(crate) fn first_leaf(&self) -> u64 {
        match self {
            Tile::Leaf(id) => *id,
            Tile::Split { a, .. } => a.first_leaf(),
        }
    }

    pub(crate) fn map_leaves(&mut self, f: &mut dyn FnMut(u64) -> u64) {
        match self {
            Tile::Leaf(id) => *id = f(*id),
            Tile::Split { a, b, .. } => {
                a.map_leaves(f);
                b.map_leaves(f);
            }
        }
    }

    pub(crate) fn replace(&mut self, old: u64, new: u64) {
        self.map_leaves(&mut |id| if id == old { new } else { id });
    }

    pub(crate) fn swap(&mut self, x: u64, y: u64) {
        self.map_leaves(&mut |id| {
            if id == x {
                y
            } else if id == y {
                x
            } else {
                id
            }
        });
    }

    /// The tree after tab `dragged` was dropped on pane `target`: in the middle of the pane they swap places (or
    /// the dragged tab takes the pane's place when it was not open), on an edge the dragged tab gets a new pane
    /// there (a tab that was already open moves instead of being copied). A tab dropped on its own pane changes
    /// nothing: only the split shortcut duplicates a pane. Past `MAX_PANES` a tab that is not open yet is refused.
    pub(crate) fn drop_onto(mut self, dragged: u64, target: u64, zone: DropZone) -> Tile {
        if dragged == target {
            return self;
        }
        if zone == DropZone::Center {
            if self.contains(dragged) {
                self.swap(dragged, target);
            } else {
                self.replace(target, dragged);
            }
            return self;
        }
        let mut ids = Vec::new();
        self.leaf_ids(&mut ids);
        let already = ids.contains(&dragged);
        if !already && ids.len() >= MAX_PANES {
            return self;
        }
        let (new_first, side_by_side) = match zone {
            DropZone::Left => (true, true),
            DropZone::Right => (false, true),
            DropZone::Top => (true, false),
            _ => (false, false),
        };
        let mut base = if already { remove_leaf(self, dragged).unwrap_or(Tile::Leaf(target)) } else { self };
        base.split_leaf(target, dragged, new_first, side_by_side);
        base
    }

    /// Turns the leaf `target` into a split holding `target` and `new`.
    pub(crate) fn split_leaf(&mut self, target: u64, new: u64, new_first: bool, side_by_side: bool) -> bool {
        match self {
            Tile::Leaf(x) => {
                if *x != target {
                    return false;
                }
                let (a, b) = if new_first {
                    (new, target)
                } else {
                    (target, new)
                };
                *self = Tile::Split {
                    side_by_side,
                    ratio: 0.5,
                    a: Box::new(Tile::Leaf(a)),
                    b: Box::new(Tile::Leaf(b)),
                };
                true
            }
            Tile::Split { a, b, .. } => {
                a.split_leaf(target, new, new_first, side_by_side)
                    || b.split_leaf(target, new, new_first, side_by_side)
            }
        }
    }
}

/// Removes a leaf; its sibling takes over the freed space.
pub(crate) fn remove_leaf(t: Tile, id: u64) -> Option<Tile> {
    match t {
        Tile::Leaf(x) => {
            if x == id {
                None
            } else {
                Some(Tile::Leaf(x))
            }
        }
        Tile::Split {
            side_by_side,
            ratio,
            a,
            b,
        } => match (remove_leaf(*a, id), remove_leaf(*b, id)) {
            (Some(a), Some(b)) => Some(Tile::Split {
                side_by_side,
                ratio,
                a: Box::new(a),
                b: Box::new(b),
            }),
            (Some(x), None) | (None, Some(x)) => Some(x),
            (None, None) => None,
        },
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DropZone {
    Center,
    Left,
    Right,
    Top,
    Bottom,
}

pub(crate) fn drop_zone(r: Rect, p: egui::Pos2, can_split: bool) -> DropZone {
    if !can_split || r.width() < 1.0 || r.height() < 1.0 {
        return DropZone::Center;
    }
    let (u, v) = ((p.x - r.left()) / r.width(), (p.y - r.top()) / r.height());
    let (dl, dr, dt, db) = (u, 1.0 - u, v, 1.0 - v);
    let near = dl.min(dr).min(dt).min(db);
    if near > 0.28 {
        DropZone::Center
    } else if near == dl {
        DropZone::Left
    } else if near == dr {
        DropZone::Right
    } else if near == dt {
        DropZone::Top
    } else {
        DropZone::Bottom
    }
}

pub(crate) fn zone_rect(r: Rect, z: DropZone) -> Rect {
    match z {
        DropZone::Center => r,
        DropZone::Left => Rect::from_min_max(r.min, pos2(r.center().x, r.bottom())),
        DropZone::Right => Rect::from_min_max(pos2(r.center().x, r.top()), r.max),
        DropZone::Top => Rect::from_min_max(r.min, pos2(r.right(), r.center().y)),
        DropZone::Bottom => Rect::from_min_max(pos2(r.left(), r.center().y), r.max),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(t: &Tile) -> Vec<u64> {
        let mut v = Vec::new();
        t.leaf_ids(&mut v);
        v
    }

    fn two() -> Tile {
        Tile::Leaf(1).drop_onto(2, 1, DropZone::Right) // 1 | 2
    }

    #[test]
    fn a_tab_dropped_on_its_own_pane_changes_nothing() {
        for zone in [DropZone::Center, DropZone::Left, DropZone::Top] {
            assert_eq!(ids(&Tile::Leaf(1).drop_onto(1, 1, zone)), vec![1]);
            assert_eq!(ids(&two().drop_onto(2, 2, zone)), vec![1, 2]);
        }
    }

    #[test]
    fn single_pane_self_drop_moves_the_tab_next_to_the_tab_left_behind() {
        // `Explorer::drop_tab` retargets a single-pane self-drop to the other tab (7) that stays in the old pane.
        assert_eq!(ids(&Tile::Leaf(7).drop_onto(1, 7, DropZone::Left)), vec![1, 7]);
        assert_eq!(ids(&Tile::Leaf(7).drop_onto(1, 7, DropZone::Right)), vec![7, 1]);
        assert_eq!(ids(&Tile::Leaf(7).drop_onto(1, 7, DropZone::Top)), vec![1, 7]);
        assert_eq!(ids(&Tile::Leaf(7).drop_onto(1, 7, DropZone::Bottom)), vec![7, 1]);
    }

    #[test]
    fn moving_a_pane_next_to_its_sibling_frees_the_old_split() {
        // 1 over 2; pane 2 dropped on the right of pane 1: the old split goes away, 1 | 2 fill the whole area.
        let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 100.0));
        let t = Tile::Leaf(1).drop_onto(2, 1, DropZone::Bottom).drop_onto(2, 1, DropZone::Right);
        let mut r = Vec::new();
        t.leaf_rects(full, 0.0, &mut r);
        assert_eq!(r, vec![(1, zone_rect(full, DropZone::Left)), (2, zone_rect(full, DropZone::Right))]);
    }

    #[test]
    fn dropping_in_the_middle_swaps_open_tabs() {
        assert_eq!(ids(&two().drop_onto(2, 1, DropZone::Center)), vec![2, 1]);
    }

    #[test]
    fn dropping_on_an_edge_moves_an_open_tab_instead_of_copying_it() {
        let t = two().drop_onto(3, 2, DropZone::Bottom); // 1 | (2 over 3)
        assert_eq!(ids(&t), vec![1, 2, 3]);
        let t = t.drop_onto(1, 3, DropZone::Top); // 2 over 1 over 3, 1 moved
        let mut v = ids(&t);
        v.sort();
        assert_eq!(v, vec![1, 2, 3]);
    }

    #[test]
    fn a_closed_tab_takes_the_place_in_the_middle_and_is_refused_past_the_limit() {
        assert_eq!(ids(&two().drop_onto(9, 2, DropZone::Center)), vec![1, 9]);
        let mut t = two();
        for n in 3..=MAX_PANES as u64 {
            t = t.drop_onto(n, 1, DropZone::Bottom);
        }
        assert_eq!(ids(&t).len(), MAX_PANES);
        assert_eq!(ids(&t.drop_onto(99, 1, DropZone::Left)).len(), MAX_PANES);
    }
}
