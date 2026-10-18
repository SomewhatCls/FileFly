//! State and helpers of the Recycle Bin view.

use crate::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrashSort {
    Name,
    Location,
    Deleted,
    Size,
}

pub(crate) struct TrashState {
    pub(crate) rows: Vec<trashbin::TrashRow>,
    pub(crate) view: Vec<usize>,
    pub(crate) selected: HashSet<usize>,
    pub(crate) anchor: Option<usize>,
    pub(crate) loading: bool,
    pub(crate) seq: u64,
    pub(crate) error: Option<String>,
    pub(crate) sort_col: TrashSort,
    pub(crate) sort_asc: bool,
    pub(crate) needs_sort: bool,
    pub(crate) scroll_to: Option<usize>,
}

impl TrashState {
    pub(crate) fn new() -> Self {
        Self {
            rows: Vec::new(),
            view: Vec::new(),
            selected: HashSet::new(),
            anchor: None,
            loading: true,
            seq: 0,
            error: None,
            sort_col: TrashSort::Deleted,
            sort_asc: false,
            needs_sort: false,
            scroll_to: None,
        }
    }
}

pub(crate) struct ConfirmPurge {
    pub(crate) items: Vec<trashbin::Item>,
    pub(crate) text: String,
}

pub(crate) fn trash_move(tr: &mut TrashState, delta: i32) {
    if tr.view.is_empty() {
        return;
    }
    let cur = tr.anchor.filter(|&a| a < tr.view.len());
    let next = match cur {
        Some(c) => (c as i32 + delta).clamp(0, tr.view.len() as i32 - 1) as usize,
        None => 0,
    };
    tr.selected.clear();
    tr.selected.insert(tr.view[next]);
    tr.anchor = Some(next);
    tr.scroll_to = Some(next);
}

pub(crate) fn trash_edge(tr: &mut TrashState, last: bool) {
    if tr.view.is_empty() {
        return;
    }
    let i = if last { tr.view.len() - 1 } else { 0 };
    tr.selected.clear();
    tr.selected.insert(tr.view[i]);
    tr.anchor = Some(i);
    tr.scroll_to = Some(i);
}

pub(crate) fn rebuild_trash_view(tr: &mut TrashState, search: &str) {
    let q = search.trim().to_lowercase();
    let rows = &tr.rows;
    tr.view.clear();
    tr.view.extend((0..rows.len()).filter(|&i| {
        q.is_empty() || rows[i].name_lc.contains(&q) || rows[i].location_lc.contains(&q)
    }));
    let (col, asc) = (tr.sort_col, tr.sort_asc);
    tr.view.sort_by(|&a, &b| {
        let (ra, rb) = (&rows[a], &rows[b]);
        let o = match col {
            TrashSort::Name => natural_cmp(&ra.name_lc, &rb.name_lc),
            TrashSort::Location => ra
                .location_lc
                .cmp(&rb.location_lc)
                .then_with(|| natural_cmp(&ra.name_lc, &rb.name_lc)),
            TrashSort::Deleted => ra.deleted.cmp(&rb.deleted),
            TrashSort::Size => ra.size.cmp(&rb.size),
        };
        if asc {
            o
        } else {
            o.reverse()
        }
    });
    tr.needs_sort = false;
}
