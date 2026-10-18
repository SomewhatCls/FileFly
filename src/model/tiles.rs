//! The tiling tree that arranges tabs side by side, and drag-and-drop zones.

use crate::prelude::*;

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

impl Tile {
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
