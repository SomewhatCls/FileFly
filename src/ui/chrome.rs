//! Custom title bar parts: caption buttons, resize handles, tab wings.

use crate::prelude::*;

/// Fills the region between a right-angle corner at `apex` and a quarter circle whose centre lies at
/// `apex + (sx, sy) * r`. Used for concave "inner corner" rounding (tab wings, sidebar/file-view junction).
pub(crate) fn corner_fillet(
    painter: &egui::Painter,
    apex: egui::Pos2,
    r: f32,
    sx: f32,
    sy: f32,
    color: Color32,
) {
    if r < 0.5 {
        return;
    }
    // The fillet meets its neighbours along its two straight sides (the ones through `apex`), which
    // usually sit between physical pixels. Two separately anti-aliased edges that merely touch leave a
    // hairline seam, so the shape is extended one pixel past both sides, into the neighbours (which
    // share its colour). The arc itself is untouched.
    let ov = 1.0 / painter.pixels_per_point();
    let c = pos2(apex.x + sx * r, apex.y + sy * r);
    let mut mesh = egui::Mesh::default();
    // Fan centre: the apex pushed out into both neighbours.
    mesh.colored_vertex(pos2(apex.x - sx * ov, apex.y - sy * ov), color);
    // Start of the arc, extended along the first straight side.
    mesh.colored_vertex(pos2(apex.x - sx * ov, apex.y + sy * r), color);
    const N: usize = 10;
    for i in 0..=N {
        let a = std::f32::consts::FRAC_PI_2 * i as f32 / N as f32;
        mesh.colored_vertex(pos2(c.x - sx * r * a.cos(), c.y - sy * r * a.sin()), color);
    }
    // End of the arc, extended along the second straight side.
    mesh.colored_vertex(pos2(apex.x + sx * r, apex.y - sy * ov), color);
    for i in 0..(N + 2) {
        mesh.add_triangle(0, 1 + i as u32, 2 + i as u32);
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// Concave fillet at the bottom corner of a tab.
pub(crate) fn tab_wing(ui: &Ui, corner: egui::Pos2, r: f32, left: bool, color: Color32) {
    corner_fillet(
        ui.painter(),
        corner,
        r,
        if left { -1.0 } else { 1.0 },
        -1.0,
        color,
    );
}

/// Band behind the nav/command rows: only its top-left corner can be rounded.
pub(crate) fn band_frame(pal: &Palette, nw: u8, l: f32, r: f32, t: f32, b: f32) -> egui::Frame {
    egui::Frame::new()
        .fill(pal.layer)
        .corner_radius(CornerRadius {
            nw: if pal.transparency != TransparencyEffect::None { nw } else { 0 },
            ne: 0,
            sw: 0,
            se: 0,
        })
        .inner_margin(Margin {
            left: l as i8,
            right: r as i8,
            top: t as i8,
            bottom: b as i8,
        })
}

/// Minimize / maximize-restore / close, drawn with the painter so they stay crisp at any scale.
pub(crate) fn caption_buttons(ui: &mut Ui, m: &Metrics, pal: &Palette, maximized: bool, show_max: bool) {
    if get_desktop().is_empty(){
        caption_buttons_windows(ui, m, pal, maximized, show_max);
    }else if get_desktop().contains("KDE"){
        caption_buttons_kde(ui, m, pal, maximized, show_max);
    }else{
        caption_buttons_gnome(ui, m, pal, maximized, show_max);
    }
}

fn get_desktop() -> String{
    std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default()
}

/// Draws KDE-style caption buttons (minimize, maximize, close, restore).
pub(crate) fn caption_buttons_kde(ui: &mut Ui, m: &Metrics, pal: &Palette, maximized: bool, show_max: bool) {
    let kinds: &[usize] = if show_max { &[0, 1, 2] } else { &[0, 2] };
    let ppp = ui.ctx().pixels_per_point();
    let snap = |v: f32| (v * ppp).round() / ppp;

    let diameter = snap(22.0 * m.accessibility_scale); // hover circle
    let pitch = snap(26.0 * m.accessibility_scale);    // center-to-center distance
    let margin = snap(6.0 * m.accessibility_scale);    // gap to the window's right edge

    // KDE icon size approximation
    let icon_size = snap(24.0 * m.accessibility_scale);

    let rect = ui.ctx().content_rect();
    let origin = rect.right_top();
    let n = kinds.len();

    for (slot, &k) in kinds.iter().enumerate() {
        let right = origin.x - margin - pitch * (n - 1 - slot) as f32;
        let c = pos2(snap(right - diameter / 2.0), snap(origin.y + m.titlebar_h / 2.0));
        let r = Rect::from_center_size(c, vec2(diameter, diameter));

        let resp = ui.interact(r, Id::unique(("caption", k)), Sense::click());
        let close = k == 2;

        // Breeze: no resting background, a circle on hover/press,
        // red for close.
        let bg = if resp.is_pointer_button_down_on() {
            if close {
                Color32::from_rgb(0xB0, 0x34, 0x40)
            } else {
                pal.subtle_pressed
            }
        } else if resp.hovered() {
            if close {
                Color32::from_rgb(0xDA, 0x44, 0x53)
            } else {
                pal.subtle_hover
            }
        } else {
            Color32::TRANSPARENT
        };
        if bg != Color32::TRANSPARENT {
            ui.painter().circle_filled(c, diameter / 2.0, bg);
        }

        let fg = if close && (resp.hovered() || resp.is_pointer_button_down_on()) {
            Color32::WHITE
        } else {
            pal.text
        };

        let name = match k {
            0 => "kde_minimize",
            1 if maximized => "kde_restore",
            1 => "kde_maximize",
            _ => "kde_close",
        };
        draw_icon(ui, c, icon_size, name, fg);

        if resp.clicked() {
            ui.send_viewport_cmd(match k {
                0 => egui::ViewportCommand::Minimized(true),
                1 => egui::ViewportCommand::Maximized(!maximized),
                _ => egui::ViewportCommand::Close,
            });
        }
    }
}

/// Draws GNOME-style caption buttons (minimize, maximize, close, restore).
pub(crate) fn caption_buttons_gnome(ui: &mut Ui, m: &Metrics, pal: &Palette, maximized: bool, show_max: bool) {
    let kinds: &[usize] = if show_max { &[0, 1, 2] } else { &[0, 2] };
    let ppp = ui.ctx().pixels_per_point();
    let snap = |v: f32| (v * ppp).round() / ppp;
    let diameter = snap(24.0 * m.accessibility_scale); // circle
    let pitch = snap(37.0 * m.accessibility_scale);    // center-to-center distance
    let margin = snap(6.0 * m.accessibility_scale);    // gap to the window's right edge

    let rect = ui.ctx().content_rect();
    let origin = rect.right_top();
    let n = kinds.len();

    // GNOME Icon size approximation
    let icon_size = snap(24.0 * m.accessibility_scale);

    for (slot, &k) in kinds.iter().enumerate() {
        let right = origin.x - margin - pitch * (n - 1 - slot) as f32;
        let c = pos2(snap(right - diameter / 2.0), snap(origin.y + m.titlebar_h / 2.0));
        let r = Rect::from_center_size(c, vec2(diameter, diameter));

        let resp = ui.interact(r, Id::unique(("caption", k)), Sense::click());
        let alpha = if resp.is_pointer_button_down_on() {
            0.30
        } else if resp.hovered() {
            0.15
        } else {
            0.10
        };
        ui.painter()
            .circle_filled(c, diameter / 2.0, pal.text.gamma_multiply(alpha));

        let name = match k {
            0 => "gnome_minimize",
            1 if maximized => "gnome_restore",
            1 => "gnome_maximize",
            _ => "gnome_close",
        };
        draw_icon(ui, c, snap(icon_size), name, pal.text);

        if resp.clicked() {
            ui.send_viewport_cmd(match k {
                0 => egui::ViewportCommand::Minimized(true),
                1 => egui::ViewportCommand::Maximized(!maximized),
                _ => egui::ViewportCommand::Close,
            });
        }
    }
}


/// Draws Windows-style caption buttons (minimize, maximize, close, restore).
pub(crate) fn caption_buttons_windows(ui: &mut Ui, m: &Metrics, pal: &Palette, maximized: bool, show_max: bool) {
    let kinds: &[usize] = if show_max { &[0, 1, 2] } else { &[0, 2] };
    let ppp = ui.ctx().pixels_per_point();
    let snap = |v: f32| (v * ppp).round() / ppp;
    let (bw, bh) = (snap(46.0 * m.accessibility_scale), snap(28.0 * m.accessibility_scale));
    let origin = ui.ctx().content_rect().right_top(); // flush top-right, like #controls in the CSS
    let left = origin.x - bw * kinds.len() as f32;

    // Windows Icon size approximation
    let icon_size = snap(13.33 * m.accessibility_scale);

    for (slot, &k) in kinds.iter().enumerate() {
        let r = Rect::from_min_size(pos2(left + bw * slot as f32, origin.y), vec2(bw, bh));
        let resp = ui.interact(r, Id::unique(("caption", k)), Sense::click());
        let close = k == 2;

        let bg = if resp.is_pointer_button_down_on() {
            if close {
                Color32::from_rgb(0xB3, 0x28, 0x1A)
            } else {
                pal.subtle_pressed
            }
        } else if resp.hovered() {
            if close {
                Color32::from_rgb(0xC4, 0x2B, 0x1C)
            } else {
                pal.subtle_hover
            }
        } else {
            Color32::TRANSPARENT
        };
        if bg != Color32::TRANSPARENT {
            ui.painter().rect_filled(r, CornerRadius::ZERO, bg);
        }

        let fg = if close && resp.hovered() {
            Color32::WHITE
        } else {
            pal.text
        };

        let name = match k {
            0 => "window_minimize",
            1 if maximized => "window_restore",
            1 => "window_maximize",
            _ => "window_close",
        };
        draw_icon(ui, r.center(), icon_size, name, fg);

        if resp.clicked() {
            ui.send_viewport_cmd(match k {
                0 => egui::ViewportCommand::Minimized(true),
                1 => egui::ViewportCommand::Maximized(!maximized),
                _ => egui::ViewportCommand::Close,
            });
        }
    }
}

/// Undecorated windows have no native resize border, so handle the edges ourselves.
pub(crate) fn handle_resize(ctx: &egui::Context, m: &Metrics) {
    use egui::viewport::ResizeDirection as D;
    let (maximized, fullscreen) = ctx.input(|i| {
        (
            i.viewport().maximized.unwrap_or(false),
            i.viewport().fullscreen.unwrap_or(false),
        )
    });
    if maximized || fullscreen {
        return;
    }
    let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) else {
        return;
    };
    let r = ctx.content_rect();
    let (b, c) = (m.accessibility_scale * 5.0, m.accessibility_scale * 12.0);
    let (el, er, et, eb) = (
        pos.x - r.left(),
        r.right() - pos.x,
        pos.y - r.top(),
        r.bottom() - pos.y,
    );
    let left = el < b || (el < c && (et < b || eb < b));
    let right = er < b || (er < c && (et < b || eb < b));
    let top = et < b || (et < c && (el < b || er < b));
    let bottom = eb < b || (eb < c && (el < b || er < b));
    let (dir, cursor) = match (left, right, top, bottom) {
        (true, _, true, _) => (D::NorthWest, egui::CursorIcon::ResizeNorthWest),
        (_, true, true, _) => (D::NorthEast, egui::CursorIcon::ResizeNorthEast),
        (true, _, _, true) => (D::SouthWest, egui::CursorIcon::ResizeSouthWest),
        (_, true, _, true) => (D::SouthEast, egui::CursorIcon::ResizeSouthEast),
        (true, ..) => (D::West, egui::CursorIcon::ResizeWest),
        (_, true, ..) => (D::East, egui::CursorIcon::ResizeEast),
        (_, _, true, _) => {
            if pos.x > r.right() - m.accessibility_scale * 46.0 * 3.0 {
                return; // don't steal clicks from the caption buttons
            }
            (D::North, egui::CursorIcon::ResizeNorth)
        }
        (_, _, _, true) => (D::South, egui::CursorIcon::ResizeSouth),
        _ => return,
    };
    ctx.set_cursor_icon(cursor);
    if ctx.input(|i| i.pointer.primary_pressed()) {
        ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
        // The OS resize loop swallows the button/touch release (see `raw_input_hook`).
        ctx.data_mut(|d| d.insert_temp(Id::unique("pointer_reset"), true));
        ctx.request_repaint();
    }
}
