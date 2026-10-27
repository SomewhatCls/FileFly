//! Title-bar tab strip.

use crate::prelude::*;

impl Explorer {
    /// The title bar: tabs on the left, draggable empty space, caption buttons on the right.
    pub(crate) fn ui_tabs(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let (m, pal) = (self.m, self.pal);
        let active = self.active;
        let left_gap = if self.settings.left_drag_space { m.accessibility_scale * 40.0 } else { 0.0 };
        // 1.0 while the first tab is selected: the band below then has a square corner and the tab's wing meets the window edge.
        self.first_anim = ui.animate_bool_with_time(
            Id::unique("first_tab_active"),
            active == 0 && !self.settings.left_drag_space,
            0.15,
        );
        let info: Vec<(u64, String, String, &'static str)> = self
            .tabs
            .iter()
            .map(|t| {
                if t.trash.is_some() {
                    (t.id, t.title(), "Recycle Bin".to_owned(), "recycle_bin")
                } else if t.settings {
                    (t.id, t.title(), "Settings".to_owned(), "settings")
                } else {
                    (t.id, t.title(), t.shown_dir().display().to_string(), "folder")
                }
            })
            .collect();
        let maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
        // Tabs that are currently shown in a tile get a thin accent underline.
        let vis: Vec<u64> = {
            let mut v = Vec::new();
            if let Some(t) = &self.tiles {
                t.leaf_ids(&mut v);
            }
            v
        };
        let mut drag_start: Option<u64> = None;
        // Tab being dragged (set by a previous frame) and where the pointer is: used to reorder tabs.
        let dragging = self.tab_drag;
        let pointer = ui.input(|i| i.pointer.latest_pos());
        let mut reorder: Option<(usize, usize)> = None;
        let mut start_move = false;
        let closing_id = Id::unique("tabs_closing");
        let mut closing: Vec<u64> = ui.data(|d| d.get_temp(closing_id)).unwrap_or_default();
        let bar_h = m.tab_h + m.accessibility_scale * 6.0;
        let caption_w = m.accessibility_scale * 46.0 * 3.0;
        let r_tab = m.radius + 2;
        let wing = r_tab as f32;
        egui::Panel::top("tabs")
            .show_separator_line(false)
            .exact_size(bar_h)
            .frame(
                bar_frame(&pal, 0.0, 0.0, 0.0, 0.0).corner_radius(CornerRadius {
                    nw: 0,
                    ne: 0,
                    sw: 0,
                    se: 0,
                }),
            )
            .show(ui, |ui| {
                let full = ui.max_rect();
                let caption =
                    Rect::from_min_max(pos2(full.right() - caption_w, full.top()), full.max);

                // Empty title-bar space moves the window; double-click maximizes. Registered first so tabs sit on top.
                let drag_rect = Rect::from_min_max(full.min, pos2(caption.left(), full.bottom()));
                let drag =
                    ui.interact(drag_rect, Id::unique("titlebar_drag"), Sense::click_and_drag());
                if drag.drag_started_by(egui::PointerButton::Primary) {
                    start_move = true; // sent after the panel, see below
                }
                if drag.double_clicked() {
                    ui.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                }

                // The first tab is inset by exactly one wing radius so its left wing ends at the window edge.
                let tabs_rect = Rect::from_min_max(
                    pos2(full.left() + left_gap + wing, full.bottom() - m.tab_h),
                    pos2(caption.left(), full.bottom()),
                );
                let mut wings: Vec<(Rect, f32)> = Vec::new();
                let mut scroll_rect = tabs_rect;
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(tabs_rect)
                        .layout(Layout::left_to_right(Align::Center)),
                    |ui| {
                        ui.spacing_mut().item_spacing.x = m.accessibility_scale * 2.0;
                        let gap = ui.spacing().item_spacing.x;
                        let n = info.len().max(1);
                        // Room for the tabs: everything left of the pinned "+" button.
                        let avail = (ui.available_width() - m.ctl_h - gap).max(0.0);

                        // New tabs grow in from zero width. Tabs seen for the first time (not on the very first
                        // frame, so a restored session doesn't animate) are seeded at 0 and eased to 1.
                        let known_id = Id::unique("tabs_known_ids");
                        let known: Vec<u64> = ui.data(|d| d.get_temp(known_id)).unwrap_or_default();
                        let first_frame = known.is_empty();
                        let grow: Vec<f32> = info
                            .iter()
                            .map(|(id, ..)| {
                                let gid = Id::unique(("tab_grow", *id));
                                if !first_frame && !known.contains(id) {
                                    ui.ctx().animate_value_with_time(gid, 0.0, 0.0);
                                }
                                let is_closing = closing.contains(id);
                                let target = if is_closing { 0.0 } else { 1.0 };
                                let animation_time = if is_closing { 0.1 } else { 0.15 };
                                let t = ui.ctx().animate_value_with_time(gid, target, animation_time);
                                // Ease-out both ways: fast at the start, settling gently at the end.
                                if is_closing { t * t * t } else { 1.0 - (1.0 - t).powi(3) }
                            })
                            .collect();
                        ui.data_mut(|d| d.insert_temp(known_id, info.iter().map(|t| t.0).collect::<Vec<u64>>()));

                        // Close down tabs only if they have shrunken enough.
                        for (i, (id, ..)) in info.iter().enumerate() {
                            if closing.contains(id) && grow[i] < 0.02 {
                                actions.push(Action::CloseTab(i));
                                closing.retain(|c| c != id);
                            }
                        }

                        let sum_k = grow.iter().sum::<f32>().max(0.01);
                        let tab_w = ((avail - gap * (n as f32 - 1.0)) / sum_k).clamp(m.accessibility_scale * 90.0, m.accessibility_scale * 240.0);
                        let content_w = tab_w * sum_k + gap * (n as f32 - 1.0);
                        let overflow = content_w > avail + 0.5;
                        //When there are too many tabs, the overflow becomes scrollable.
                        // The scroll area is exactly as wide as the tabs (so the rest of the title bar
                        // stays draggable) but never wider than the space before the "+" button.
                        let scroll_w = content_w.min(avail);
                        scroll_rect = Rect::from_min_size(tabs_rect.min, vec2(scroll_w, m.tab_h));

                        // Bring the active tab into view when it (or the tab count) changes.
                        let strip_key = (info.get(active).map(|t| t.0).unwrap_or(0), info.len());
                        let strip_id = Id::unique("tab_strip_last");
                        let strip_changed =
                            ui.data(|d| d.get_temp::<(u64, usize)>(strip_id)) != Some(strip_key);
                        ui.data_mut(|d| d.insert_temp(strip_id, strip_key));

                        ui.allocate_ui(vec2(scroll_w, m.tab_h), |ui| {
                        egui::ScrollArea::horizontal()
                            .id_salt("tab_strip")
                            .scroll_source(ScrollSource {
                                drag: DragScroll::Never,
                                ..Default::default()
                            })
                            .scroll_bar_visibility(
                                egui::containers::scroll_area::ScrollBarVisibility::AlwaysHidden,
                            )
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                        ui.scope_builder(
                            egui::UiBuilder::new().layout(Layout::left_to_right(Align::Center)),
                            |ui| {
                        ui.spacing_mut().item_spacing.x = gap;
                        // The mouse wheel scrolls the strip sideways. The delta is taken so the
                        // scroll area does not apply it a second time.
                        if overflow && ui.rect_contains_pointer(scroll_rect) {
                            let d = ui.input_mut(|i| std::mem::take(&mut i.smooth_scroll_delta));
                            let dx = if d.y != 0.0 { d.y } else { d.x };
                            if dx != 0.0 {
                                //multiplier for scrolling the tab bar
                                const TAB_SCROLL_SPEED: f32 = 8.0;
                                ui.scroll_with_delta(vec2(dx * TAB_SCROLL_SPEED, 0.0));
                            }
                        }
                        let mut tab_rects: Vec<Rect> = Vec::with_capacity(info.len());
                        for (i, (id, title, full_path, tab_icon)) in info.iter().enumerate() {
                            let w = tab_w * grow[i];
                            let (rect, resp) = ui.allocate_exact_size(vec2(w, m.tab_h), Sense::click_and_drag());
                            let old_clip = ui.clip_rect();
                            ui.set_clip_rect(rect.intersect(old_clip)); // icon/title/close must not spill while narro
                            tab_rects.push(rect);
                            if i == active && strip_changed && overflow {
                                ui.scroll_to_rect(rect.expand2(vec2(gap, 0.0)), None);
                            }
                            let is_active = i == active;
                            if resp.drag_started_by(egui::PointerButton::Primary) {
                                drag_start = Some(*id);
                            }
                            let k = ui.animate_bool_with_time(
                                Id::unique(("tab_wing", *id)),
                                is_active,
                                0.12,
                            );
                            if k > 0.01 {
                                wings.push((rect, k));
                            }
                            if is_active {
                                ui.painter().rect_filled(
                                    rect,
                                    CornerRadius {
                                        nw: r_tab,
                                        ne: r_tab,
                                        sw: 0,
                                        se: 0,
                                    },
                                    pal.layer,
                                );
                            } else {
                                // Resting tabs get a very faint plate; hovering strengthens it.
                                let c = if resp.hovered() || dragging == Some(*id) {
                                    pal.subtle_pressed
                                } else {
                                    Color32::TRANSPARENT
                                };
                                ui.painter().rect_filled(
                                    rect.shrink2(vec2(0.0, m.accessibility_scale * 3.0)),
                                    CornerRadius::same(m.radius),
                                    c,
                                );
                            }
                            if !is_active && vis.contains(id) {
                                ui.painter().rect_filled(
                                    Rect::from_min_size(
                                        pos2(rect.left() + m.pad, rect.bottom() - m.accessibility_scale * 4.0),
                                        vec2(rect.width() - m.pad * 2.0, m.accessibility_scale * 2.0),
                                    ),
                                    CornerRadius::same(1),
                                    pal.accent.gamma_multiply(0.75),
                                );
                            }
                            let close_sz = m.small_icon * 2.0;
                            let close_rect = Rect::from_center_size(
                                pos2(rect.right() - m.pad * 0.8 - close_sz / 2.0, rect.center().y),
                                vec2(close_sz, close_sz),
                            );
                            let close = ui.interact(
                                close_rect,
                                Id::unique(("tab_close", *id)),
                                Sense::click(),
                            );
                            // Only show close button if tab is almost fully grown.
                            let show_close = (is_active || resp.hovered() || close.hovered()) && grow[i] > 0.95;
                            // Unlike the fetched icons, the settings icon is a white svg and therefore needs to be colored in properly.
                            let color = if *tab_icon == "settings" {pal.text_secondary} else {Color32::WHITE};
                            draw_icon(
                                ui,
                                pos2(rect.left() + m.pad + m.small_icon / 2.0, rect.center().y),
                                m.small_icon,
                                tab_icon,
                                color,
                            );
                            let right_edge = if show_close {
                                close_rect.left() - m.pad * 0.3
                            } else {
                                rect.right() - m.pad
                            };
                            let tr = Rect::from_min_max(
                                pos2(rect.left() + m.pad * 1.7 + m.small_icon, rect.top()),
                                pos2(right_edge, rect.bottom()),
                            );
                            paint_text(
                                ui,
                                tr,
                                title,
                                FontId::proportional(m.font),
                                if is_active {
                                    pal.text
                                } else {
                                    pal.text_secondary
                                },
                                false,
                            );
                            if show_close {
                                if close.hovered() || close.is_pointer_button_down_on() {
                                    let c = if close.is_pointer_button_down_on() {
                                        pal.subtle_pressed
                                    } else {
                                        pal.subtle_pressed
                                    };
                                    ui.painter().rect_filled(
                                        close_rect,
                                        CornerRadius::same(m.radius),
                                        c,
                                    );
                                }
                                draw_icon(
                                    ui,
                                    close_rect.center(),
                                    m.icon * 0.8,
                                    "close",
                                    if close.hovered() {
                                        pal.text
                                    } else {
                                        pal.text_secondary
                                    },
                                );
                            }

                            // Starts the closing animation for tabs.
                            let is_closing = closing.contains(id);
                            if !is_closing {
                                if close.clicked() || resp.middle_clicked() {
                                    if info.len() > 1 {
                                        closing.push(*id); // shrink first, closed in step 2
                                    } else {
                                        actions.push(Action::CloseTab(i)); // last tab: nothing to animate
                                    }
                                } else if resp.clicked() {
                                    actions.push(Action::SwitchTab(i));
                                }
                            }
                            if !close.clone().hovered() {
                                resp.on_hover_text(full_path);
                            }
                            close.on_hover_text("Close tab (Ctrl+W)");
                            ui.set_clip_rect(old_clip);
                        }
                        // Reorder: while a tab is dragged over the title bar it swaps places with a
                        // neighbour as soon as the pointer passes that neighbour's centre.
                        if let (Some(d), Some(p)) = (dragging, pointer) {
                            if full.contains(p) {
                                if let Some(i) = info.iter().position(|t| t.0 == d) {
                                    let to = if i > 0 && p.x < tab_rects[i - 1].center().x {
                                        i - 1
                                    } else if i + 1 < tab_rects.len()
                                        && p.x > tab_rects[i + 1].center().x
                                    {
                                        i + 1
                                    } else {
                                        i
                                    };
                                    if to != i {
                                        reorder = Some((i, to));
                                    }
                                }
                            }
                        }
                            },
                        );
                            });
                        });
                        if icon_button(ui, &m, &pal, "plus", "New tab (Ctrl+T)", true, false)
                            .clicked()
                        {
                            actions.push(Action::NewTab);
                        }
                    },
                );

                // Concave "wings" blend the selected tab into the band below it.
                // A tab cut off by the edge of the scrolled strip gets no wing on that side.
                for (rect, k) in &wings {
                    if rect.left() >= scroll_rect.left() - 0.5 {
                        tab_wing(ui, rect.left_bottom(), wing * k, true, pal.layer);
                    }
                    if rect.right() <= scroll_rect.right() + 0.5 {
                        tab_wing(ui, rect.right_bottom(), wing * k, false, pal.layer);
                    }
                }

                if self.settings.custom_frame {
                    caption_buttons(ui, &m, &pal, maximized, true);
                }
            });

        ui.data_mut(|d| d.insert_temp(closing_id, closing));

        if let Some(id) = drag_start {
            self.tab_drag = Some(id);
        }
        if start_move {
            ui.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            ui.ctx()
                .data_mut(|d| d.insert_temp(Id::unique("pointer_reset"), true));
            ui.request_repaint();
        }
        if let Some((from, to)) = reorder {
            self.move_tab(from, to);
            ui.request_repaint();
        }
    }

    /// Moves a tab to a new position and keeps `active` pointing at the same tab.
    pub(crate) fn move_tab(&mut self, from: usize, to: usize) {
        if from == to || from >= self.tabs.len() || to >= self.tabs.len() {
            return;
        }
        let active_id = self.tabs.get(self.active).map(|t| t.id);
        let t = self.tabs.remove(from);
        self.tabs.insert(to, t);
        if let Some(id) = active_id {
            if let Some(i) = self.tabs.iter().position(|t| t.id == id) {
                self.active = i;
            }
        }
    }

    /// While a tab is being dragged: shows where it would land (clearly, in the accent colour) and
    /// performs the drop on release.
    pub(crate) fn tab_drag_overlay(&mut self, ui: &mut Ui, panes: &[(u64, Rect)]) {
        let Some(dragged) = self.tab_drag else {
            return;
        };
        let (pos, down, released) = ui.input(|i| {
            (
                i.pointer.latest_pos(),
                i.pointer.primary_down(),
                i.pointer.primary_released(),
            )
        });
        let (m, pal) = (self.m, self.pal);
        ui.set_cursor_icon(egui::CursorIcon::Grabbing);
        ui.request_repaint();
        let outside = self.cursor_outside_window() == Some(true);
        let overlay = ui.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            Id::unique("tile_drop_preview"),
        ));
        let mut target: Option<(u64, DropZone)> = None;
        // Over the title bar (above all panes) a dragged tab only reorders: no split overlay there.
        let panes_top = panes
            .iter()
            .map(|(_, r)| r.top())
            .fold(f32::INFINITY, f32::min);

        if outside {
            if self.peer_under.0.elapsed() > Duration::from_millis(150) {
                let over = cursor_screen_pos()
                    .map_or(false, |(cx, cy)| self.peers.window_at(cx, cy).is_some());
                self.peer_under = (Instant::now(), over);
            }
            let msg = if self.peer_under.1 {
                if self.tabs.len() > 1 {
                    "Release to move this tab into that window  (hold Shift: move all tabs)"
                } else {
                    "Release to merge this window into that window"
                }
            } else if self.tabs.len() > 1 {
                "Release to open this tab in its own window"
            } else {
                "Drop on another FileFly window to merge"
            };
            let screen = ui.content_rect();
            overlay.rect_filled(screen, CornerRadius::ZERO, Color32::from_black_alpha(110));
            let band = Rect::from_center_size(
                screen.center_bottom() - vec2(0.0, m.row_h * 2.0),
                vec2((screen.width() - m.pad * 4.0).min(m.accessibility_scale * 680.0), m.row_h * 1.6),
            );
            overlay.rect_filled(band, CornerRadius::same(m.radius), pal.accent);
            overlay.text(
                band.center(),
                Align2::CENTER_CENTER,
                msg,
                FontId::proportional(m.font * 1.05),
                pal.on_accent,
            );
        } else if let Some(p) = pos.filter(|p| panes.is_empty() || p.y >= panes_top) {
            let already = panes.iter().any(|(t, _)| *t == dragged);
            let can_split = already || panes.len() < 4;
            let dragged_special = self.tabs.iter().any(|t| t.id == dragged && t.special());
            if let Some((tid, rect)) = panes.iter().find(|(_, r)| r.contains(p)) {
                let same = *tid == dragged;
                // Dropping a tab on an edge of its own pane duplicates it into a new pane, as long as
                // there is room (and it is a normal folder tab when other panes are already open).
                let zone_ok = if !same {
                    can_split
                } else if panes.len() == 1 {
                    true
                } else {
                    panes.len() < 4 && !dragged_special
                };
                let zone = drop_zone(*rect, p, zone_ok);
                if !(same && zone == DropZone::Center) {
                    target = Some((*tid, zone));
                }
            }
            // Dim everything, then light up the exact area the tab will occupy.
            let all = panes
                .iter()
                .fold(Rect::NOTHING, |acc, (_, r)| acc.union(*r));
            overlay.rect_filled(all, CornerRadius::ZERO, Color32::from_black_alpha(95));
            if let Some((tid, zone)) = target {
                if let Some((_, rect)) = panes.iter().find(|(t, _)| *t == tid) {
                    let zr = zone_rect(*rect, zone).shrink(m.accessibility_scale * 4.0);
                    overlay.rect_filled(zr, CornerRadius::same(m.radius), pal.accent.gamma_multiply(0.55));
                    overlay.rect_stroke(
                        zr,
                        CornerRadius::same(m.radius),
                        Stroke::new(3.0_f32, pal.accent),
                        StrokeKind::Inside,
                    );
                    let label = match zone {
                        DropZone::Left => "Open on the left",
                        DropZone::Right => "Open on the right",
                        DropZone::Top => "Open above",
                        DropZone::Bottom => "Open below",
                        DropZone::Center => "Replace this pane",
                    };
                    let g = overlay.layout_no_wrap(
                        label.to_owned(),
                        FontId::proportional(m.font * 1.2),
                        pal.on_accent,
                    );
                    let pill = Rect::from_center_size(zr.center(), g.size() + vec2(m.pad * 3.0, m.pad * 1.6));
                    overlay.rect_filled(pill, CornerRadius::same(m.radius), pal.accent);
                    overlay.galley(pill.center() - g.size() / 2.0, g, pal.on_accent);
                }
            }
            // A small label follows the pointer.
            let title = self
                .tabs
                .iter()
                .find(|t| t.id == dragged)
                .map(|t| t.title())
                .unwrap_or_default();
            let tip = ui.layer_painter(egui::LayerId::new(
                egui::Order::Tooltip,
                Id::unique("tile_drag_ghost"),
            ));
            let galley = tip.layout_no_wrap(title, FontId::proportional(m.font), pal.text);
            let r = Rect::from_min_size(p + vec2(14.0, 14.0), galley.size() + vec2(m.pad * 2.0, m.pad));
            tip.rect_filled(r, CornerRadius::same(m.radius), pal.layer);
            tip.rect_stroke(
                r,
                CornerRadius::same(m.radius),
                Stroke::new(1.0_f32, pal.accent),
                StrokeKind::Inside,
            );
            tip.galley(r.min + vec2(m.pad, m.pad * 0.5), galley, pal.text);
        }
        if released || !down {
            self.tab_drag = None;
            if let Some((tid, zone)) = target {
                self.drop_tab(dragged, tid, zone);
            } else if outside {
                self.detach_tab(dragged);
            }
        }
    }
}
