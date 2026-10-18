//! Left sidebar (quick access, drives).

use crate::prelude::*;

pub(crate) fn nav_item(
    ui: &mut Ui,
    m: &Metrics,
    pal: &Palette,
    icon: &str,
    label: &str,
    selected: bool,
) -> egui::Response {
    nav_item_ex(ui, m, pal, icon, None, label, selected)
}

/// Section title with a small "+" button at its right end (returns true when the button is clicked).
pub(crate) fn quick_header(ui: &mut Ui, m: &Metrics, pal: &Palette, text: &str, can_add: bool) -> bool {
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), m.font * 2.0), Sense::hover());
    let tr = Rect::from_min_max(pos2(rect.left() + m.pad * 1.5, rect.top()), rect.max);
    paint_text(ui, tr, text, FontId::proportional(m.font * 0.86), pal.text_secondary, false);
    let sz = m.font * 1.6;
    let btn = Rect::from_center_size(
        pos2(rect.right() - m.pad * 1.5 - sz / 2.0, rect.center().y),
        vec2(sz, sz),
    );
    let resp = ui.interact(
        btn,
        ui.id().with("quick_add_btn"),
        if can_add { Sense::click() } else { Sense::hover() },
    );
    if can_add {
        hover_fill(ui, btn, &resp, pal, m);
    }
    can_add && resp.on_hover_text("Pin the current folder to Quick access").clicked()
}

/// Navigation item. With a `path` the folder's own platform icon is shown (Documents, Downloads,
/// ... have special icons on Windows); `icon` is the built-in stand-in.
pub(crate) fn nav_item_ex(
    ui: &mut Ui,
    m: &Metrics,
    pal: &Palette,
    icon: &str,
    path: Option<&Path>,
    label: &str,
    selected: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), m.nav_h), Sense::click());
    let inner = rect.shrink2(vec2(0.0, 1.0));
    let r = CornerRadius::same(m.radius);
    if selected {
        ui.painter().rect_filled(inner, r, pal.control);
        if resp.hovered() {
            ui.painter().rect_filled(inner, r, pal.hover);
        }
        let pill = Rect::from_center_size(
            pos2(inner.left() + m.s * 2.0, inner.center().y),
            vec2(m.s * 3.0, m.font * 1.1),
        );
        ui.painter()
            .rect_filled(pill, CornerRadius::same(2), pal.accent);
    } else {
        list_hover(ui, inner, &resp, pal, m);
    }
    let icon_c = pos2(inner.left() + m.pad * 1.5 + m.icon / 2.0, inner.center().y);
    match path {
        Some(p) => draw_path_icon(
            ui,
            icon_c,
            m.icon,
            &make_icon_key(true, None, p),
            p,
            icon,
            Color32::WHITE,
        ),
        None => draw_icon(ui, icon_c, m.icon, icon, Color32::WHITE),
    }
    let tr = Rect::from_min_max(
        pos2(inner.left() + m.pad * 2.3 + m.icon, inner.top()),
        pos2(inner.right() - m.pad, inner.bottom()),
    );
    paint_text(ui, tr, label, FontId::proportional(m.font), pal.text, false);
    resp
}

pub(crate) fn section_label(ui: &mut Ui, m: &Metrics, pal: &Palette, text: &str) {
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), m.font * 2.0), Sense::hover());
    let tr = Rect::from_min_max(pos2(rect.left() + m.pad * 1.5, rect.top()), rect.max);
    paint_text(
        ui,
        tr,
        text,
        FontId::proportional(m.font * 0.86),
        pal.text_secondary,
        false,
    );
}

pub(crate) fn drive_card(
    ui: &mut Ui,
    m: &Metrics,
    pal: &Palette,
    d: &DriveInfo,
    active: bool,
) -> egui::Response {
    let h = m.font * 4.4;
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
    let inner = rect.shrink2(vec2(0.0, 1.0));
    let r = CornerRadius::same(m.radius);
    if active {
        ui.painter().rect_filled(inner, r, pal.control_hover);
        if resp.hovered() {
            ui.painter().rect_filled(inner, r, pal.hover);
        }
        let pill = Rect::from_center_size(
            pos2(inner.left() + m.s * 2.0, inner.center().y),
            vec2(m.s * 3.0, m.font * 1.6),
        );
        ui.painter()
            .rect_filled(pill, CornerRadius::same(2), pal.accent);
    } else {
        list_hover(ui, inner, &resp, pal, m);
    }
    let mount = d.mount_point.display().to_string();
    let title = if d.name.is_empty() {
        mount.clone()
    } else {
        format!("{} ({})", d.name, mount.trim_end_matches(['\\', '/']))
    };
    let x0 = inner.left() + m.pad * 1.5;
    let x1 = inner.right() - m.pad * 1.5;
    let y1 = inner.top() + h * 0.24;
    let icon_c = pos2(x0 + m.icon / 2.0, y1);
    if d.total == 0 && d.kind == DriveKind::Network {
        // Unreachable share: don't ask the shell for its icon (that can block on the network).
        draw_icon(ui, icon_c, m.icon, "drive", Color32::WHITE);
    } else {
        draw_path_icon(
            ui,
            icon_c,
            m.icon,
            &make_icon_key(true, None, &d.mount_point),
            &d.mount_point,
            "drive",
            Color32::WHITE,
        );
    }
    paint_text(
        ui,
        Rect::from_min_max(
            pos2(x0 + m.icon + m.pad * 0.8, y1 - m.font),
            pos2(x1, y1 + m.font),
        ),
        &title,
        FontId::proportional(m.font),
        pal.text,
        false,
    );

    let used = d.total.saturating_sub(d.available);
    let frac = if d.total == 0 {
        0.0
    } else {
        (used as f32 / d.total as f32).clamp(0.0, 1.0)
    };
    let bar_h = m.s * 4.0;
    let bar = Rect::from_min_max(
        pos2(x0, inner.top() + h * 0.54),
        pos2(x1, inner.top() + h * 0.54 + bar_h),
    );
    ui.painter()
        .rect_filled(bar, CornerRadius::same(2), pal.divider);
    let color = if frac > 0.9 {
        Color32::from_rgb(0xC4, 0x2B, 0x1C)
    } else {
        pal.accent
    };
    let fill = Rect::from_min_size(bar.min, vec2(bar.width() * frac, bar_h));
    ui.painter().rect_filled(fill, CornerRadius::same(2), color);
    let info = if d.total == 0 {
        "Not available".to_owned()
    } else {
        format!("{} free of {}", format_size(d.available), format_size(d.total))
    };
    paint_text(
        ui,
        Rect::from_min_max(
            pos2(x0, inner.top() + h * 0.68),
            pos2(x1, inner.top() + h * 0.92),
        ),
        &info,
        FontId::proportional(m.font * 0.86),
        pal.text_secondary,
        false,
    );
    resp.on_hover_text(mount)
}

impl Explorer {
    pub(crate) fn ui_sidebar(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let (m, pal) = (self.m, self.pal);
        let cur = self.tabs[self.active].dir.clone();
        let special_active = self.tabs[self.active].special();
        let trash_active = self.tabs[self.active].trash.is_some();
        let (quick, drives) = (&self.quick, &self.drives);
        // Folder context menus (file list) need to know what is pinned.
        ui.data_mut(|d| {
            d.insert_temp(
                Id::new("pinned_paths"),
                Arc::new(quick.iter().map(|q| q.path.clone()).collect::<HashSet<PathBuf>>()),
            )
        });
        let can_pin = !special_active && !quick.iter().any(|q| same_path(&q.path, &cur));
        let maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
        let radius = if maximized { 0 } else { RADIUS };
        let dragging = item_drag_paths(ui);
        let panel = egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(m.s * 250.0)
            .size_range(m.s * 180.0..=m.s * 440.0)
            .show_separator_line(false)
            .frame(
                bar_frame(&pal, m.pad * 0.6, m.pad * 0.6, m.pad * 0.4, m.pad * 0.4).corner_radius(
                    CornerRadius {
                        nw: 0,
                        ne: 0,
                        sw: radius,
                        se: 0,
                    },
                ),
            )
            .show(ui, |ui| {
                // Bottom-up: the Recycle Bin is pinned, the rest scrolls only when it has to.
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    if nav_item(ui, &m, &pal, "recycle_bin", "Recycle Bin", trash_active).clicked()
                    {
                        actions.push(Action::OpenRecycleBin);
                    }
                    ui.add_space(m.s * 2.0);
                    let (r, _) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
                    ui.painter().rect_filled(r, CornerRadius::ZERO, pal.divider);
                    ui.add_space(m.s * 2.0);

                    ui.with_layout(Layout::top_down(Align::Min), |ui| {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                if quick_header(ui, &m, &pal, "Quick access", can_pin) {
                                    actions.push(Action::PinQuick(cur.clone()));
                                }
                                for q in quick.iter().filter(|q| q.available) {
                                    let resp = nav_item_ex(
                                        ui,
                                        &m,
                                        &pal,
                                        q.icon,
                                        Some(q.path.as_path()),
                                        &q.label,
                                        same_path(&q.path, &cur) && !special_active,
                                    );
                                    if let Some(ps) = &dragging {
                                        drag_drop_zone(ui, &pal, &m, resp.rect, &q.path, ps);
                                    }
                                    if resp.clicked() {
                                        actions.push(Action::Navigate(q.path.clone()));
                                    } else if resp.middle_clicked() {
                                        actions.push(Action::NewTabAt(q.path.clone()));
                                    }
                                    scroll_context_menu(&resp, |ui| {
                                        if menu_item(ui, &m, &pal, Some("external"), "Open", None, true) {
                                            actions.push(Action::Navigate(q.path.clone()));
                                        }
                                        if menu_item(ui, &m, &pal, Some("new_tab"), "Open in new tab", None, true) {
                                            actions.push(Action::NewTabAt(q.path.clone()));
                                        }
                                        ui.separator();
                                        if menu_item(
                                            ui,
                                            &m,
                                            &pal,
                                            Some("close"),
                                            "Remove from Quick access",
                                            None,
                                            true,
                                        ) {
                                            actions.push(Action::UnpinQuick(q.path.clone()));
                                        }
                                    });
                                }
                                if !quick.iter().any(|q| q.available) {
                                    section_label(ui, &m, &pal, "Nothing pinned");
                                }

                                // System drives, removable drives and network drives are listed apart.
                                let mut any_drive = false;
                                for (title, kind) in [
                                    ("System drives", DriveKind::System),
                                    ("Removable drives", DriveKind::Removable),
                                    ("Network drives", DriveKind::Network),
                                ] {
                                    let group: Vec<&DriveInfo> =
                                        drives.iter().filter(|d| d.kind == kind).collect();
                                    if group.is_empty() {
                                        continue;
                                    }
                                    any_drive = true;
                                    ui.add_space(m.pad);
                                    section_label(ui, &m, &pal, title);
                                    for d in group {
                                        let resp = drive_card(
                                            ui,
                                            &m,
                                            &pal,
                                            d,
                                            same_path(&d.mount_point, &cur) && !special_active,
                                        );
                                        if let Some(ps) = &dragging {
                                            drag_drop_zone(ui, &pal, &m, resp.rect, &d.mount_point, ps);
                                        }
                                        if resp.clicked() {
                                            actions.push(Action::Navigate(d.mount_point.clone()));
                                        } else if resp.middle_clicked() {
                                            actions.push(Action::NewTabAt(d.mount_point.clone()));
                                        }
                                    }
                                }
                                if !any_drive {
                                    ui.add_space(m.pad);
                                    section_label(ui, &m, &pal, "Drives");
                                    section_label(ui, &m, &pal, "No drives found");
                                }
                            });
                    });
                });
            });
        self.sidebar_rect = Some(panel.response.rect);
    }
}
