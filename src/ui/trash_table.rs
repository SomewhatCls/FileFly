//! The Recycle Bin table.

use crate::prelude::*;

/// The Recycle Bin list: multi-select (Ctrl / Shift), sortable columns, virtualized rows.
pub(crate) fn trash_table_ui(
    ui: &mut Ui,
    m: &Metrics,
    pal: &Palette,
    tr: &mut TrashState,
    tab_id: u64,
    notice: &mut Option<Notice>,
    searching: bool,
    // Ids of items a running operation is working on: dimmed, and can't be restored or deleted again.
    busy: &HashSet<std::ffi::OsString>,
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

    // Same notification bar as the file view: the tab's notice, or the Recycle Bin's own error text.
    notice_bar(ui, m, pal, tab_id, avail, notice, error, actions);

    // Columns after Name: Original location, Date deleted, Size. Dropped in that order when narrow.
    // The Size column always has room for the widest regular value ("999.9 MB") plus the spinner.
    let size_w = size_col_min_width(ui, m).max(m.accessibility_scale * 90.0);
    let widths = [m.accessibility_scale * 230.0, m.accessibility_scale * 150.0, size_w];
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
    let body_resp = ui.interact(body_rect, win_id("trash_body"), Sense::click());
    if body_resp.clicked() {
        selected.clear();
    }
    // Emptying the bin would also take the items an operation is working on.
    let any_rows = !rows.is_empty() && busy.is_empty();
    scroll_context_menu(&body_resp, |ui| {
        if menu_item_tip(
            ui,
            m,
            pal,
            Some("empty_bin"),
            "Empty Recycle Bin",
            None,
            any_rows,
            // Greyed out because of a running operation (an empty bin has nothing to explain).
            (!rows.is_empty() && !busy.is_empty()).then_some("Not available while items are being restored or deleted"),
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
                win_id("trash_rubber"),
                content_top,
                row_h,
                view.len(),
                true,
                band_hit,
            );
            if let Some(u) = &band {
                let base_id = win_id("trash_rubber_base");
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
                let is_busy = busy.contains(&row.item.id);
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
                // When the Size column has been dropped (narrow window), a row whose size is still being
                // summed keeps its spinner at the row's right edge, and the name makes room for it.
                let edge_spinner = row.size_pending && !show[2];
                let spinner_d = size_spinner_diameter(m);
                let name_left = rect.left() + m.pad * 1.7 + m.icon;
                let name_right = (rect.left() + name_w - m.pad * 0.5
                    - if edge_spinner { spinner_d + m.pad } else { 0.0 })
                .max(name_left);
                let name_rect = Rect::from_min_max(pos2(name_left, rect.top()), pos2(name_right, rect.bottom()));
                paint_text(
                    ui,
                    name_rect,
                    &row.name,
                    FontId::proportional(m.font),
                    if is_busy { pal.text_secondary } else { pal.text },
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
                    // A folder whose size is still being summed: spinner next to the number.
                    let text_rect = if ci == 2 && row.size_pending {
                        size_cell_spinner(ui, m, pal, cell, text)
                    } else {
                        cell
                    };
                    paint_text(
                        ui,
                        text_rect,
                        text,
                        FontId::proportional(m.font),
                        pal.text_secondary,
                        ci == 2,
                    );
                    cx += widths[ci];
                }
                if edge_spinner {
                    paint_spinner(
                        ui,
                        pos2(rect.right() - m.pad - spinner_d / 2.0, rect.center().y),
                        spinner_d,
                        pal.accent,
                    );
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
                    if menu_item_tip(ui, m, pal, Some("restore"), "Restore", None, !is_busy, Some("Being restored or deleted right now")) {
                        actions.push(Action::TrashRestoreSelected);
                        
                    }
                    if menu_item_tip(
                        ui,
                        m,
                        pal,
                        Some("delete"),
                        "Delete permanently",
                        Some("Del"),
                        !is_busy,
                        Some("Being restored or deleted right now"),
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

#[cfg(test)]
mod tests {
    //! Headless checks of the Recycle Bin's error reporting: a job error must show up in the notification
    //! bar, a tab notice beats the bin's own error text (as `git_error` does in the file view), and the
    //! close button clears both. Each test drives `trash_table_ui` through a real egui context.
    use super::*;

    const WIDTH: f32 = 800.0;

    fn look() -> (Metrics, Palette) {
        (
            Metrics::new(14.0, 1.0, false),
            Palette::new(egui::Theme::Dark, Accent::fallback(), TransparencyEffect::None),
        )
    }

    /// Everything painted as text during a frame (the bar paints with the painter, not as a label).
    fn painted_text(out: &egui::FullOutput) -> Vec<String> {
        fn walk(shape: &egui::Shape, into: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(t) => into.push(t.galley.text().to_owned()),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, into)),
                _ => {}
            }
        }
        let mut text = Vec::new();
        for clipped in &out.shapes {
            walk(&clipped.shape, &mut text);
        }
        text
    }

    struct Frame {
        text: Vec<String>,
        /// Where the table's top-left is, and how wide the space for it is.
        origin: egui::Pos2,
        width: f32,
    }

    /// Runs one frame of the table in `ctx` with the given input events. If egui has no `run_ui`
    /// in your version, this is the only function to adapt.
    fn frame(ctx: &egui::Context, events: Vec<egui::Event>, tr: &mut TrashState, notice: &mut Option<Notice>) -> Frame {
        let (m, pal) = look();
        let mut raw = egui::RawInput::default();
        raw.screen_rect = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(WIDTH, 600.0)));
        raw.events = events;
        let (mut origin, mut width) = (egui::Pos2::ZERO, 0.0);
        let mut actions = Vec::new();
        let mut out = ctx.run_ui(raw, |ui| {
            origin = ui.cursor().min;
            width = ui.available_width();
            trash_table_ui(ui, &m, &pal, tr, 1, notice, false, &HashSet::new(), &mut actions);
        });
        out.textures_delta.clear(); // there is no renderer to upload them; egui panics when a frame's deltas are dropped
        Frame { text: painted_text(&out), origin, width }
    }

    fn shows(f: &Frame, needle: &str) -> bool {
        f.text.iter().any(|t| t.contains(needle))
    }

    /// Clicks the bar's close button (position derived from `notice_bar`'s layout).
    fn click_close(ctx: &egui::Context, f: &Frame, tr: &mut TrashState, notice: &mut Option<Notice>) {
        let (m, _) = look();
        let bar_w = (f.width - m.pad * 0.5).max(100.0);
        let at = egui::pos2(
            f.origin.x + bar_w - m.pad - m.small_icon / 2.0,
            f.origin.y + m.row_h * 1.1 / 2.0,
        );
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        for event in [egui::Event::PointerMoved(at), button(true), button(false)] {
            frame(ctx, vec![event], tr, notice);
        }
    }

    fn bin() -> TrashState {
        let mut tr = TrashState::new();
        tr.loading = false;
        tr
    }

    #[test]
    fn nothing_is_shown_without_an_error() {
        let (ctx, mut tr, mut notice) = (egui::Context::default(), bin(), None);
        let f = frame(&ctx, vec![], &mut tr, &mut notice);
        // The empty-bin message proves the table itself was drawn, so the checks below mean something.
        assert!(shows(&f, "The Recycle Bin is empty"), "{:?}", f.text);
        assert!(!shows(&f, "Restore failed"), "{:?}", f.text);
    }

    #[test]
    fn a_job_error_in_the_bin_is_shown() {
        let (ctx, mut tr, mut notice) = (egui::Context::default(), bin(), None);
        tr.error = Some("Restore failed: the item already exists".to_owned());
        let f = frame(&ctx, vec![], &mut tr, &mut notice);
        assert!(shows(&f, "Restore failed"), "{:?}", f.text);
    }

    #[test]
    fn a_tab_notice_is_shown_in_the_bin() {
        let (ctx, mut tr) = (egui::Context::default(), bin());
        let mut notice = Some(Notice::error("Restore failed: access denied".to_owned()));
        let f = frame(&ctx, vec![], &mut tr, &mut notice);
        assert!(shows(&f, "access denied"), "{:?}", f.text);
    }

    #[test]
    fn the_tab_notice_wins_over_the_bins_own_error() {
        let (ctx, mut tr) = (egui::Context::default(), bin());
        tr.error = Some("old load error".to_owned());
        let mut notice = Some(Notice::error("fresh job error".to_owned()));
        let f = frame(&ctx, vec![], &mut tr, &mut notice);
        assert!(shows(&f, "fresh job error"), "{:?}", f.text);
        assert!(!shows(&f, "old load error"), "{:?}", f.text);
    }

    #[test]
    fn closing_the_bar_clears_both_and_hides_it() {
        let (ctx, mut tr) = (egui::Context::default(), bin());
        tr.error = Some("load error".to_owned());
        let mut notice = Some(Notice::error("job error".to_owned()));
        let first = frame(&ctx, vec![], &mut tr, &mut notice);
        assert!(shows(&first, "job error"), "{:?}", first.text);

        click_close(&ctx, &first, &mut tr, &mut notice);

        assert!(notice.is_none() && tr.error.is_none(), "the close button did not clear the error");
        let after = frame(&ctx, vec![], &mut tr, &mut notice);
        assert!(!shows(&after, "job error") && !shows(&after, "load error"), "{:?}", after.text);
    }
}
