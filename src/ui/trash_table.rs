//! The Recycle Bin table.

use crate::prelude::*;

/// The Recycle Bin list: multi-select (Ctrl / Shift), sortable columns, virtualized rows.
pub(crate) fn trash_table_ui(
    ui: &mut Ui,
    m: &Metrics,
    pal: &Palette,
    tr: &mut TrashState,
    searching: bool,
    actions: &mut Vec<Action>,
) {
    let TrashState {
        rows,
        view,
        selected,
        anchor,
        scroll_to,
        sort_col,
        sort_asc,
        needs_sort,
        error,
        loading,
        ..
    } = tr;
    let avail = (ui.available_width() - m.pad * 0.5).max(100.0);

    if let Some(msg) = error.clone() {
        let (r, _) = ui.allocate_exact_size(vec2(avail, m.row_h * 1.1), Sense::hover());
        ui.painter()
            .rect_filled(r, CornerRadius::same(m.radius), pal.danger_bg);
        let tr_rect = Rect::from_min_max(
            r.min + vec2(m.pad, 0.0),
            r.max - vec2(m.pad * 3.0 + m.small_icon, 0.0),
        );
        paint_text(
            ui,
            tr_rect,
            &msg,
            FontId::proportional(m.font),
            pal.danger,
            false,
        );
        let cr = Rect::from_center_size(
            pos2(r.right() - m.pad - m.small_icon / 2.0, r.center().y),
            vec2(m.ctl_h * 0.8, m.ctl_h * 0.8),
        );
        let resp = ui.interact(cr, Id::unique("trash_err_close"), Sense::click());
        hover_fill(ui, cr, &resp, pal, m);
        draw_icon(ui, cr.center(), m.small_icon * 0.8, "close", pal.danger);
        if resp.clicked() {
            *error = None;
        }
        ui.add_space(m.pad * 0.5);
    }

    // Columns after Name: Original location, Date deleted, Size. Dropped in that order when narrow.
    let widths = [m.accessibility_scale * 230.0, m.accessibility_scale * 150.0, m.accessibility_scale * 90.0];
    let mut show = [true; 3];
    let name_min = m.accessibility_scale * 220.0;
    let total = |show: &[bool; 3]| (0..3).filter(|&i| show[i]).map(|i| widths[i]).sum::<f32>();
    for d in [0usize, 1, 2] {
        if name_min + total(&show) > avail {
            show[d] = false;
        }
    }
    let name_w = (avail - total(&show)).max(0.0);

    let header_h = m.row_h * 0.95;
    let (hrect, _) = ui.allocate_exact_size(vec2(avail, header_h), Sense::hover());
    let labels = ["Name", "Original location", "Date deleted", "Size"];
    let sorts = [
        TrashSort::Name,
        TrashSort::Location,
        TrashSort::Deleted,
        TrashSort::Size,
    ];
    let mut x = hrect.left();
    for c in 0..4 {
        if c > 0 && !show[c - 1] {
            continue;
        }
        let cw = if c == 0 { name_w } else { widths[c - 1] };
        let crect = Rect::from_min_size(pos2(x, hrect.top()), vec2(cw, header_h));
        x += cw;
        let resp = ui.interact(crect, Id::unique(("trash_hdr", c)), Sense::click());
        hover_fill(ui, crect.shrink2(vec2(m.accessibility_scale * 2.0, m.accessibility_scale * 2.0)), &resp, pal, m);
        let active = *sort_col == sorts[c];
        let color = if active { pal.text } else { pal.text_secondary };
        let right = c == 3;
        let text_left = if c == 0 {
            crect.left() + m.pad * 1.7 + m.icon
        } else {
            crect.left() + m.pad * 0.6
        };
        let tr_rect = Rect::from_min_max(
            pos2(text_left, crect.top()),
            pos2(crect.right() - m.pad * 0.6, crect.bottom()),
        );
        paint_text(
            ui,
            tr_rect,
            labels[c],
            FontId::proportional(m.font * 0.93),
            color,
            right,
        );
        if active {
            let tw = text_width(ui, labels[c], m.font * 0.93);
            let ax = if right {
                tr_rect.right() - tw - m.small_icon * 0.7
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
                *sort_asc = c != 2;
            }
            *needs_sort = true;
            ui.request_repaint();
        }
    }
    ui.painter().line_segment(
        [
            pos2(hrect.left(), hrect.bottom()),
            pos2(hrect.right(), hrect.bottom()),
        ],
        Stroke::new(1.0_f32, pal.divider),
    );

    let body_rect = ui.available_rect_before_wrap();
    let body_resp = ui.interact(body_rect, Id::unique("trash_body"), Sense::click());
    if body_resp.clicked() {
        selected.clear();
    }
    let any_rows = !rows.is_empty();
    scroll_context_menu(&body_resp, |ui| {
        if menu_item(
            ui,
            m,
            pal,
            Some("empty_bin"),
            "Empty Recycle Bin",
            None,
            any_rows,
        ) {
            actions.push(Action::TrashEmpty);
        }
        if menu_item(ui, m, pal, Some("refresh"), "Refresh", Some("F5"), true) {
            actions.push(Action::Refresh);
        }
    });

    if view.is_empty() {
        let msg = if *loading {
            "Loading…"
        } else if searching {
            "No items match your search"
        } else {
            "The Recycle Bin is empty"
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
    ui.spacing_mut().item_spacing.y = 0.0;
    egui::ScrollArea::vertical()
        .id_salt("trash_rows")
        .scroll_source(ScrollSource {
            drag: DragScroll::Never,
            ..Default::default()
        })
        .auto_shrink([false, false])
        .show_rows(ui, row_h, view.len(), |ui, range| {
            let content_top = ui.max_rect().top() - range.start as f32 * row_h;
            let band_hit = ui.clip_rect();
            let band = rubber_band(
                ui,
                m,
                Id::unique("trash_rubber"),
                content_top,
                row_h,
                view.len(),
                true,
                band_hit,
            );
            if let Some(u) = &band {
                let base_id = Id::unique("trash_rubber_base");
                if u.started {
                    let base: Vec<usize> = if u.additive {
                        selected.iter().copied().collect()
                    } else {
                        Vec::new()
                    };
                    ui.data_mut(|d| d.insert_temp(base_id, base));
                }
                let base: Vec<usize> = ui.data(|d| d.get_temp(base_id)).unwrap_or_default();
                selected.clear();
                selected.extend(base);
                if let Some((lo, hi)) = u.span {
                    for vi in lo..=hi {
                        selected.insert(view[vi]);
                    }
                }
                if let Some(end) = u.end {
                    *anchor = Some(end);
                }
            }
            for vi in range.clone() {
                let ri = view[vi];
                let row = &rows[ri];
                let (rect, resp) = ui.allocate_exact_size(vec2(avail, row_h), Sense::click());
                let is_sel = selected.contains(&ri);
                let bg = if is_sel {
                    sel_fill(pal, resp.hovered())
                } else if resp.hovered() {
                    pal.hover
                } else {
                    Color32::TRANSPARENT
                };
                if bg != Color32::TRANSPARENT {
                    ui.painter().rect_filled(
                        rect.shrink2(vec2(m.accessibility_scale * 2.0, m.accessibility_scale)),
                        CornerRadius::same(m.radius),
                        bg,
                    );
                }
                if is_sel {
                    paint_sel_marker(ui, rect, m, pal);
                }
                draw_icon(
                    ui,
                    pos2(rect.left() + m.pad + m.icon / 2.0, rect.center().y),
                    m.icon,
                    row.icon,
                    Color32::WHITE,
                );
                let name_rect = Rect::from_min_max(
                    pos2(rect.left() + m.pad * 1.7 + m.icon, rect.top()),
                    pos2(rect.left() + name_w - m.pad * 0.5, rect.bottom()),
                );
                paint_text(
                    ui,
                    name_rect,
                    &row.name,
                    FontId::proportional(m.font),
                    pal.text,
                    false,
                );
                let mut cx = rect.left() + name_w;
                for ci in 0..3 {
                    if !show[ci] {
                        continue;
                    }
                    let cell = Rect::from_min_size(pos2(cx, rect.top()), vec2(widths[ci], row_h))
                        .shrink2(vec2(m.pad * 0.6, 0.0));
                    let text = match ci {
                        0 => row.location.as_str(),
                        1 => row.date_text.as_str(),
                        _ => row.size_text.as_str(),
                    };
                    paint_text(
                        ui,
                        cell,
                        text,
                        FontId::proportional(m.font),
                        pal.text_secondary,
                        ci == 2,
                    );
                    cx += widths[ci];
                }

                if resp.clicked() || resp.secondary_clicked() {
                    let mods = ui.input(|i| i.modifiers);
                    if resp.clicked() && mods.command {
                        if !selected.remove(&ri) {
                            selected.insert(ri);
                        }
                        *anchor = Some(vi);
                    } else if resp.clicked() && mods.shift && anchor.is_some() {
                        let a = anchor.unwrap_or(vi);
                        let (lo, hi) = (a.min(vi), a.max(vi));
                        selected.clear();
                        for pos in lo..=hi {
                            if let Some(&idx) = view.get(pos) {
                                selected.insert(idx);
                            }
                        }
                    } else if !(resp.secondary_clicked() && is_sel) {
                        selected.clear();
                        selected.insert(ri);
                        *anchor = Some(vi);
                    }
                }
                scroll_context_menu(&resp, |ui| {
                    if menu_item(ui, m, pal, Some("restore"), "Restore", None, true) {
                        actions.push(Action::TrashRestoreSelected);
                        
                    }
                    if menu_item(
                        ui,
                        m,
                        pal,
                        Some("delete"),
                        "Delete permanently",
                        Some("Del"),
                        true,
                    ) {
                        actions.push(Action::TrashPurgeSelected);
                        
                    }
                });
            }
            if let Some(u) = &band {
                paint_band(ui, pal, u.rect);
            }
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
