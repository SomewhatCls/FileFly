//! Small reusable widgets and frames.

use crate::prelude::*;

fn margin(l: f32, r: f32, t: f32, b: f32) -> Margin {
    Margin { left: l as i8, right: r as i8, top: t as i8, bottom: b as i8 }
}

fn sense(enabled: bool) -> Sense {
    if enabled { Sense::click() } else { Sense::hover() }
}

fn text_color(pal: &Palette, enabled: bool) -> Color32 {
    if enabled { pal.text } else { pal.text_disabled }
}

pub(crate) fn bar_frame(pal: &Palette, l: f32, r: f32, t: f32, b: f32) -> egui::Frame {
    let fill = if pal.transparency != TransparencyEffect::None { Color32::TRANSPARENT } else { pal.base };
    egui::Frame::new().fill(fill).inner_margin(margin(l, r, t, b))
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
        CornerRadius { nw: radius * 2, ..CornerRadius::ZERO }
    } else {
        CornerRadius::ZERO
    };
    egui::Frame::new().fill(pal.layer).corner_radius(cr).inner_margin(margin(l, r, t, b))
}

/// Wraps a pill around a label with a given color.
pub(crate) fn status_pill(ui: &mut Ui, m: &Metrics, bg: Color32, text_color: Color32, text: &str) {
    ui.style_mut().interaction.selectable_labels = false;
    ui.spacing_mut().item_spacing.x = 7.0;
    egui::Frame::new()
        .fill(bg)
        .corner_radius(CornerRadius::same(255))
        .inner_margin(Margin::symmetric(9, 0))
        .outer_margin(Margin::symmetric(0, (m.pad * 0.6) as i8))
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(text_color).strong().size(m.font * 0.93))
        });
}

/// Draws text and returns whether the text was truncated.
pub(crate) fn paint_text(
    ui: &Ui,
    rect: Rect,
    text: &str,
    font: FontId,
    color: Color32,
    right: bool,
) -> bool {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(rect.width().max(1.0));
    let galley = ui.painter().layout_job(job);
    let elided = galley.elided;
    let x = if right { rect.right() - galley.size().x } else { rect.left() };
    let pos = pos2(x, rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(pos, galley, color);
    elided
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
    ui.painter().rect_filled(rect, CornerRadius::same(m.radius), c);
}

/// Hover / press feedback for list-like items (quick access, drives): accent-tinted fill plus a
/// small accent marker, so the item under the pointer is easy to spot.
pub(crate) fn list_hover(ui: &Ui, rect: Rect, resp: &egui::Response, pal: &Palette, m: &Metrics) {
    let down = resp.is_pointer_button_down_on();
    if !down && !resp.hovered() {
        return;
    }
    let fill = if down { pal.hover.gamma_multiply(1.6) } else { pal.hover };
    ui.painter().rect_filled(rect, CornerRadius::same(m.radius), fill);
    let marker = Rect::from_center_size(
        pos2(rect.left() + m.accessibility_scale * 2.0, rect.center().y),
        vec2(m.accessibility_scale * 3.0, m.font * 0.8),
    );
    ui.painter()
        .rect_filled(marker, CornerRadius::same(2), pal.accent.gamma_multiply(0.6));
}

/// Visual style of a [`Btn`]; all kinds share hover / pressed / disabled behaviour.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum BtnKind {
    /// Transparent until hovered (toolbars, title bars).
    Subtle,
    /// Bordered neutral control (dialogs, settings).
    Standard,
    /// Accent-filled call to action.
    Accent,
    /// Red destructive action.
    Danger,
}

/// Scales the RGB channels of an opaque colour (hover / pressed shades of filled buttons).
fn shade(c: Color32, f: f32) -> Color32 {
    let s = |v: u8| (v as f32 * f).round() as u8;
    Color32::from_rgb(s(c.r()), s(c.g()), s(c.b()))
}

/// The one Fluent button: optional icon and label, tooltip, toggled state and a [`BtnKind`].
#[derive(Clone, Copy)]
pub(crate) struct Btn<'a> {
    icon: Option<&'a str>,
    label: Option<&'a str>,
    tip: Option<&'a str>,
    kind: BtnKind,
    enabled: bool,
    toggled: bool,
}

impl<'a> Btn<'a> {
    pub(crate) fn new() -> Self {
        Self { icon: None, label: None, tip: None, kind: BtnKind::Subtle, enabled: true, toggled: false }
    }
    pub(crate) fn icon(mut self, icon: &'a str) -> Self { self.icon = Some(icon); self }
    pub(crate) fn label(mut self, label: &'a str) -> Self { self.label = Some(label); self }
    pub(crate) fn tip(mut self, tip: &'a str) -> Self { self.tip = Some(tip); self }
    pub(crate) fn kind(mut self, kind: BtnKind) -> Self { self.kind = kind; self }
    pub(crate) fn enabled(mut self, enabled: bool) -> Self { self.enabled = enabled; self }
    pub(crate) fn toggled(mut self, toggled: bool) -> Self { self.toggled = toggled; self }

    pub(crate) fn show(self, ui: &mut Ui, m: &Metrics, pal: &Palette) -> egui::Response {
        let enabled = self.enabled && ui.is_enabled();
        let filled = self.kind != BtnKind::Subtle;
        let isz = if self.icon.is_some() { m.icon * 0.85 } else { 0.0 };
        let gap = if self.icon.is_some() && self.label.is_some() { m.pad * 0.6 } else { 0.0 };
        let text_w = self.label.map_or(0.0, |l| text_width(ui, l, m.font));
        let content_w = isz + gap + text_w;
        let w = if self.label.is_none() {
            m.ctl_h
        } else if filled {
            (m.pad * 2.0 + content_w).max(m.accessibility_scale * 92.0)
        } else {
            m.pad * 2.0 + content_w
        };
        let (rect, resp) = ui.allocate_exact_size(vec2(w, m.ctl_h), sense(enabled));
        let radius = CornerRadius::same(m.radius);
        let (down, hover) = (resp.is_pointer_button_down_on(), resp.hovered());

        // Background, then (for enabled buttons) hover / pressed feedback.
        let accent_fill = match self.kind {
            BtnKind::Accent => Some(pal.accent),
            BtnKind::Danger => Some(DANGER_RED),
            _ => None,
        };
        let text_col = match (enabled, self.kind) {
            (false, _) => pal.text_disabled,
            (_, BtnKind::Accent) => pal.on_accent,
            (_, BtnKind::Danger) => Color32::WHITE,
            _ => pal.text,
        };
        match accent_fill {
            Some(fill) if enabled => {
                let f = if down { 0.8 } else if hover { 0.9 } else { 1.0 };
                ui.painter().rect_filled(rect, radius, shade(fill, f));
            }
            Some(_) => { ui.painter().rect_filled(rect, radius, pal.subtle_hover); }
            None => {
                if self.kind == BtnKind::Standard {
                    ui.painter().rect_filled(rect, radius, pal.control);
                    ui.painter()
                        .rect_stroke(rect, radius, Stroke::new(1.0_f32, pal.control_stroke), StrokeKind::Inside);
                }
                if self.toggled {
                    ui.painter().rect_filled(rect, radius, pal.selected);
                } else if enabled {
                    hover_fill(ui, rect, &resp, pal, m);
                }
            }
        }

        let mut x = rect.center().x - content_w / 2.0;
        if let Some(icon) = self.icon {
            draw_icon(ui, pos2(x + isz / 2.0, rect.center().y), isz, icon, text_col);
            x += isz + gap;
        }
        if let Some(l) = self.label {
            let tr = Rect::from_min_max(pos2(x, rect.top()), pos2(rect.right() - m.pad, rect.bottom()));
            paint_text(ui, tr, l, FontId::proportional(m.font), text_col, false);
        }
        match self.tip {
            Some(t) => resp.on_hover_text(t),
            None => resp,
        }
    }
}

const DANGER_RED: Color32 = Color32::from_rgb(0xC4, 0x2B, 0x1C);

pub(crate) fn icon_button(
    ui: &mut Ui,
    m: &Metrics,
    pal: &Palette,
    icon: &str,
    tip: &str,
    enabled: bool,
    toggled: bool,
) -> egui::Response {
    Btn::new().icon(icon).tip(tip).enabled(enabled).toggled(toggled).show(ui, m, pal)
}

pub(crate) fn primary_button(ui: &mut Ui, m: &Metrics, pal: &Palette, text: &str) -> egui::Response {
    Btn::new().label(text).kind(BtnKind::Accent).show(ui, m, pal)
}

pub(crate) fn secondary_button(ui: &mut Ui, m: &Metrics, pal: &Palette, text: &str) -> egui::Response {
    Btn::new().label(text).kind(BtnKind::Standard).show(ui, m, pal)
}

pub(crate) fn danger_button(ui: &mut Ui, m: &Metrics, pal: &Palette, text: &str) -> egui::Response {
    Btn::new().label(text).kind(BtnKind::Danger).show(ui, m, pal)
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

    // paint_text elides labels wider than their rect, so only the width needs capping here.
    let width = match label {
        Some(l) => m.pad * 3.2 + icon_size + chevron + text_width(ui, l, m.font).min(max_label_w),
        None => m.pad * 2.4 + icon_size + chevron,
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(width, m.ctl_h), sense(enabled));

    // Show button as pressed while drop-down menu is opened.
    let open_id = resp.id.with("dd_open");
    let open = ui.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);

    if open {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(m.radius), pal.selected);
    } else if enabled {
        hover_fill(ui, rect, &resp, pal, m);
    }

    let color = text_color(pal, enabled);
    draw_icon(
        ui,
        pos2(rect.left() + m.pad + icon_size / 2.0, rect.center().y),
        icon_size,
        icon,
        color,
    );
    let chevron_pos = pos2(rect.right() - m.pad - chevron / 2.0, rect.center().y);
    draw_icon(
        ui,
        chevron_pos,
        chevron,
        "chevron_down",
        if enabled { pal.text_secondary } else { pal.text_disabled },
    );
    if let Some(l) = label {
        let tr = Rect::from_min_max(
            pos2(rect.left() + m.pad * 1.6 + icon_size, rect.top()),
            pos2(chevron_pos.x - chevron / 2.0 - m.pad * 0.6, rect.bottom()),
        );
        paint_text(ui, tr, l, FontId::proportional(m.font), color, false);
    }

    // Don't show the tooltip on top of the menu.
    let resp = if open { resp } else { resp.on_hover_text(tip) };
    if !enabled {
        return None;
    }

    let res = egui::Popup::menu(&resp)
        .width(rect.width().max(m.accessibility_scale * 180.0))
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
    let w = ui.available_width().max(m.accessibility_scale * 230.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, m.row_h * 0.95), sense(enabled));
    if enabled && resp.hovered() {
        ui.painter().rect_filled(
            rect.shrink2(vec2(0.0, 1.0)),
            CornerRadius::same(m.radius),
            pal.control_hover,
        );
    }
    let col = text_color(pal, enabled);
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
    let hint_size = m.font * 0.86;

    // Width the hint occupies, so the label can stop before it.
    let hint_width = hint.map_or(0.0, |h| text_width(ui, h, hint_size) + m.pad);

    let mut label_rect = text_rect;
    label_rect.max.x = (text_rect.right() - hint_width).max(text_rect.left() + 1.0);

    let truncated = paint_text(ui, label_rect, label, FontId::proportional(m.font), col, false);

    if let Some(h) = hint {
        paint_text(ui, text_rect, h, FontId::proportional(hint_size), pal.text_secondary, true);
    }
    if truncated {
        resp.on_hover_text(label).clicked()
    } else {
        resp.clicked()
    }
}

/// Gap between a popup menu and the window edge.
const MENU_PADDING: i8 = 6;

/// Contents of a context menu that can never run off the window: when the menu is taller than the
/// window it scrolls instead of being cut off (the popup itself already shifts to stay on screen).
pub(crate) fn menu_scroll(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    // Everything the popup frame adds around the content:
    // inner margin + stroke + the outer margin that provides the gap.
    let style = ui.style();
    let frame_overhead = style.spacing.menu_margin.sum().y
        + 2.0 * style.visuals.window_stroke.width
        + 2.0 * f32::from(MENU_PADDING);

    let max_h = ui.ctx().content_rect().height() - frame_overhead;

    ui.set_max_height(max_h);
    egui::ScrollArea::vertical()
        .max_height(max_h)
        .max_width(100.0)
        .auto_shrink([true, true])
        .show(ui, add);
}

/// `resp.context_menu(..)` with the scrolling behavior of `menu_scroll`.
pub(crate) fn scroll_context_menu(resp: &egui::Response, add: impl FnOnce(&mut Ui)) {
    let frame = egui::Frame::popup(&resp.ctx.global_style())
        .outer_margin(Margin::symmetric(0, MENU_PADDING));

    egui::Popup::context_menu(resp)
        .frame(frame)
        .show(|ui| menu_scroll(ui, add));
}

/// The one surface colour of a dialog. Opaque: a see-through dialog would show the file list's text through it
/// (a real backdrop blur is not available for an in-window popup).
fn dialog_surface(pal: &Palette) -> Color32 {
    if pal.dark { Color32::from_rgb(43, 43, 43) } else { Color32::from_rgb(249, 249, 249) }
}

/// Fluent-style dialog: rounded flyout surface, optional dimmed scrim. Returns true when closed (X / Esc /
/// a click outside of it).
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
    dialog_shell_with_footer(ui, m, pal, id, title, modal, width, add, None)
}

/// `dialog_shell` with a command row along the bottom edge, like a Windows content dialog.
pub(crate) fn dialog_shell_with_footer<'a>(
    ui: &mut Ui,
    m: &Metrics,
    pal: &Palette,
    id: &str,
    title: &str,
    modal: bool,
    width: f32,
    add: impl FnOnce(&mut Ui),
    footer: Option<Box<dyn FnOnce(&mut Ui) + 'a>>,
) -> bool {
    // A click outside the dialog closes it. Not on the frame it first appears in: that frame may still
    // carry the click that opened it.
    let frame_nr = ui.ctx().cumulative_pass_nr();
    let seen_id = Id::unique((id, "last_frame"));
    let first_frame = ui.data(|d| d.get_temp::<u64>(seen_id)).is_none_or(|last| last + 1 < frame_nr);
    ui.data_mut(|d| d.insert_temp(seen_id, frame_nr));
    let mut clicked_outside = false;
    if modal {
        let screen = ui.content_rect();
        egui::Area::new(Id::unique((id, "scrim")))
            .order(egui::Order::Middle)
            .fixed_pos(screen.min)
            .show(ui, |ui| {
                let (rect, resp) = ui.allocate_exact_size(screen.size(), Sense::click_and_drag());
                clicked_outside = resp.clicked();
                ui.painter().rect_filled(
                    rect,
                    CornerRadius::ZERO,
                    Color32::from_black_alpha(if pal.dark { 110 } else { 55 }),
                );
            });
    }
    let radius = (m.radius as f32 * 1.6).round() as u8;
    let mut closed = false;
    let window = egui::Window::new(title)
        .id(Id::unique(id))
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .order(egui::Order::Foreground)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .frame(
            egui::Frame::window(&ui.global_style())
                .fill(dialog_surface(pal))
                .corner_radius(CornerRadius::same(radius))
                .shadow(popup_shadow(pal.dark))
                .stroke(Stroke::new(1.0_f32, pal.divider))
                .inner_margin(Margin::ZERO),
        )
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = m.pad * 0.6;
            let side = (m.pad * 2.5) as i8;
            egui::Frame::new()
                .inner_margin(Margin { left: side, right: side, top: (m.pad * 2.0) as i8, bottom: side })
                .show(ui, |ui| {
                    ui.set_width(width);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(title).size(m.font * 1.4).color(pal.text));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            closed |= icon_button(ui, m, pal, "close", "Close", true, false).clicked();
                        });
                    });
                    ui.add_space(m.pad * 0.8);
                    add(ui);
                });
            if let Some(footer) = footer {
                // Same surface as the body, set apart by a hairline.
                ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), Stroke::new(1.0_f32, pal.divider));
                egui::Frame::new()
                    .inner_margin(Margin::symmetric(side, (m.pad * 1.3) as i8))
                    .show(ui, |ui| {
                        // A fixed height: a bare right-to-left layout would take all the height there is.
                        ui.allocate_ui_with_layout(
                            vec2(width, m.ctl_h),
                            Layout::right_to_left(Align::Center),
                            footer,
                        );
                    });
            }
        });
    if !modal {
        // Without a scrim: a click on the main window (not on a menu opened from the dialog).
        if let (Some(w), Some(pos)) = (&window, ui.input(|i| i.pointer.interact_pos())) {
            let on_page = ui.ctx().layer_id_at(pos).is_some_and(|l| l.order <= egui::Order::Middle);
            clicked_outside = ui.input(|i| i.pointer.any_click()) && on_page && !w.response.rect.contains(pos);
        }
    }
    closed |= clicked_outside && !first_frame;
    closed || (ui.input(|i| i.key_pressed(egui::Key::Escape)) && !ui.egui_wants_keyboard_input())
}

pub(crate) fn theme_pref(t: ThemeChoice) -> egui::ThemePreference {
    match t {
        ThemeChoice::System => egui::ThemePreference::System,
        ThemeChoice::Light => egui::ThemePreference::Light,
        ThemeChoice::Dark => egui::ThemePreference::Dark,
    }
}

/// The busy indicator as a widget: reserves a `diameter` square in the layout and paints the spinner in `color`.
pub(crate) fn spinner(ui: &mut Ui, diameter: f32, color: Color32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::Vec2::splat(diameter), Sense::hover());
    paint_spinner(ui, rect.center(), diameter, color);
    response
}

/// Busy spinner for a right-aligned size cell, directly left of the text, never on top of it.
/// Nothing is drawn when the cell has no room for both.
pub(crate) fn size_cell_spinner(ui: &Ui, m: &Metrics, pal: &Palette, cell: Rect, text: &str) {
    let diameter = m.font * 0.85;
    let left = cell.right() - text_width(ui, text, m.font) - m.pad * 0.5 - diameter;
    if left >= cell.left() {
        paint_spinner(ui, pos2(left + diameter * 0.5, cell.center().y), diameter, pal.accent);
    }
}

/// Stepped busy indicator: 8 ticks with a fading tail, advancing 10x per second.
/// Unlike `egui::Spinner` it only asks for a repaint when the next step is due.
pub(crate) fn paint_spinner(ui: &Ui, center: egui::Pos2, diameter: f32, color: Color32) {
    use std::f32::consts::{FRAC_PI_2, TAU};
    const TICKS: usize = 8;
    const STEP_MS: u64 = 100;

    let now_ms = (ui.input(|i| i.time) * 1000.0) as u64;
    let head = (now_ms / STEP_MS) as usize % TICKS;
    let (r_in, r_out) = (diameter * 0.25, diameter * 0.5);
    let width = (diameter * 0.12).max(1.0);

    for k in 0..TICKS {
        let behind = (head + TICKS - k) % TICKS; // 0 = brightest tick
        let a = 0.15 + 0.85 * (1.0 - behind as f32 / TICKS as f32);
        let ang = k as f32 / TICKS as f32 * TAU - FRAC_PI_2;
        let dir = vec2(ang.cos(), ang.sin());
        ui.painter().line_segment(
            [center + dir * r_in, center + dir * r_out],
            Stroke::new(width, color.gamma_multiply(a)),
        );
    }
    // Wake up exactly when the next step is due, not every frame.
    ui.ctx().request_repaint_after(Duration::from_millis(STEP_MS - now_ms % STEP_MS));
}
