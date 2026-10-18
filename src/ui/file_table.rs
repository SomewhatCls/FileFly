//! The file list: rows, rubber-band selection, context menus.

use crate::prelude::*;

pub(crate) fn select_range(ui: &mut Ui, id: Id, a: usize, b: usize) {
    let mut state = egui::TextEdit::load_state(ui, id).unwrap_or_default();
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(a),
            egui::text::CCursor::new(b),
        )));
    state.store(ui, id);
}

/// Fill for a selected row. Deliberately stronger than the generic accent wash used elsewhere.
pub(crate) fn sel_fill(pal: &Palette, hovered: bool) -> Color32 {
    let base = if hovered {
        pal.selected_hover
    } else {
        pal.selected
    };
    base.gamma_multiply(1.55)
}

/// Fluent-style accent indicator on the left edge of a selected row.
pub(crate) fn paint_sel_marker(ui: &Ui, rect: Rect, m: &Metrics, pal: &Palette) {
    let marker = Rect::from_center_size(
        pos2(rect.left() + m.s * 4.0, rect.center().y),
        vec2(m.s * 3.0, rect.height() * 0.5),
    );
    ui.painter()
        .rect_filled(marker, CornerRadius::same(2), pal.accent);
}

#[derive(Clone, Default)]
pub(crate) struct Rubber {
    /// Where the button went down (screen space) - used for the drag threshold.
    pub(crate) press: egui::Pos2,
    /// x in screen space, y in *content* space, so the band stays anchored while the list scrolls.
    pub(crate) origin: egui::Pos2,
    pub(crate) active: bool,
    pub(crate) additive: bool,
}

pub(crate) struct BandUpdate {
    /// First frame of this drag: the caller should snapshot the selection it wants to keep.
    pub(crate) started: bool,
    /// Ctrl/Shift was held on press: keep the old selection and add to it.
    pub(crate) additive: bool,
    /// Inclusive range of list rows touched by the band.
    pub(crate) span: Option<(usize, usize)>,
    /// Row under the pointer (becomes the keyboard anchor).
    pub(crate) end: Option<usize>,
    /// Band rectangle in screen space, clipped to the list viewport.
    pub(crate) rect: Rect,
}

/// Drag-to-select for a virtualised, fixed-row-height list. Call it from inside the `show_rows`
/// closure (before drawing the rows) with the y of the first row of the whole list.
pub(crate) fn rubber_band(
    ui: &mut Ui,
    m: &Metrics,
    id: Id,
    content_top: f32,
    row_h: f32,
    rows: usize,
    enabled: bool,
    hit: Rect,
) -> Option<BandUpdate> {
    let viewport = ui.clip_rect();
    let (pressed, down, pos, latest, dt, mods) = ui.input(|i| {
        (
            i.pointer.primary_pressed(),
            i.pointer.primary_down(),
            i.pointer.interact_pos(),
            i.pointer.latest_pos(),
            i.stable_dt,
            i.modifiers,
        )
    });
    let mut state: Option<Rubber> = ui.data_mut(|d| d.get_temp(id));

    if pressed && enabled && state.is_none() {
        if let Some(p) = pos {
            let on_scrollbar = viewport.contains(p) && p.x > viewport.right() - m.s * 14.0;
            // layer_id_at() is None over plain panels and Some(..) over dialogs/menus/popups.
            let covered = ui.layer_id_at(p).is_some_and(|l| l != ui.layer_id());
            // `hit` is the viewport plus the blank margins around it: a drag may start there too.
            if hit.contains(p) && !on_scrollbar && !covered {
                state = Some(Rubber {
                    press: p,
                    origin: pos2(p.x, p.y - content_top),
                    active: false,
                    additive: mods.command || mods.shift,
                });
            }
        }
    }
    let mut r = state?;
    if !down {
        ui.data_mut(|d| d.remove_temp::<Rubber>(id));
        return None;
    }
    let Some(p) = latest else {
        ui.data_mut(|d| d.insert_temp(id, r));
        return None;
    };

    // Stay clear of egui's own click threshold (6 px) so a drag never also counts as a click.
    let mut started = false;
    if !r.active && (p - r.press).length() > 7.0 {
        r.active = true;
        started = true;
    }
    let mut out = None;
    if r.active {
        ui.request_repaint();

        // Auto-scroll while the pointer is above/below the list.
        let dy = if p.y > viewport.bottom() {
            p.y - viewport.bottom()
        } else if p.y < viewport.top() {
            p.y - viewport.top()
        } else {
            0.0
        };
        if dy != 0.0 {
            ui.scroll_with_delta(vec2(0.0, -dy.clamp(-80.0, 80.0) * 10.0 * dt));
        }

        let cur_y = p.y - content_top;
        let (y0, y1) = (r.origin.y.min(cur_y), r.origin.y.max(cur_y));
        let span = if rows == 0 || y1 < 0.0 || y0 >= rows as f32 * row_h {
            None
        } else {
            let lo = (y0.max(0.0) / row_h).floor() as usize;
            let hi = ((y1 / row_h).floor() as usize).min(rows - 1);
            Some((lo.min(rows - 1), hi))
        };
        let end = if rows == 0 {
            None
        } else {
            Some(((cur_y / row_h).floor().max(0.0) as usize).min(rows - 1))
        };
        let origin_screen = pos2(r.origin.x, r.origin.y + content_top);
        out = Some(BandUpdate {
            started,
            additive: r.additive,
            span,
            end,
            rect: Rect::from_two_pos(origin_screen, p).intersect(viewport),
        });
    }
    ui.data_mut(|d| d.insert_temp(id, r));
    out
}

pub(crate) fn paint_band(ui: &Ui, pal: &Palette, rect: Rect) {
    ui.painter()
        .rect_filled(rect, CornerRadius::same(2), pal.accent.gamma_multiply(0.18));
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(2),
        Stroke::new(1.0_f32, pal.accent),
        StrokeKind::Inside,
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn table_ui(
    ui: &mut Ui,
    m: &Metrics,
    pal: &Palette,
    tab: &mut Tab,
    cols: &mut [f32; 3],
    cut: Option<&HashSet<PathBuf>>,
    can_paste: bool,
    has_peazip: bool,
    hit: Rect,
    actions: &mut Vec<Action>,
) {
    let Tab {
        id: tab_id,
        dir,
        entries,
        search_results,
        search_job,
        view,
        selected,
        anchor,
        rename,
        scroll_to,
        sort_col,
        sort_asc,
        view_dirty,
        error,
        loading,
        search,
        ..
    } = tab;
    let tab_id = *tab_id;
    // `view` holds indices into whichever list is on screen.
    let searching_now = !search.trim().is_empty();
    let entries: &Vec<Entry> = if searching_now {
        &*search_results
    } else {
        &*entries
    };
    let avail = (ui.available_width() - m.pad * 0.5).max(100.0);

    // ---- error bar (Fluent InfoBar)
    if let Some(msg) = error.clone() {
        let offer = ui.data(|d| d.get_temp::<u64>(Id::new("admin_retry_tab"))) == Some(tab_id);
        let btn_w = if offer { m.s * 150.0 } else { 0.0 };
        let (r, _) = ui.allocate_exact_size(vec2(avail, m.row_h * 1.1), Sense::hover());
        ui.painter()
            .rect_filled(r, CornerRadius::same(m.radius), pal.danger_bg);
        let tr = Rect::from_min_max(
            r.min + vec2(m.pad, 0.0),
            r.max - vec2(m.pad * 3.0 + m.small_icon + btn_w, 0.0),
        );
        paint_text(
            ui,
            tr,
            &msg,
            FontId::proportional(m.font),
            pal.danger,
            false,
        );
        let cr = Rect::from_center_size(
            pos2(r.right() - m.pad - m.small_icon / 2.0, r.center().y),
            vec2(m.ctl_h * 0.8, m.ctl_h * 0.8),
        );
        if offer {
            let right = r.right() - m.pad * 3.0 - m.small_icon;
            let br = Rect::from_min_max(
                pos2(right - btn_w + m.pad, r.top() + m.row_h * 0.15),
                pos2(right, r.bottom() - m.row_h * 0.15),
            );
            let resp = ui.interact(br, Id::new(("err_admin", tab_id)), Sense::click());
            hover_fill(ui, br, &resp, pal, m);
            ui.painter().text(
                br.center(),
                Align2::CENTER_CENTER,
                "Try as administrator",
                FontId::proportional(m.font * 0.95),
                pal.danger,
            );
            if resp.clicked() {
                actions.push(Action::RetryAsAdmin);
            }
        }
        let resp = ui.interact(cr, Id::new(("err_close", tab_id)), Sense::click());
        hover_fill(ui, cr, &resp, pal, m);
        draw_icon(ui, cr.center(), m.small_icon * 0.8, "close", pal.danger);
        if resp.clicked() {
            *error = None;
        }
        ui.add_space(m.pad * 0.5);
    }

    // ---- column geometry: Name flexes; Type, then Date, then Size are dropped when space runs out
    // Each pane keeps its own widths. Whether a column fits depends on the pane's width, so
    // one shared set made a column vanish in a narrower pane when another pane was resized.
    // A pane starts from the shared widths; the most recent resize anywhere becomes the
    // shared default (used for new panes and saved to settings).
    let cols_id = Id::new(("tab_cols", tab_id));
    let base_cols = ui.data(|d| d.get_temp::<[f32; 3]>(cols_id)).unwrap_or(*cols);
    // No column is ever hidden. When the pane is too narrow for the stored widths, the data
    // columns are squeezed (never below `col_floor`) so Name keeps its preferred width; a wider
    // pane gets the stored widths back. (Hiding columns with a `>` test also flickered: a drag
    // that exactly filled the pane flipped it on rounding error.)
    let name_min = m.s * 220.0;
    let col_floor = m.s * 50.0;
    let want = [base_cols[0] * m.s, base_cols[1] * m.s, base_cols[2] * m.s];
    let room = (avail - name_min).max(0.0);
    let sum_want: f32 = want.iter().sum();
    let eff_px: [f32; 3] = if sum_want <= room {
        want
    } else {
        let floors = 3.0 * col_floor;
        let excess = (sum_want - floors).max(f32::EPSILON);
        let k = ((room.max(floors) - floors) / excess).clamp(0.0, 1.0);
        [0, 1, 2].map(|i| col_floor + (want[i] - col_floor).max(0.0) * k)
    };
    // The widths actually on screen, in stored units: drags start from these.
    let eff_cols = [eff_px[0] / m.s, eff_px[1] / m.s, eff_px[2] / m.s];
    let w = |i: usize| eff_px[i];
    let show = [true; 3];
    let name_w = (avail - eff_px.iter().sum::<f32>()).max(0.0);
    let last_shown = Some(2usize);
    let first_shown = Some(0usize);
    let mut new_cols = base_cols;

    // ---- header
    let header_h = m.row_h * 0.95;
    let (hrect, _) = ui.allocate_exact_size(vec2(avail, header_h), Sense::hover());
    let labels = ["Name", "Date modified", "Type", "Size"];
    let sorts = [
        SortColumn::Name,
        SortColumn::Modified,
        SortColumn::Type,
        SortColumn::Size,
    ];
    let mut x = hrect.left();
    for c in 0..4 {
        if c > 0 && !show[c - 1] {
            continue;
        }
        let cw = if c == 0 { name_w } else { w(c - 1) };
        let crect = Rect::from_min_size(pos2(x, hrect.top()), vec2(cw, header_h));
        x += cw;
        let resp = ui.interact(crect, Id::new(("hdr", tab_id, c)), Sense::click());
        hover_fill(ui, crect.shrink2(vec2(m.s * 2.0, m.s * 2.0)), &resp, pal, m);
        let active = *sort_col == sorts[c];
        let color = if active { pal.text } else { pal.text_secondary };
        let right = c == 3;
        let text_left = if c == 0 {
            crect.left() + m.pad * 1.7 + m.icon
        } else {
            crect.left() + m.pad * 0.6
        };
        let tr = Rect::from_min_max(
            pos2(text_left, crect.top()),
            pos2(crect.right() - m.pad * 0.6, crect.bottom()),
        );
        paint_text(
            ui,
            tr,
            labels[c],
            FontId::proportional(m.font * 0.93),
            color,
            right,
        );
        if active {
            let tw = text_width(ui, labels[c], m.font * 0.93);
            let ax = if right {
                tr.right() - tw - m.small_icon * 0.7
            } else {
                (text_left + tw + m.small_icon * 0.7).min(crect.right() - m.small_icon * 0.5)
            };
            draw_icon(
                ui,
                pos2(ax, crect.center().y),
                m.small_icon * 0.75,
                if *sort_asc {
                    "sort_ascending"
                } else {
                    "sort_descending"
                },
                pal.text_secondary,
            );
        }
        if resp.clicked() {
            if active {
                *sort_asc = !*sort_asc;
            } else {
                *sort_col = sorts[c];
                *sort_asc = true;
            }
            *view_dirty = true;
        }

        // Resize handle on this column's right edge.
        //  * Name | first column: the first column changes, Name absorbs the difference.
        //  * Between two data columns: the two trade width, so every other boundary stays put
        //    and the dragged boundary follows the pointer.
        // The last column's right edge is the table edge and has no handle.
        let boundary = if c == 0 {
            first_shown.map(|k| (k, None))
        } else if Some(c - 1) != last_shown {
            (c..3).find(|&j| show[j]).map(|k2| (c - 1, Some(k2)))
        } else {
            None
        };
        if let Some((k, next)) = boundary {
            let hr = Rect::from_center_size(
                pos2(crect.right(), crect.center().y),
                vec2(m.s * 8.0, header_h),
            );
            let hid = Id::new(("resize", tab_id, c));
            let h = ui.interact(hr, hid, Sense::drag());
            let hot = h.hovered() || h.dragged();
            if hot {
                ui.set_cursor_icon(egui::CursorIcon::ResizeColumn);
            }
            if h.drag_started() {
                ui.data_mut(|d| d.insert_temp(hid, eff_cols));
            }
            if h.dragged() {
                // Work from the widths at drag start and the total pointer movement, so
                // clamping never lets the boundary drift away from the pointer.
                let start = ui.data(|d| d.get_temp::<[f32; 3]>(hid));
                if let (Some(start), Some(total_drag)) = (start, h.total_drag_delta()) {
                    const MIN_W: f32 = 50.0;
                    const MAX_W: f32 = 600.0;
                    let dx = total_drag.x / m.s;
                    let mut nc = start;
                    match next {
                        None => {
                            let shown_sum: f32 = (0..3).filter(|&i| show[i]).map(|i| start[i]).sum();
                            // Name must keep its minimum width, or other columns would vanish.
                            let room = (avail / m.s - name_min / m.s - shown_sum).max(0.0);
                            let hi = (start[k] + room).min(MAX_W).max(start[k]);
                            let lo = MIN_W.min(start[k]);
                            nc[k] = (start[k] - dx).clamp(lo, hi);
                        }
                        Some(k2) => {
                            let pair = start[k] + start[k2];
                            let lo = MIN_W.max(pair - MAX_W);
                            let hi = MAX_W.min(pair - MIN_W);
                            if lo <= hi {
                                nc[k] = (start[k] + dx).clamp(lo, hi);
                                nc[k2] = pair - nc[k];
                            }
                        }
                    }
                    new_cols = nc;
                }
            }
            let ly = header_h * if hot { 0.15 } else { 0.28 };
            ui.painter().line_segment(
                [
                    pos2(crect.right(), crect.top() + ly),
                    pos2(crect.right(), crect.bottom() - ly),
                ],
                Stroke::new(1.0_f32, if hot { pal.text_secondary } else { pal.divider }),
            );
        }
    }
    if new_cols != base_cols {
        COLS_TOUCHED.store(true, AtomicOrdering::Relaxed);
        *cols = new_cols;
    }
    ui.data_mut(|d| {
        d.insert_temp(cols_id, new_cols);
    });
    ui.painter().line_segment(
        [
            pos2(hrect.left(), hrect.bottom()),
            pos2(hrect.right(), hrect.bottom()),
        ],
        Stroke::new(1.0_f32, pal.divider),
    );

    // ---- body
    ui.add_space(m.pad * 0.3);
    let body_rect = ui.available_rect_before_wrap();
    // The click/drag area reaches into the blank side margins, so a selection drag (or a click that
    // clears the selection) can start next to the rows instead of having to hit a row exactly.
    let band_hit = Rect::from_min_max(pos2(hit.left(), body_rect.top()), hit.max);
    let body_resp = ui.interact(band_hit, Id::new(("body", tab_id)), Sense::click());
    if body_resp.clicked() {
        selected.clear();
        *anchor = None;
    }
    scroll_context_menu(&body_resp, |ui| {
        let ro = archive_of(dir).is_some(); // inside an opened zip: nothing can be added
        if menu_item(ui, m, pal, Some("new_folder"), "New folder", Some("Ctrl+Shift+N"), !ro, ) {
            actions.push(Action::NewFolder);
            
        }
        if menu_item(
            ui, m, pal, Some("paste"), "Paste", Some("Ctrl+V"), can_paste && !ro, ) {
            actions.push(Action::Paste);
            
        }
        if menu_item(ui, m, pal, Some("code"), "Open in Terminal", None, true) {
            actions.push(Action::OpenTerminal(dir.clone()));
            
        }
        if menu_item(ui, m, pal, Some("app"), "Open folder as administrator", None, !ro) {
            actions.push(Action::OpenFolderAsAdmin(dir.clone()));
        }
        if let Some(zip) = archive_of(dir) {
            if menu_item(ui, m, pal, Some("archive"), "Extract all", None, true) {
                actions.push(Action::Extract(vec![zip.clone()]));
            }
            if menu_item(ui, m, pal, Some("browse"), "Extract to…", None, true) {
                actions.push(Action::ExtractTo(vec![zip]));
            }
        }
        let dir_pinned = ui
            .data(|d| d.get_temp::<Arc<HashSet<PathBuf>>>(Id::new("pinned_paths")))
            .is_some_and(|s| s.contains(dir));
        if dir_pinned {
            if menu_item(ui, m, pal, Some("close"), "Remove folder from Quick access", None, true) {
                actions.push(Action::UnpinQuick(dir.clone()));
            }
        } else if menu_item(ui, m, pal, Some("plus"), "Pin folder to Quick access", None, true) {
            actions.push(Action::PinQuick(dir.clone()));
        }
        ui.separator();
        if menu_item(ui, m, pal, Some("refresh"), "Refresh", Some("F5"), true) {
            actions.push(Action::Refresh);
            
        }
        if menu_item(ui, m, pal, Some("info"), "Properties", None, true) {
            actions.push(Action::Properties(dir.clone()));
            
        }
    });

    if view.is_empty() {
        let msg = if *loading {
            "Loading…"
        } else if !search.trim().is_empty() {
            if search_job.as_ref().is_some_and(|j| !j.done) {
                "Searching…"
            } else {
                "No items match your search"
            }
        } else {
            "This folder is empty"
        };
        ui.painter().text(
            pos2(body_rect.center().x, body_rect.top() + m.row_h * 2.5),
            Align2::CENTER_CENTER,
            msg,
            FontId::proportional(m.font),
            pal.text_secondary,
        );
        return;
    }

    let row_h = m.row_h;
    let dragging = item_drag_paths(ui);
    ui.spacing_mut().item_spacing.y = 0.0;
    egui::ScrollArea::vertical()
        .id_salt(("rows", tab_id, dir.clone()))
        .scroll_source(ScrollSource {
            drag: DragScroll::Never,
            ..Default::default()
        })
        .auto_shrink([false, false])
        // two extra blank rows below the list: always some empty space to start a drag in
        .show_rows(ui, row_h, view.len() + 2, |ui, range| {
            let content_top = ui.max_rect().top() - range.start as f32 * row_h;
            // A press on a row starts dragging that row (drag to copy); only a press in the blank
            // space around/below the rows starts the selection rectangle.
            let press_pos = ui.input(|i| {
                if i.pointer.primary_pressed() {
                    i.pointer.interact_pos()
                } else {
                    None
                }
            });
            let sb_x = ui.clip_rect().right() - m.s * 14.0; // keep the scrollbar clear
            let rows_left = ui.max_rect().left();
            let press_on_row = press_pos.is_some_and(|p| {
                let y = p.y - content_top;
                p.x >= rows_left
                    && p.x < rows_left + avail
                    && p.x <= sb_x
                    && y >= 0.0
                    && ((y / row_h) as usize) < view.len()
            });
            let band = rubber_band(
                ui,
                m,
                Id::new(("rubber", tab_id)),
                content_top,
                row_h,
                view.len(),
                rename.is_none() && !press_on_row,
                band_hit,
            );
            if let Some(u) = &band {
                let base_id = Id::new(("rubber_base", tab_id));
                if u.started {
                    let base: Vec<PathBuf> = if u.additive {
                        selected.iter().cloned().collect()
                    } else {
                        Vec::new()
                    };
                    ui.data_mut(|d| d.insert_temp(base_id, base));
                }
                let base: Vec<PathBuf> = ui.data(|d| d.get_temp(base_id)).unwrap_or_default();
                selected.clear();
                selected.extend(base);
                if let Some((lo, hi)) = u.span {
                    for vi in lo..=hi {
                        selected.insert(entries[view[vi]].path.clone());
                    }
                }
                if let Some(end) = u.end {
                    *anchor = Some(entries[view[end]].path.clone());
                }
            }
            for vi in range.clone() {
                if vi >= view.len() {
                    break; // the blank rows
                }
                let e = &entries[view[vi]];
                let (rect, resp) = ui.allocate_exact_size(vec2(avail, row_h), Sense::click());
                let is_sel = selected.contains(&e.path);
                let bg = if is_sel {
                    sel_fill(pal, resp.hovered())
                } else if resp.hovered() {
                    pal.hover
                } else {
                    Color32::TRANSPARENT
                };
                if bg != Color32::TRANSPARENT {
                    ui.painter().rect_filled(
                        rect.shrink2(vec2(m.s * 2.0, m.s)),
                        CornerRadius::same(m.radius),
                        bg,
                    );
                }
                if is_sel {
                    paint_sel_marker(ui, rect, m, pal);
                }
                // Drag to copy: remember a press on this row; highlight folders as drop targets.
                if let Some(pp) = press_pos {
                    if pp.x <= sb_x
                        && rename.is_none()
                        && rect.contains(pp)
                        && ui.rect_contains_pointer(rect)
                    {
                        ui.data_mut(|d| {
                            d.insert_temp(
                                item_drag_id(),
                                ItemDrag {
                                    tab: tab_id,
                                    row: e.path.clone(),
                                    press: pp,
                                    active: false,
                                    paths: Arc::new(Vec::new()),
                                    target: None,
                                },
                            )
                        });
                    }
                }
                if e.is_dir {
                    if let Some(paths) = &dragging {
                        drag_drop_zone(
                            ui,
                            pal,
                            m,
                            rect.shrink2(vec2(m.s * 2.0, m.s)),
                            &e.path,
                            paths,
                        );
                    }
                }
                let dim = cut.is_some_and(|c| c.contains(&e.path));
                let tint = if dim {
                    Color32::from_white_alpha(110)
                } else {
                    Color32::WHITE
                };
                let text_col = if dim { pal.text_disabled } else { pal.text };
                let sec_col = if dim {
                    pal.text_disabled
                } else {
                    pal.text_secondary
                };
                draw_system_icon(
                    ui,
                    pos2(rect.left() + m.pad + m.icon / 2.0, rect.center().y),
                    m.icon,
                    e,
                    tint,
                );

                let name_rect = Rect::from_min_max(
                    pos2(rect.left() + m.pad * 1.7 + m.icon, rect.top()),
                    pos2(rect.left() + name_w - m.pad * 0.5, rect.bottom()),
                );
                let renaming_here = rename.as_ref().is_some_and(|r| r.path == e.path);
                if renaming_here {
                    let rs = rename.as_mut().unwrap();
                    let id = Id::new(("rename_edit", tab_id));
                    let er = name_rect.shrink2(vec2(0.0, m.s * 2.0));
                    let te = egui::TextEdit::singleline(&mut rs.text)
                        .id(id)
                        .margin(Margin::symmetric(4, 0))
                        .desired_width(er.width())
                        .vertical_align(Align::Center);
                    let tr = ui.put(er, te);
                    if rs.focus_pending {
                        tr.request_focus();
                        let total_chars = rs.text.chars().count();
                        let stem = if rs.is_dir {
                            total_chars
                        } else {
                            match rs.text.rfind('.') {
                                Some(i) if i > 0 => rs.text[..i].chars().count(),
                                _ => total_chars,
                            }
                        };
                        select_range(ui, id, 0, stem);
                        rs.focus_pending = false;
                        ui.request_repaint();
                    } else if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        actions.push(Action::CancelRename);
                    } else if tr.lost_focus() {
                        actions.push(Action::CommitRename(rs.path.clone(), rs.text.clone()));
                    }
                } else {
                    // In search results, show where the item lives relative to the searched folder.
                    let label;
                    let name_text: &str = if searching_now {
                        let scope = tab.current_search_scope == SearchScope::System;
                        label = match e.path.parent().and_then(|p| if scope { Some(p) } else { p.strip_prefix(&*dir).ok() }) {
                            Some(rel) if !rel.as_os_str().is_empty() =>
                                format!("{}   \u{2192}   {}", e.name, rel.display()),
                            _ => e.name.clone(),
                        };
                        &label
                    } else {
                        &e.name
                    };
                    paint_text(
                        ui,
                        name_rect,
                        name_text,
                        FontId::proportional(m.font),
                        text_col,
                        false,
                    );
                }

                let size_text = if e.is_dir {
                    match (e.dir_size, e.is_link) {
                        (Some(s), _) => format_size(s),
                        (None, true) => "—".to_owned(),
                        (None, false) if searching_now => "—".to_owned(),
                        (None, false) => "Calculating…".to_owned(),
                    }
                } else {
                    format_size(e.size)
                };
                let mut cx = rect.left() + name_w;
                for ci in 0..3 {
                    if !show[ci] {
                        continue;
                    }
                    let cw = w(ci);
                    let cell = Rect::from_min_size(pos2(cx, rect.top()), vec2(cw, row_h))
                        .shrink2(vec2(m.pad * 0.6, 0.0));
                    let (text, color) = match ci {
                        0 => (e.date_text.as_str(), sec_col),
                        1 => (e.type_text.as_str(), sec_col),
                        _ => (
                            size_text.as_str(),
                            if e.is_dir && e.dir_size.is_none() {
                                pal.text_disabled
                            } else {
                                sec_col
                            },
                        ),
                    };
                    paint_text(ui, cell, text, FontId::proportional(m.font), color, ci == 2);
                    cx += cw;
                }

                if (resp.clicked() || resp.secondary_clicked()) && !renaming_here {
                    let mods = ui.input(|i| i.modifiers);
                    if resp.clicked() && mods.command {
                        // Ctrl-click toggles one item.
                        if !selected.remove(&e.path) {
                            selected.insert(e.path.clone());
                        }
                        *anchor = Some(e.path.clone());
                    } else if resp.clicked() && mods.shift && anchor.is_some() {
                        // Shift-click selects the range from the anchor to this row.
                        let from = anchor
                            .as_ref()
                            .and_then(|a| view.iter().position(|&v| &entries[v].path == a))
                            .unwrap_or(vi);
                        let (lo, hi) = (from.min(vi), from.max(vi));
                        selected.clear();
                        for pos in lo..=hi {
                            selected.insert(entries[view[pos]].path.clone());
                        }
                    } else if !(resp.secondary_clicked() && is_sel) {
                        // Right-clicking inside the selection keeps it, so the menu acts on all of it.
                        selected.clear();
                        selected.insert(e.path.clone());
                        *anchor = Some(e.path.clone());
                    }
                }
                if resp.double_clicked() && !renaming_here {
                    actions.push(Action::Open(e.path.clone()));
                }
                if resp.middle_clicked() && (e.is_dir || has_zip_ext(&e.path)) {
                    actions.push(Action::NewTabAt(e.path.clone()));
                }

                scroll_context_menu(&resp, |ui| {
                    let p = &e.path;
                    // Menu actions apply to the whole selection when the clicked row is part of it.
                    let targets: Vec<PathBuf> = if selected.contains(&e.path) {
                        view.iter()
                            .map(|&i| &entries[i].path)
                            .filter(|q| selected.contains(*q))
                            .cloned()
                            .collect()
                    } else {
                        vec![e.path.clone()]
                    };
                    if menu_item(ui, m, pal, Some("external"), "Open", Some("Enter"), true) {
                        actions.push(Action::Open(p.clone()));
                        
                    }
                    // Elevated variants: programs run as administrator, folders open in a new elevated window.
                    let runnable = !e.is_dir
                        && e.path
                            .extension()
                            .and_then(|x| x.to_str())
                            .is_some_and(|x| ["exe", "bat", "cmd", "msi", "com", "lnk", "ps1"].contains(&x.to_lowercase().as_str()));
                    if runnable && menu_item(ui, m, pal, Some("app"), "Run as administrator", None, true) {
                        actions.push(Action::RunAsAdmin(p.clone()));
                    }
                    if e.is_dir && menu_item(ui, m, pal, Some("app"), "Open as administrator", None, true) {
                        actions.push(Action::OpenFolderAsAdmin(p.clone()));
                    }
                    if e.is_dir && archive_of(p).is_none() {
                        if menu_item(ui, m, pal, Some("image"), "Change folder icon…", None, true) {
                            actions.push(Action::ChangeFolderIcon(p.clone()));
                        }
                        if custom_icon_of(p).is_some()
                            && menu_item(ui, m, pal, Some("refresh"), "Reset folder icon", None, true)
                        {
                            actions.push(Action::ResetFolderIcon(p.clone()));
                        }
                    }
                    // An opened zip is read-only (it is shown from a temporary unpacked copy).
                    let in_archive = archive_of(p).is_some();
                    if (e.is_dir || has_zip_ext(&e.path))
                        && menu_item(ui, m, pal, Some("new_tab"), "Open in new tab", None, true)
                    {
                        actions.push(Action::NewTabAt(p.clone()));
                        
                    }
                    if e.is_dir {
                        let pinned = ui
                            .data(|d| d.get_temp::<Arc<HashSet<PathBuf>>>(Id::new("pinned_paths")))
                            .is_some_and(|s| s.contains(p));
                        if pinned {
                            if menu_item(ui, m, pal, Some("close"), "Remove from Quick access", None, true) {
                                actions.push(Action::UnpinQuick(p.clone()));
                            }
                        } else if menu_item(ui, m, pal, Some("plus"), "Pin to Quick access", None, true) {
                            actions.push(Action::PinQuick(p.clone()));
                        }
                    }
                    ui.separator();
                    if menu_item(ui, m, pal, Some("cut"), "Cut", Some("Ctrl+X"), !in_archive) {
                        actions.push(Action::Cut(targets.clone()));
                        
                    }
                    if menu_item(ui, m, pal, Some("copy"), "Copy", Some("Ctrl+C"), true) {
                        actions.push(Action::Copy(targets.clone()));
                        
                    }
                    if menu_item(ui, m, pal, Some("paste"), "Paste", Some("Ctrl+V"), !in_archive) {
                        actions.push(Action::Paste);
                        
                    }
                    if menu_item(ui, m, pal, Some("link"), "Copy path", None, true) {
                        actions.push(Action::CopyPath(targets.clone()));
                        
                    }
                    if menu_item(
                        ui,
                        m,
                        pal,
                        Some("rename"),
                        "Rename",
                        Some("F2"),
                        targets.len() == 1 && !in_archive,
                    ) {
                        actions.push(Action::Rename(p.clone()));
                        
                    }
                    if menu_item(ui, m, pal, Some("delete"), "Delete", Some("Del"), !in_archive) {
                        actions.push(Action::Delete(targets.clone()));
                        
                    }
                    ui.separator();
                    // Native zip support: unzipped items can be compressed, zip files extracted.
                    let zips: Vec<PathBuf> =
                        targets.iter().filter(|t| has_zip_ext(t)).cloned().collect();
                    if !zips.is_empty() {
                        if menu_item(ui, m, pal, Some("archive"), "Extract all", None, true) {
                            actions.push(Action::Extract(zips.clone()));
                        }
                        if menu_item(ui, m, pal, Some("browse"), "Extract to…", None, true) {
                            actions.push(Action::ExtractTo(zips));
                        }
                    }
                    if menu_item(
                        ui,
                        m,
                        pal,
                        Some("archive"),
                        "Compress to ZIP file",
                        None,
                        !in_archive,
                    ) {
                        actions.push(Action::Compress(targets.clone()));
                    }
                    if has_peazip {
                        ui.separator();
                        if menu_item(ui, m, pal, Some("archive"), "Open with PeaZip", None, true) {
                            actions.push(Action::PeaOpen(p.clone()));
                            
                        }
                        if menu_item(
                            ui,
                            m,
                            pal,
                            Some("archive"),
                            "Add to archive with PeaZip…",
                            None,
                            true,
                        ) {
                            actions.push(Action::PeaAdd(p.clone()));
                            
                        }
                    }
                    ui.separator();
                    if menu_item(
                        ui,
                        m,
                        pal,
                        Some("folder"),
                        "Open containing folder",
                        None,
                        true,
                    ) {
                        actions.push(Action::OpenContaining(p.clone()));
                        
                    }
                    if menu_item(
                        ui,
                        m,
                        pal,
                        Some("info"),
                        "Properties",
                        Some("Alt+Enter"),
                        true,
                    ) {
                        actions.push(Action::Properties(p.clone()));
                        
                    }
                });
            }

            if let Some(u) = &band {
                paint_band(ui, pal, u.rect);
            }

            // Keyboard navigation: bring the selected row into view even if it isn't rendered yet.
            if let Some(vi) = scroll_to.take() {
                let top = ui.max_rect().top() - range.start as f32 * row_h;
                let target = Rect::from_min_size(
                    pos2(ui.max_rect().left(), top + vi as f32 * row_h),
                    vec2(avail, row_h),
                );
                ui.scroll_to_rect(target, None);
            }
        });
}
