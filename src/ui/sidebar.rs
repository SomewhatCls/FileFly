//! Left sidebar (quick access, drives). Supports an expanded and a compact (icon-only) mode.

use std::f32::consts::{FRAC_PI_2, TAU};

use crate::prelude::*;

// ---------------------------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------------------------

/// Row background: the hover highlight, or `selected_fill` plus the accent pill on the left edge.
fn paint_row_bg(
    ui: &Ui,
    m: &Metrics,
    pal: &Palette,
    inner: Rect,
    resp: &egui::Response,
    selected_fill: Option<Color32>,
    pill_h: f32,
) {
    let Some(fill) = selected_fill else {
        list_hover(ui, inner, resp, pal, m);
        return;
    };
    let r = CornerRadius::same(m.radius);
    ui.painter().rect_filled(inner, r, fill);
    if resp.hovered() {
        ui.painter().rect_filled(inner, r, pal.hover);
    }
    let pill = Rect::from_center_size(
        pos2(inner.left() + m.accessibility_scale * 2.0, inner.center().y),
        vec2(m.accessibility_scale * 3.0, pill_h),
    );
    ui.painter()
        .rect_filled(pill, CornerRadius::same(2), pal.accent);
}

/// Folder icon (the platform's own when `path` is known, otherwise the built-in `icon`).
fn draw_nav_icon(ui: &mut Ui, c: egui::Pos2, size: f32, icon: &str, path: Option<&Path>) {
    match path {
        Some(p) => draw_path_icon(
            ui,
            c,
            size,
            &make_icon_key(true, None, p),
            p,
            icon,
            Color32::WHITE,
        ),
        None => draw_icon(ui, c, size, icon, Color32::WHITE),
    }
}

/// Drive icon. An unreachable network share gets the built-in icon: asking the shell for its
/// icon can block on the network.
fn draw_drive_icon(ui: &mut Ui, c: egui::Pos2, size: f32, d: &DriveInfo) {
    if d.total == 0 && d.kind == DriveKind::Network {
        draw_icon(ui, c, size, "drive", Color32::WHITE);
    } else {
        draw_path_icon(
            ui,
            c,
            size,
            &make_icon_key(true, None, &d.mount_point),
            &d.mount_point,
            "drive",
            Color32::WHITE,
        );
    }
}

/// Thin horizontal divider spanning the available width.
fn divider(ui: &mut Ui, pal: &Palette) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(r, CornerRadius::ZERO, pal.divider);
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

// ---------------------------------------------------------------------------------------------
// Drive data
// ---------------------------------------------------------------------------------------------

/// "Name (D:)" or just the mount point when the drive has no label.
fn drive_title(d: &DriveInfo) -> String {
    let mount = d.mount_point.display().to_string();
    if d.name.is_empty() {
        mount
    } else {
        format!("{} ({})", d.name, mount.trim_end_matches(['\\', '/']))
    }
}

/// "624.8 GB free of 753.7 GB" or "Not available".
fn drive_info(d: &DriveInfo) -> String {
    if d.total == 0 {
        "Not available".to_owned()
    } else {
        format!("{} free of {}", format_size(d.available), format_size(d.total))
    }
}

/// Used fraction of a drive in 0..=1.
fn drive_fraction(d: &DriveInfo) -> f32 {
    let used = d.total.saturating_sub(d.available);
    if d.total == 0 {
        0.0
    } else {
        (used as f32 / d.total as f32).clamp(0.0, 1.0)
    }
}

fn drive_fill_color(pal: &Palette, frac: f32) -> Color32 {
    if frac > 0.9 {
        Color32::from_rgb(0xC4, 0x2B, 0x1C)
    } else {
        pal.accent
    }
}

/// Windows drive letter with its colon ("N:"); `None` on other platforms.
fn drive_letter(d: &DriveInfo) -> Option<String> {
    d.mount_point
        .display()
        .to_string()
        .chars()
        .next()
        .filter(|ch| cfg!(windows) && ch.is_ascii_alphabetic())
        .map(|ch| format!("{}:", ch.to_ascii_uppercase()))
}

// ---------------------------------------------------------------------------------------------
// Compact-mode metrics
// ---------------------------------------------------------------------------------------------

// Fixed spacings (regular margins). Row heights that must follow the app-wide compact setting
// live in `Metrics` (e.g. `nav_h`) instead.

/// Height of a drive card (expanded mode).
fn card_h(m: &Metrics) -> f32 {
    m.font * 4.4
}

/// Visible gap between two drive rings (sidebar compact mode).
fn ring_gap(m: &Metrics) -> f32 {
    m.font * 0.7
}

/// Minimum visible gap on each side of a divider (sidebar compact mode).
fn divider_gap(m: &Metrics) -> f32 {
    m.font * 0.45
}

/// Space above a drive group's title (expanded mode).
fn group_gap(m: &Metrics) -> f32 {
    m.pad
}

/// Space on each side of the divider above the Recycle Bin (expanded mode).
fn bin_divider_pad(m: &Metrics) -> f32 {
    m.accessibility_scale * 2.0
}

fn ring_radius(m: &Metrics) -> f32 {
    m.font * 1.15
}

fn ring_thick(m: &Metrics) -> f32 {
    m.accessibility_scale * 3.0
}

/// Compact drive row: the ring plus the visible gap to the next ring.
fn compact_ring_row_h(m: &Metrics) -> f32 {
    2.0 * ring_radius(m) + ring_thick(m) + ring_gap(m)
}

/// x of the icon column's centre, shared by the hamburger, the folder icons and the rings: the
/// position the icons have in expanded mode. Compact folder rows are as tall as expanded ones.
fn compact_icon_cx(m: &Metrics, inner: Rect) -> f32 {
    inner.left() + m.pad * 1.5 + m.icon / 2.0
}

/// Empty space between a folder icon and the edge of its row.
fn compact_icon_pad(m: &Metrics) -> f32 {
    (m.nav_h - m.icon) / 2.0
}

/// Empty space between a drive ring and the edge of its row.
fn compact_ring_pad(m: &Metrics) -> f32 {
    ring_gap(m) / 2.0
}

/// A divider with the same visible gap on both sides. `above_pad` / `below_pad` are the paddings
/// the neighbouring rows already carry; only the rest is added as space. (In a bottom-up layout
/// the arguments are in call order, i.e. the first one ends up below the divider.)
fn compact_divider(
    ui: &mut Ui,
    m: &Metrics,
    pal: &Palette,
    above_pad: f32,
    below_pad: f32,
) {
    let gap = divider_gap(m)
        .max(compact_icon_pad(m))
        .max(compact_ring_pad(m));
    ui.add_space((gap - above_pad).max(0.0));
    divider(ui, pal);
    ui.add_space((gap - below_pad).max(0.0));
}

// ---------------------------------------------------------------------------------------------
// Header (hamburger, "Quick access")
// ---------------------------------------------------------------------------------------------

/// Draws the hamburger glyph centered on `c`: three fine bars with fully rounded ends
/// (Fluent 2 style), all derived from the icon size. Pill-shaped rects are used because egui
/// strokes have no round caps. Every dimension is snapped to whole physical pixels, otherwise
/// anti-aliasing smears the thin bars and they look bigger or blurrier depending on position.
fn draw_hamburger(ui: &mut Ui, m: &Metrics, pal: &Palette, c: egui::Pos2) {
    let ppp = ui.ctx().pixels_per_point();
    let px = |v: f32| (v * ppp).round().max(1.0);
    let w_px = px(m.icon * 0.7);
    let gap_px = px(m.icon * 0.22);
    let thick_px = px(m.icon * 0.06);
    let (cx_px, cy_px) = ((c.x * ppp).round(), (c.y * ppp).round());
    let round = CornerRadius::same((thick_px / ppp * 0.5).ceil() as u8);
    for k in [-1.0_f32, 0.0, 1.0] {
        let top_px = cy_px + k * gap_px - (thick_px / 2.0).floor();
        let left_px = cx_px - (w_px / 2.0).floor();
        let bar = Rect::from_min_size(
            pos2(left_px / ppp, top_px / ppp),
            vec2(w_px / ppp, thick_px / ppp),
        );
        ui.painter().rect_filled(bar, round, pal.text);
    }
}

/// Top row of the sidebar. Returns `(toggle_compact, pin_current_folder)`.
pub(crate) fn sidebar_header(
    ui: &mut Ui,
    m: &Metrics,
    pal: &Palette,
    compact: bool,
    can_pin: bool,
) -> (bool, bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), m.nav_h), Sense::hover());
    let inner = rect.shrink2(vec2(0.0, 1.0));

    // The whole row is the hamburger button (same box as the nav items, label included). The
    // "+" button is registered afterwards, so it wins where the two overlap.
    let resp = ui.interact(inner, ui.scope_id().with("menu_btn"), Sense::click());
    list_hover(ui, inner, &resp, pal, m);
    draw_hamburger(ui, m, pal, pos2(compact_icon_cx(m, inner), inner.center().y));
    let toggle = resp
        .on_hover_text(if compact { "Expand sidebar" } else { "Collapse sidebar" })
        .clicked();

    let mut pin = false;
    if !compact {
        let sz = m.font * 1.6;
        let btn = Rect::from_center_size(
            pos2(rect.right() - m.pad * 1.5 - sz / 2.0, rect.center().y),
            vec2(sz, sz),
        );
        let presp = ui.interact(
            btn,
            ui.scope_id().with("quick_add_btn"),
            if can_pin { Sense::click() } else { Sense::hover() },
        );
        if can_pin {
            hover_fill(ui, btn, &presp, pal, m);
        }
        pin = can_pin && presp.on_hover_text("Pin the current folder to Quick access").clicked();

        // Title, aligned with the nav item labels.
        let tr = Rect::from_min_max(
            pos2(inner.left() + m.pad * 2.3 + m.icon, inner.top()),
            pos2(btn.left(), inner.bottom()),
        );
        paint_text(ui, tr, "Quick access", FontId::proportional(m.font * 0.86), pal.text_secondary, false);
    }
    (toggle, pin)
}

// ---------------------------------------------------------------------------------------------
// Navigation items
// ---------------------------------------------------------------------------------------------

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
    paint_row_bg(ui, m, pal, inner, &resp, selected.then_some(pal.control), m.font * 1.1);
    draw_nav_icon(ui, pos2(compact_icon_cx(m, inner), inner.center().y), m.icon, icon, path);
    let tr = Rect::from_min_max(
        pos2(inner.left() + m.pad * 2.3 + m.icon, inner.top()),
        pos2(inner.right() - m.pad, inner.bottom()),
    );
    paint_text(ui, tr, label, FontId::proportional(m.font), pal.text, false);
    resp
}

/// Icon-only navigation item for compacted sidebar.
pub(crate) fn nav_item_compact(
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
    paint_row_bg(ui, m, pal, inner, &resp, selected.then_some(pal.control), m.font * 1.1);
    draw_nav_icon(ui, pos2(compact_icon_cx(m, inner), inner.center().y), m.icon, icon, path);
    resp.on_hover_text(label)
}

// ---------------------------------------------------------------------------------------------
// Drives
// ---------------------------------------------------------------------------------------------

pub(crate) fn drive_card(
    ui: &mut Ui,
    m: &Metrics,
    pal: &Palette,
    d: &DriveInfo,
    active: bool,
) -> egui::Response {
    let h = card_h(m);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
    let inner = rect.shrink2(vec2(0.0, 1.0));
    paint_row_bg(ui, m, pal, inner, &resp, active.then_some(pal.control_hover), m.font * 1.6);

    let x0 = inner.left() + m.pad * 1.5;
    let x1 = inner.right() - m.pad * 1.5;
    let y1 = inner.top() + h * 0.24;
    draw_drive_icon(ui, pos2(x0 + m.icon / 2.0, y1), m.icon, d);
    paint_text(
        ui,
        Rect::from_min_max(
            pos2(x0 + m.icon + m.pad * 0.8, y1 - m.font),
            pos2(x1, y1 + m.font),
        ),
        &drive_title(d),
        FontId::proportional(m.font),
        pal.text,
        false,
    );

    // Usage bar.
    let frac = drive_fraction(d);
    let bar_h = m.accessibility_scale * 4.0;
    let bar = Rect::from_min_max(
        pos2(x0, inner.top() + h * 0.54),
        pos2(x1, inner.top() + h * 0.54 + bar_h),
    );
    ui.painter()
        .rect_filled(bar, CornerRadius::same(2), pal.divider);
    let fill = Rect::from_min_size(bar.min, vec2(bar.width() * frac, bar_h));
    ui.painter()
        .rect_filled(fill, CornerRadius::same(2), drive_fill_color(pal, frac));
    paint_text(
        ui,
        Rect::from_min_max(
            pos2(x0, inner.top() + h * 0.68),
            pos2(x1, inner.top() + h * 0.92),
        ),
        &drive_info(d),
        FontId::proportional(m.font * 0.86),
        pal.text_secondary,
        false,
    );
    resp.on_hover_text(d.mount_point.display().to_string())
}

/// Progress ring: a full track plus an arc of `frac` starting at 12 o'clock, running clockwise,
/// with round caps. Track and arc are drawn as the same kind of polyline, so they share exactly
/// the same centre line (a `circle_stroke` track next to a polyline arc ended up misaligned).
fn paint_ring(
    ui: &Ui,
    c: egui::Pos2,
    radius: f32,
    thick: f32,
    frac: f32,
    track: Color32,
    fill: Color32,
) {
    use egui::{Shape, Stroke};
    let point = |a: f32| c + radius * vec2(a.cos(), a.sin());
    let arc = |sweep: f32, n: usize| -> Vec<egui::Pos2> {
        (0..=n)
            .map(|i| point(-FRAC_PI_2 + sweep * i as f32 / n as f32))
            .collect()
    };

    let mut full = arc(TAU, 96);
    full.pop(); // the last point duplicates the first; the shape is closed instead
    ui.painter()
        .add(Shape::closed_line(full, Stroke::new(thick, track)));

    if frac > 0.0 {
        let n = ((frac * 96.0).ceil() as usize).max(2);
        ui.painter()
            .add(Shape::line(arc(TAU * frac, n), Stroke::new(thick, fill)));
        // Polyline strokes have butt ends: a circle of half the stroke width rounds each end.
        for a in [-FRAC_PI_2, -FRAC_PI_2 + TAU * frac] {
            ui.painter().circle_filled(point(a), thick * 0.5, fill);
        }
    }
}

/// Compact drive: a ring showing the fill percentage, with the drive letter ("N:") inside on
/// Windows and the drive's icon elsewhere. The hover hint has the full name and free/total space.
pub(crate) fn drive_compact(
    ui: &mut Ui,
    m: &Metrics,
    pal: &Palette,
    d: &DriveInfo,
    active: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        vec2(ui.available_width(), compact_ring_row_h(m)),
        Sense::click(),
    );
    let inner = rect.shrink2(vec2(0.0, 1.0));
    paint_row_bg(ui, m, pal, inner, &resp, active.then_some(pal.control_hover), m.font * 1.6);

    // Snap the ring's centre to a whole physical pixel so a half-pixel row centre can't shift it.
    let ppp = ui.ctx().pixels_per_point();
    let snap = |v: f32| (v * ppp).round() / ppp;
    let c = pos2(snap(compact_icon_cx(m, inner)), snap(inner.center().y));
    let radius = ring_radius(m);

    let frac = drive_fraction(d);
    paint_ring(ui, c, radius, ring_thick(m), frac, pal.divider, drive_fill_color(pal, frac));

    match drive_letter(d) {
        Some(text) => {
            // CENTER_CENTER would centre the line box (ascent + descent), leaving capitals
            // off-centre. Centre on the cap height instead, measured from a reference "H".
            let font = FontId::proportional(m.font * 0.8);
            let reference = ui
                .painter()
                .layout_no_wrap("H".to_owned(), font.clone(), pal.text);
            let galley = ui.painter().layout_no_wrap(text, font, pal.text);
            let pos = pos2(
                snap(c.x - galley.size().x / 2.0),
                snap(c.y - reference.mesh_bounds.center().y),
            );
            ui.painter().galley(pos, galley, pal.text);
        }
        None => draw_drive_icon(ui, c, radius * 1.1, d),
    }
    resp.on_hover_text(format!("{}\n{}", drive_title(d), drive_info(d)))
}

// ---------------------------------------------------------------------------------------------
// The sidebar panel
// ---------------------------------------------------------------------------------------------

impl Explorer {
    pub(crate) fn ui_sidebar(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let (m, pal) = (self.m, self.pal);
        let compact = self.sidebar_compact;
        let mut toggle_compact = false;
        let cur = self.tabs[self.active].dir.clone();
        let special_active = self.tabs[self.active].special();
        let trash_active = self.tabs[self.active].trash.is_some();
        let (quick, drives) = (&self.quick, &self.drives);
        // Folder context menus (file list) need to know what is pinned.
        ui.data_mut(|d| {
            d.insert_temp(
                Id::unique("pinned_paths"),
                Arc::new(quick.iter().map(|q| q.path.clone()).collect::<HashSet<PathBuf>>()),
            )
        });
        let can_pin = !special_active && !quick.iter().any(|q| same_path(&q.path, &cur));
        let dragging = item_drag_paths(ui);
        let old_grab_radius = ui.style().interaction.resize_grab_radius_side;
        ui.style_mut().interaction.resize_grab_radius_side = 2.0;

        let compact_w = m.accessibility_scale * 56.0;
        // A separate id per mode, so the expanded width the user dragged to is remembered.
        let panel = egui::Panel::left(if compact { "sidebar_compact" } else { "sidebar" })
            .resizable(!compact)
            .default_size(if compact {
                compact_w
            } else {
                // Only used the first time the panel is shown; egui remembers the dragged width
                // after that. The size range below keeps a stale saved value in bounds.
                self.sidebar_width
                    .unwrap_or(m.accessibility_scale * 200.0)
            })
            .size_range(if compact {
                compact_w..=compact_w
            } else {
                m.accessibility_scale * 150.0..=m.accessibility_scale * 400.0
            })
            .show_separator_line(false)
            .frame(
                bar_frame(&pal, m.pad * 0.6, m.pad * 0.6, m.pad * 0.4, m.pad * 0.4).corner_radius(
                    CornerRadius {
                        nw: 0,
                        ne: 0,
                        sw: if get_desktop().contains("GNOME") && !Self::is_maximized(ui.ctx()) && self.settings.custom_frame {15}else{0},
                        se: 0,
                    },
                ),
            )
            .show(ui, |ui| {
                // Bottom-up: the Recycle Bin is pinned, the rest scrolls only when it has to.
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    let bin = if compact {
                        nav_item_compact(ui, &m, &pal, "recycle_bin", None, "Recycle Bin", trash_active)
                    } else {
                        nav_item(ui, &m, &pal, "recycle_bin", "Recycle Bin", trash_active)
                    };
                    if bin.clicked() {
                        actions.push(Action::OpenRecycleBin);
                    }
                    if compact {
                        // Below the divider is the Recycle Bin row, above it the drive rings.
                        compact_divider(ui, &m, &pal, compact_icon_pad(&m), compact_ring_pad(&m));
                    } else {
                        ui.add_space(bin_divider_pad(&m));
                        divider(ui, &pal);
                        ui.add_space(bin_divider_pad(&m));
                    }

                    ui.with_layout(Layout::top_down(Align::Min), |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let (toggle, pin) = sidebar_header(ui, &m, &pal, compact, can_pin);
                        toggle_compact = toggle;
                        if pin {
                            actions.push(Action::PinQuick(cur.clone()));
                        }
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                for q in quick.iter().filter(|q| q.available) {
                                    let sel = same_path(&q.path, &cur) && !special_active;
                                    let path = Some(q.path.as_path());
                                    let resp = if compact {
                                        nav_item_compact(ui, &m, &pal, q.icon, path, &q.label, sel)
                                    } else {
                                        nav_item_ex(ui, &m, &pal, q.icon, path, &q.label, sel)
                                    };
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
                                        if archive_of(&q.path).is_none() {
                                            if menu_item(ui, &m, &pal, Some("image"), "Change folder icon…", None, true) {
                                                actions.push(Action::ChangeFolderIcon(q.path.clone()));
                                            }
                                            if custom_icon_of(&q.path).is_some()
                                                && menu_item(ui, &m, &pal, Some("refresh"), "Reset folder icon", None, true)
                                            {
                                                actions.push(Action::ResetFolderIcon(q.path.clone()));
                                            }
                                        }
                                        ui.separator();
                                        if menu_item(ui, &m, &pal, Some("close"), "Remove from Quick access", None, true) {
                                            actions.push(Action::UnpinQuick(q.path.clone()));
                                        }
                                    });
                                }
                                if !compact && !quick.iter().any(|q| q.available) {
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
                                    if compact {
                                        // No room for a title: a divider separates the groups. The
                                        // first one follows the folder icons, the others a ring.
                                        let above = if any_drive {
                                            compact_ring_pad(&m)
                                        } else {
                                            compact_icon_pad(&m)
                                        };
                                        compact_divider(ui, &m, &pal, above, compact_ring_pad(&m));
                                    } else {
                                        ui.add_space(group_gap(&m));
                                        section_label(ui, &m, &pal, title);
                                    }
                                    any_drive = true;
                                    for d in group {
                                        let active = same_path(&d.mount_point, &cur) && !special_active;
                                        let resp = if compact {
                                            drive_compact(ui, &m, &pal, d, active)
                                        } else {
                                            drive_card(ui, &m, &pal, d, active)
                                        };
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
                                if !any_drive && !compact {
                                    ui.add_space(group_gap(&m));
                                    section_label(ui, &m, &pal, "Drives");
                                    section_label(ui, &m, &pal, "No drives found");
                                }
                            });
                    });
                });
            });
        // Reset the style's drag radius for other widgets.
        ui.style_mut().interaction.resize_grab_radius_side = old_grab_radius;
        self.sidebar_rect = Some(panel.response.rect);
        // Track the expanded width for the session (the compact width is fixed, not saved).
        // Whole points, so sub-pixel jitter doesn't count as a change.
        if !compact {
            self.sidebar_width = Some(panel.response.rect.width().round());
        }
        if toggle_compact {
            self.sidebar_compact = !self.sidebar_compact;
        }
    }
}
