//! Central panel: tile layout and tab bodies.

use crate::prelude::*;

// ==============================================================================================
// Panels
//window decoration radius for linux
pub(crate) const RADIUS: u8 = if cfg!(target_os = "linux") { 12 } else { 0 };

/// The contents of one pane: the file table (or the Recycle Bin / Settings page) of `tab`, inside `rect`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn tab_body(
    ui: &mut Ui,
    rect: Rect,
    m: &Metrics,
    pal: &Palette,
    tab: &mut Tab,
    cols: &mut [f32; 3],
    cut: Option<&HashSet<PathBuf>>,
    can_paste: bool,
    has_peazip: bool,
    actions: &mut Vec<Action>,
    sp: Option<&mut SettingsPage>,
) {
    let tab_id = tab.id;
    if tab.settings {
        if let Some(sp) = sp {
            // Same side gutter as the file view, so the scroll bar does not sit on the window edge.
            let inner = Rect::from_min_max(
                pos2(rect.left() + m.pad, rect.top()),
                pos2(rect.right() - m.pad, rect.bottom()),
            );
            settings_page(ui, inner, m, pal, tab_id, sp);
        }
        return;
    }
    if tab.trash.is_some() {
        let Tab {
            trash,
            search,
            view_dirty,
            ..
        } = tab;
        let tr = trash.as_mut().unwrap();
        if *view_dirty || tr.needs_sort {
            rebuild_trash_view(tr, search);
            *view_dirty = false;
        }
        let searching = !search.trim().is_empty();
        let inner = rect.shrink2(vec2(m.pad * 0.5, 0.0));
        ui.scope_builder(
            egui::UiBuilder::new()
                .id_salt(("tab_body", tab_id))
                .max_rect(inner),
            |ui| {
                trash_table_ui(ui, m, pal, tr, searching, actions);
            },
        );
        return;
    }
    if tab.view_dirty {
        rebuild_view(tab);
    }
    // A little breathing room left and right of the list.
    let gutter = m.pad;
    let inner = Rect::from_min_max(
        pos2(rect.left() + gutter, rect.top()),
        pos2(rect.right() - gutter, rect.bottom()),
    );
    ui.scope_builder(
        egui::UiBuilder::new()
            .id_salt(("tab_body", tab_id))
            .max_rect(inner),
        |ui| {
            table_ui(
                ui, m, pal, tab, cols, cut, can_paste, has_peazip, rect, actions,
            );
        },
    );
}

/// Lays the tile tree out inside `rect`, handling the draggable dividers on the way.
/// Collects (tab id, pane rectangle) for every leaf.
pub(crate) fn layout_tiles(
    ui: &mut Ui,
    t: &mut Tile,
    rect: Rect,
    m: &Metrics,
    pal: &Palette,
    out: &mut Vec<(u64, Rect)>,
) {
    match t {
        Tile::Leaf(id) => out.push((*id, rect)),
        Tile::Split {
            side_by_side,
            ratio,
            a,
            b,
        } => {
            let gap = m.s * 5.0;
            let span = (if *side_by_side {
                rect.width()
            } else {
                rect.height()
            } - gap)
                .max(1.0);
            let split = (span * *ratio).round();
            let (ra, rb, handle) = if *side_by_side {
                let x = rect.left() + split;
                (
                    Rect::from_min_max(rect.min, pos2(x, rect.bottom())),
                    Rect::from_min_max(pos2(x + gap, rect.top()), rect.max),
                    Rect::from_min_max(pos2(x, rect.top()), pos2(x + gap, rect.bottom())),
                )
            } else {
                let y = rect.top() + split;
                (
                    Rect::from_min_max(rect.min, pos2(rect.right(), y)),
                    Rect::from_min_max(pos2(rect.left(), y + gap), rect.max),
                    Rect::from_min_max(pos2(rect.left(), y), pos2(rect.right(), y + gap)),
                )
            };
            let id = Id::unique(("tile_divider", a.first_leaf(), b.first_leaf()));
            let resp = ui.interact(handle, id, Sense::drag());
            let hot = resp.hovered() || resp.dragged();
            if hot {
                ui.set_cursor_icon(if *side_by_side {
                    egui::CursorIcon::ResizeHorizontal
                } else {
                    egui::CursorIcon::ResizeVertical
                });
            }
            if resp.dragged() {
                let d = if *side_by_side {
                    resp.drag_delta().x
                } else {
                    resp.drag_delta().y
                };
                *ratio = (*ratio + d / span).clamp(0.15, 0.85);
            }
            let c = handle.center();
            let line = if *side_by_side {
                [pos2(c.x, handle.top()), pos2(c.x, handle.bottom())]
            } else {
                [pos2(handle.left(), c.y), pos2(handle.right(), c.y)]
            };
            ui.painter().line_segment(
                line,
                Stroke::new(
                    if hot { 2.0_f32 } else { 1.0_f32 },
                    if hot { pal.accent } else { pal.divider },
                ),
            );
            layout_tiles(ui, a, ra, m, pal, out);
            layout_tiles(ui, b, rb, m, pal, out);
        }
    }
}

impl Explorer {
    pub(crate) fn ui_central(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        self.fix_tiles();
        let (m, pal) = (self.m, self.pal);
        // The Settings tab edits a working copy of the settings (see `finish_settings_page`).
        let mut sp: Option<SettingsPage> = if self.visible_tabs().iter().any(|&i| self.tabs[i].settings) {
            Some(SettingsPage::new(
                &self.settings,
                self.font_edit,
                self.index_dir.display().to_string(),
                self.indexer.len(),
                self.default_on,
                self.default_error.clone(),
            ))
        } else {
            None
        };
        let can_paste = get_clipboard().is_some();
        let has_peazip = self.peazip.is_some();
        let cut_set: Option<HashSet<PathBuf>> = self
            .clipboard
            .as_ref()
            .filter(|c| c.1)
            .map(|c| c.0.iter().cloned().collect());
        let active = self.active;
        let mut panes: Vec<(u64, Rect)> = Vec::new();
        let mut focus: Option<usize> = None;
        let mut tree = self.tiles.take();
        let frame = layer_frame(&pal, m.radius, 0.0, 0.0, 0.0, 0.0, false);

        match tree.as_mut() {
            None => {
                let cols = &mut self.settings.col_widths;
                let tab = &mut self.tabs[active];
                let tab_id = tab.id;
                egui::CentralPanel::default().frame(frame).show(ui, |ui| {
                    let rect = ui.max_rect();
                    panes.push((tab_id, rect));
                    tab_body(
                        ui,
                        rect,
                        &m,
                        &pal,
                        tab,
                        cols,
                        cut_set.as_ref(),
                        can_paste,
                        has_peazip,
                        actions,
                        sp.as_mut(),
                    );
                });
            }
            Some(t) => {
                let tabs = &mut self.tabs;
                let cols = &mut self.settings.col_widths;
                egui::CentralPanel::default().frame(frame).show(ui, |ui| {
                    let full = ui.max_rect();
                    let mut rects: Vec<(u64, Rect)> = Vec::new();
                    layout_tiles(ui, t, full, &m, &pal, &mut rects);
                    let press = ui.input(|i| {
                        if i.pointer.any_pressed() {
                            i.pointer.interact_pos()
                        } else {
                            None
                        }
                    });
                    let hdr_h = m.row_h;
                    for (tid, rect) in rects {
                        let Some(idx) = tabs.iter().position(|x| x.id == tid) else {
                            continue;
                        };
                        panes.push((tid, rect));
                        let is_active = idx == active;
                        // Clicking anywhere in a pane focuses it (but not through a menu or dialog on top of it).
                        if let Some(p) = press {
                            let covered = ui.layer_id_at(p).is_some_and(|l| l != ui.layer_id());
                            if !is_active && !covered && rect.contains(p) {
                                focus = Some(idx);
                            }
                        }
                        let hdr = Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + hdr_h));
                        let body = Rect::from_min_max(pos2(rect.left(), hdr.bottom()), rect.max);

                        // Pane title strip: folder name, and a button that closes just this pane.
                        ui.painter()
                            .rect_filled(hdr, CornerRadius::ZERO, pal.subtle_pressed);
                        if is_active {
                            ui.painter().rect_filled(
                                Rect::from_min_size(hdr.min, vec2(hdr.width(), m.s * 2.0)),
                                CornerRadius::ZERO,
                                pal.accent,
                            );
                        }
                        let in_bin = tabs[idx].trash.is_some();
                        draw_icon(
                            ui,
                            pos2(hdr.left() + m.pad + m.small_icon / 2.0, hdr.center().y),
                            m.small_icon,
                            if in_bin {
                                "recycle_bin"
                            } else if tabs[idx].settings {
                                "settings"
                            } else {
                                "folder"
                            },
                            Color32::WHITE,
                        );
                        let close_rect = Rect::from_center_size(
                            pos2(hdr.right() - m.pad - m.small_icon / 2.0, hdr.center().y),
                            vec2(m.ctl_h * 0.8, m.ctl_h * 0.8),
                        );
                        let title_rect = Rect::from_min_max(
                            pos2(hdr.left() + m.pad * 1.7 + m.small_icon, hdr.top()),
                            pos2(close_rect.left() - m.pad * 0.3, hdr.bottom()),
                        );
                        paint_text(
                            ui,
                            title_rect,
                            &tabs[idx].title(),
                            FontId::proportional(m.font * 0.93),
                            if is_active {
                                pal.text
                            } else {
                                pal.text_secondary
                            },
                            false,
                        );
                        let cr = ui.interact(close_rect, Id::unique(("pane_close", tid)), Sense::click());
                        hover_fill(ui, close_rect, &cr, &pal, &m);
                        draw_icon(ui, close_rect.center(), m.small_icon * 0.8, "close", pal.text_secondary);
                        cr.clone().on_hover_text("Close this pane");
                        if cr.clicked() {
                            actions.push(Action::ClosePane(tid));
                        }

                        let tab = &mut tabs[idx];
                        ui.scope_builder(
                            egui::UiBuilder::new()
                                .id_salt(("pane", tid))
                                .max_rect(body),
                            |ui| {
                                ui.set_clip_rect(body.intersect(ui.clip_rect()));
                                tab_body(
                                    ui,
                                    body,
                                    &m,
                                    &pal,
                                    tab,
                                    cols,
                                    cut_set.as_ref(),
                                    can_paste,
                                    has_peazip,
                                    actions,
                                    sp.as_mut(),
                                );
                            },
                        );
                    }
                });
            }
        }
        self.tiles = tree;
        self.pane_rects = panes.clone();
        if let Some(sp) = sp {
            self.finish_settings_page(ui, sp);
        }

        if let Some(i) = focus {
            if i != self.active && i < self.tabs.len() {
                self.active = i;
                self.on_active_changed();
                self.load_tab(i); // the pane may have been out of date: only the focused folder is watched
            }
        }
        self.tab_drag_overlay(ui, &panes);
    }
}
