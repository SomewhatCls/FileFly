//! Small reusable widgets and frames.

use crate::prelude::*;

pub(crate) fn bar_frame(pal: &Palette, l: f32, r: f32, t: f32, b: f32) -> egui::Frame {
    egui::Frame::new()
        .fill(if pal.transparency != TransparencyEffect::None {
            Color32::TRANSPARENT
        } else {
            pal.base
        })
        .inner_margin(Margin {
            left: l as i8,
            right: r as i8,
            top: t as i8,
            bottom: b as i8,
        })
}

/// The big rounded "card" on the right (nav row, command bar, file list, status bar share it).
pub(crate) fn layer_frame(
    pal: &Palette,
    radius: u8,
    l: f32,
    r: f32,
    t: f32,
    b: f32,
    round_top_left: bool,
) -> egui::Frame {
    let cr = if round_top_left && pal.transparency != TransparencyEffect::None {
        CornerRadius {
            nw: radius * 2,
            ne: 0,
            sw: 0,
            se: 0,
        }
    } else {
        CornerRadius::ZERO
    };
    egui::Frame::new()
        .fill(pal.layer)
        .corner_radius(cr)
        .inner_margin(Margin {
            left: l as i8,
            right: r as i8,
            top: t as i8,
            bottom: b as i8,
        })
}

/// Wraps a pill around a label with a given color.
pub(crate) fn status_pill(ui: &mut Ui,  pal: &Palette, m: &Metrics, bg: Color32, text_color:  Color32, text: &str) {
    egui::Frame::new()
        .fill(bg)
        .corner_radius(CornerRadius::same(255))
        .inner_margin(Margin::symmetric(10, 3))
        .outer_margin(Margin::symmetric(2, (m.pad * 0.4) as i8))
        .show(ui, |ui| {
        ui.label(
            RichText::new(text)
                .color(text_color)
                .strong()
                .extra_letter_spacing(0.5)
                .size(m.font),
        )}
    );
}

pub(crate) fn paint_text(ui: &Ui, rect: Rect, text: &str, font: FontId, color: Color32, right: bool) {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(rect.width().max(1.0));
    let galley = ui.painter().layout_job(job);
    let x = if right {
        rect.right() - galley.size().x
    } else {
        rect.left()
    };
    let pos = pos2(x, rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(pos, galley, color);
}

pub(crate) fn text_width(ui: &Ui, text: &str, size: f32) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_owned(), FontId::proportional(size), Color32::WHITE)
        .size()
        .x
}

pub(crate) fn hover_fill(ui: &Ui, rect: Rect, resp: &egui::Response, pal: &Palette, m: &Metrics) {
    let c = if resp.is_pointer_button_down_on() {
        pal.subtle_pressed
    } else if resp.hovered() {
        pal.subtle_hover
    } else {
        return;
    };
    ui.painter()
        .rect_filled(rect, CornerRadius::same(m.radius), c);
}

/// Hover / press feedback for list-like items (quick access, drives): accent-tinted fill plus a
/// small accent marker, so the item under the pointer is easy to spot.
pub(crate) fn list_hover(ui: &Ui, rect: Rect, resp: &egui::Response, pal: &Palette, m: &Metrics) {
    let down = resp.is_pointer_button_down_on();
    if !down && !resp.hovered() {
        return;
    }
    let fill = if down {
        pal.hover.gamma_multiply(1.6)
    } else {
        pal.hover
    };
    ui.painter()
        .rect_filled(rect, CornerRadius::same(m.radius), fill);
    let marker = Rect::from_center_size(
        pos2(rect.left() + m.s * 2.0, rect.center().y),
        vec2(m.s * 3.0, m.font * 0.8),
    );
    ui.painter()
        .rect_filled(marker, CornerRadius::same(2), pal.accent.gamma_multiply(0.6));
}

pub(crate) fn icon_button(
    ui: &mut Ui,
    m: &Metrics,
    pal: &Palette,
    icon: &str,
    tip: &str,
    enabled: bool,
    toggled: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        vec2(m.ctl_h, m.ctl_h),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    if toggled {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(m.radius), pal.selected);
    } else if enabled {
        hover_fill(ui, rect, &resp, pal, m);
    }
    let c = if enabled { pal.text } else { pal.text_disabled };
    draw_icon(ui, rect.center(), m.icon * 0.85, icon, c);
    resp.on_hover_text(tip)
}

pub(crate) fn cmd_button(
    ui: &mut Ui,
    m: &Metrics,
    pal: &Palette,
    icon: &str,
    label: Option<&str>,
    tip: &str,
    enabled: bool,
    toggled: bool,
) -> egui::Response {
    let isz = m.icon * 0.85;
    let w = match label {
        Some(l) => m.pad * 2.0 + isz + m.pad * 0.6 + text_width(ui, l, m.font),
        None => m.ctl_h,
    };
    let (rect, resp) = ui.allocate_exact_size(
        vec2(w, m.ctl_h),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    if toggled {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(m.radius), pal.selected);
    } else if enabled {
        hover_fill(ui, rect, &resp, pal, m);
    }
    let c = if enabled { pal.text } else { pal.text_disabled };
    match label {
        Some(l) => {
            draw_icon(
                ui,
                pos2(rect.left() + m.pad + isz / 2.0, rect.center().y),
                isz,
                icon,
                c,
            );
            let tr =
                Rect::from_min_max(pos2(rect.left() + m.pad * 1.6 + isz, rect.top()), rect.max);
            paint_text(ui, tr, l, FontId::proportional(m.font), c, false);
        }
        None => draw_icon(ui, rect.center(), isz, icon, c),
    }
    resp.on_hover_text(tip)
}

/// Truncates `text` with "…" so it fits in `max_w`.
fn fit_text(ui: &Ui, text: &str, font: f32, max_w: f32) -> String {
    if text_width(ui, text, font) <= max_w {
        return text.to_owned();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while chars.pop().is_some() {
        let cand = format!("{}…", chars.iter().collect::<String>());
        if text_width(ui, &cand, font) <= max_w {
            return cand;
        }
    }
    "…".to_owned()
}

/// creates a drop-down menu for the toolbar.
pub(crate) fn cmd_dropdown<R>(
    ui: &mut Ui,
    m: &Metrics,
    pal: &Palette,
    icon: &str,
    label: Option<&str>,
    tip: &str,
    enabled: bool,
    max_label_w: f32,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> Option<R> {
    let icon_size = m.icon * 0.85;
    let chevron = m.small_icon * 0.7;
    let label = label.map(|l| fit_text(ui, l, m.font, max_label_w));

    let width = match &label {
        Some(l) => m.pad * 3.2 + icon_size + chevron + text_width(ui, l, m.font),
        None => m.pad * 2.4 + icon_size + chevron,
    };
    let (rect, resp) = ui.allocate_exact_size(
        vec2(width, m.ctl_h),
        if enabled { Sense::click() } else { Sense::hover() },
    );

    // Show button as pressed while drop-down menu is opened.
    let open_id = resp.id.with("dd_open");
    let open = ui.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);

    if open {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(m.radius), pal.selected);
    } else if enabled {
        hover_fill(ui, rect, &resp, pal, m);
    }

    let color = if enabled { pal.text } else { pal.text_disabled };
    draw_icon(
        ui,
        pos2(rect.left() + m.pad + icon_size / 2.0, rect.center().y),
        icon_size,
        icon,
        color,
    );
    let chevron_color = pos2(rect.right() - m.pad - chevron / 2.0, rect.center().y);
    draw_icon(
        ui,
        chevron_color,
        chevron,
        "chevron_down",
        if enabled { pal.text_secondary } else { pal.text_disabled },
    );
    if let Some(l) = &label {
        let tr = Rect::from_min_max(
            pos2(rect.left() + m.pad * 1.6 + icon_size, rect.top()),
            pos2(chevron_color.x - chevron / 2.0 - m.pad * 0.6, rect.bottom()),
        );
        paint_text(ui, tr, l, FontId::proportional(m.font), color, false);
    }

    // Don't show the tooltip on top of the menu.
    let resp = if open { resp } else { resp.on_hover_text(tip) };
    if !enabled {
        return None;
    }

    let res = egui::Popup::menu(&resp)
        .width(rect.width().max(m.s * 180.0))
        .show(add_contents);
    ui.data_mut(|d| d.insert_temp(open_id, res.is_some()));
    res.map(|r| r.inner)
}

pub(crate) fn vsep(ui: &mut Ui, m: &Metrics, pal: &Palette) {
    let (r, _) = ui.allocate_exact_size(vec2(m.pad, m.ctl_h * 0.6), Sense::hover());
    ui.painter().line_segment(
        [pos2(r.center().x, r.top()), pos2(r.center().x, r.bottom())],
        Stroke::new(1.0_f32, pal.divider),
    );
}

pub(crate) fn menu_item(
    ui: &mut Ui,
    m: &Metrics,
    pal: &Palette,
    icon: Option<&str>,
    label: &str,
    hint: Option<&str>,
    enabled: bool,
) -> bool {
    let w = ui.available_width().max(m.s * 230.0);
    let (rect, resp) = ui.allocate_exact_size(
        vec2(w, m.row_h * 0.95),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    if enabled && resp.hovered() {
        ui.painter().rect_filled(
            rect.shrink2(vec2(0.0, 1.0)),
            CornerRadius::same(m.radius),
            pal.control_hover,
        );
    }
    let col = if enabled { pal.text } else { pal.text_disabled };
    if let Some(i) = icon {
        draw_icon(
            ui,
            pos2(rect.left() + m.pad + m.small_icon / 2.0, rect.center().y),
            m.small_icon,
            i,
            col,
        );
    }
    let text_rect = Rect::from_min_max(
        pos2(rect.left() + m.pad * 2.2 + m.small_icon, rect.top()),
        pos2(rect.right() - m.pad, rect.bottom()),
    );
    paint_text(
        ui,
        text_rect,
        label,
        FontId::proportional(m.font),
        col,
        false,
    );
    if let Some(h) = hint {
        paint_text(
            ui,
            text_rect,
            h,
            FontId::proportional(m.font * 0.86),
            pal.text_secondary,
            true,
        );
    }
    resp.clicked()
}

/// Contents of a context menu that can never run off the window: when the menu is taller than the
/// window it scrolls instead of being cut off (the popup itself already shifts to stay on screen).
pub(crate) fn menu_scroll(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    // Room for the popup frame (margins + stroke) and a little air to the window edges.
    let max_h = (ui.ctx().content_rect().height() - 40.0).max(120.0);
    egui::ScrollArea::vertical()
        .max_height(max_h)
        .auto_shrink([true, true])
        .show(ui, add);
}

/// `resp.context_menu(..)` with the scrolling behavior of `menu_scroll`.
pub(crate) fn scroll_context_menu(resp: &egui::Response, add: impl FnOnce(&mut Ui)) {
    resp.context_menu(|ui| menu_scroll(ui, add));
}

pub(crate) fn primary_button(ui: &mut Ui, m: &Metrics, pal: &Palette, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(pal.on_accent))
            .fill(pal.accent)
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(m.radius))
            .min_size(vec2(m.s * 92.0, m.ctl_h)),
    )
}

pub(crate) fn secondary_button(ui: &mut Ui, m: &Metrics, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(text)
            .corner_radius(CornerRadius::same(m.radius))
            .min_size(vec2(m.s * 92.0, m.ctl_h)),
    )
}

/// Fluent-style dialog: rounded flyout surface, optional dimmed scrim. Returns true when closed (X / Esc).
pub(crate) fn dialog_shell(
    ui: &mut Ui,
    m: &Metrics,
    pal: &Palette,
    id: &str,
    title: &str,
    modal: bool,
    width: f32,
    add: impl FnOnce(&mut Ui),
) -> bool {
    if modal {
        let screen = ui.content_rect();
        egui::Area::new(Id::unique((id, "scrim")))
            .order(egui::Order::Middle)
            .fixed_pos(screen.min)
            .show(ui, |ui| {
                let (rect, _) = ui.allocate_exact_size(screen.size(), Sense::click_and_drag());
                ui.painter().rect_filled(
                    rect,
                    CornerRadius::ZERO,
                    Color32::from_black_alpha(if pal.dark { 95 } else { 48 }),
                );
            });
    }
    let mut closed = false;
    egui::Window::new(title)
        .id(Id::unique(id))
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .order(egui::Order::Foreground)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .frame(
            egui::Frame::window(&ui.global_style())
                .fill(pal.flyout)
                .shadow(popup_shadow(pal.dark))
                .stroke(Stroke::new(1.0_f32, pal.divider))
                .inner_margin(Margin::same((m.pad * 2.0) as i8)),
        )
        .show(ui, |ui| {
            ui.set_width(width);
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).size(m.font * 1.3).strong());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(ui, m, pal, "close", "Close", true, false).clicked() {
                        closed = true;
                    }
                });
            });
            ui.add_space(m.pad);
            add(ui);
        });
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) && !ui.egui_wants_keyboard_input() {
        closed = true;
    }
    closed
}

pub(crate) fn theme_pref(t: ThemeChoice) -> egui::ThemePreference {
    match t {
        ThemeChoice::System => egui::ThemePreference::System,
        ThemeChoice::Light => egui::ThemePreference::Light,
        ThemeChoice::Dark => egui::ThemePreference::Dark,
    }
}

pub(crate) fn danger_button(ui: &mut Ui, m: &Metrics, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(Color32::WHITE))
            .fill(Color32::from_rgb(0xC4, 0x2B, 0x1C))
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(m.radius))
            .min_size(vec2(m.s * 92.0, m.ctl_h)),
    )
}
