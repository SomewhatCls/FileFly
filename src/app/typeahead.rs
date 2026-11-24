//! Type-to-select in the file list: typing the start of a name selects the first item that begins with it.

use crate::prelude::*;

/// How long a pause ends the typed string: the next key starts a new one.
const TIMEOUT: Duration = Duration::from_millis(1000);

pub(crate) struct TypeAhead {
    text: String,
    last_key: Instant,
}

impl Default for TypeAhead {
    fn default() -> Self {
        Self { text: String::new(), last_key: Instant::now() }
    }
}

impl TypeAhead {
    fn active(&self) -> bool {
        !self.text.is_empty() && self.last_key.elapsed() < TIMEOUT
    }
}

/// The system's error sound (nothing elsewhere).
fn error_beep() {
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        unsafe extern "system" {
            fn MessageBeep(kind: u32) -> i32;
        }
        const MB_ICONHAND: u32 = 0x10;
        // SAFETY: plain call with a constant argument.
        unsafe { MessageBeep(MB_ICONHAND) };
    }
}

impl Explorer {
    /// Extends the typed string by `typed` (or starts a new one after a pause) and selects the first
    /// item of the list, in the order shown, whose name starts with it. Without a match the selection
    /// stays where it is.
    pub(crate) fn type_ahead(&mut self, ti: usize, typed: &str) {
        let ta = &mut self.typeahead;
        if !ta.active() {
            ta.text.clear();
        }
        ta.last_key = Instant::now();
        if ta.text.is_empty() && typed.trim().is_empty() {
            return; // a leading space is not part of a name
        }
        ta.text.push_str(typed);
        let needle = ta.text.to_lowercase();
        let tab = &mut self.tabs[ti];
        if tab.special() {
            return;
        }
        let src = tab.source();
        let starts = |pos: usize, needle: &str| src[tab.view[pos]].name.to_lowercase().starts_with(needle);
        let mut hit = (0..tab.view.len()).find(|&pos| starts(pos, &needle));
        // The same key again and again ("aaa") steps through the names starting with that letter,
        // from the selected item on, unless a name starts with the whole string.
        let mut chars = needle.chars();
        let first = chars.next();
        if hit.is_none() && needle.chars().count() > 1 && chars.all(|c| Some(c) == first) {
            let letter = needle.chars().next().map(String::from).unwrap_or_default();
            let current = tab.anchor.as_ref().and_then(|a| tab.index_of(a)).and_then(|i| tab.view.iter().position(|&v| v == i));
            let n = tab.view.len();
            let start = current.map_or(0, |c| c + 1);
            hit = (0..n).map(|k| (start + k) % n).find(|&pos| starts(pos, &letter));
        }
        match hit {
            Some(pos) => {
                let path = src[tab.view[pos]].path.clone();
                tab.select_only(path);
                tab.scroll_to = Some(pos);
                tab.scroll_center = true;
            }
            None => error_beep(),
        }
    }

    /// The typed string as a subtle label in the corner of the active pane while it is still valid.
    pub(crate) fn draw_type_ahead(&self, ui: &Ui, panes: &[(u64, Rect)]) {
        let ta = &self.typeahead;
        if !ta.active() {
            return;
        }
        let Some(&(_, rect)) = panes.iter().find(|(id, _)| *id == self.tabs[self.active].id) else { return };
        let (m, pal) = (&self.m, &self.pal);
        let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, Id::unique("type_ahead")));
        let galley = painter.layout_no_wrap(ta.text.clone(), FontId::proportional(m.font), pal.text);
        let pad = m.pad;
        let size = galley.size() + vec2(pad * 2.0, pad);
        let r = Rect::from_min_size(pos2(rect.left() + pad, rect.bottom() - pad - size.y), size);
        painter.rect_filled(r, CornerRadius::same(m.radius), pal.flyout);
        painter.rect_stroke(r, CornerRadius::same(m.radius), Stroke::new(1.0, pal.control_stroke), StrokeKind::Inside);
        painter.galley(r.min + vec2(pad, pad * 0.5), galley, pal.text);
        // Redraw once more when it expires, so the label goes away without further input.
        ui.ctx().request_repaint_after(TIMEOUT.saturating_sub(ta.last_key.elapsed()) + Duration::from_millis(20));
    }
}
