//! Settings page.

use crate::prelude::*;

/// Working copy of the settings the Settings tab edits during one frame, plus the buttons that
/// were pressed. `Explorer::finish_settings_page` applies the result.
pub(crate) struct SettingsPage {
    pub(crate) old: Settings,
    pub(crate) s: Settings,
    pub(crate) font: f32,
    pub(crate) font_edit: Option<f32>,
    pub(crate) cache_len: usize,
    pub(crate) rescan: bool,
    pub(crate) choose: bool,
    pub(crate) delete_index: bool,
    pub(crate) reset_quick: bool,
    pub(crate) default_on: bool,
    pub(crate) default_error: Option<String>,
    /// Some(true): make FileFly the default file manager; Some(false): back to Windows Explorer.
    pub(crate) default_request: Option<bool>,
    /// The "View licenses" button was pressed: open the licensing popup.
    pub(crate) show_licenses: bool,
    /// Current state of the update check.
    pub(crate) update: UpdateState,
    pub(crate) check_update: bool,
    pub(crate) download_update: bool,
}

impl SettingsPage {
    pub(crate) fn new(
        settings: &Settings,
        font_edit: Option<f32>,
        cache_len: usize,
        default_on: bool,
        default_error: Option<String>,
        update: UpdateState,
    ) -> Self {
        Self {
            old: settings.clone(),
            s: settings.clone(),
            font: font_edit.unwrap_or(settings.font_size),
            font_edit,
            cache_len,
            rescan: false,
            choose: false,
            delete_index: false,
            reset_quick: false,
            default_on,
            default_error,
            default_request: None,
            show_licenses: false,
            update,
            check_update: false,
            download_update: false,
        }
    }
}

pub(crate) fn fl_card_fill(pal: &Palette) -> Color32 {
    if pal.dark {
        Color32::from_white_alpha(13)
    } else {
        Color32::from_white_alpha(185)
    }
}

pub(crate) fn fl_section(ui: &mut Ui, m: &Metrics, pal: &Palette, text: &str) {
    ui.add_space(m.pad * 1.4);
    ui.label(RichText::new(text).size(m.font * 1.05).strong().color(pal.text));
    ui.add_space(m.pad * 0.5);
}

/// Rounded surface with a hairline border that groups related rows.
pub(crate) fn fl_card(ui: &mut Ui, m: &Metrics, pal: &Palette, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(fl_card_fill(pal))
        .stroke(Stroke::new(1.0_f32, pal.divider))
        .corner_radius(CornerRadius::same(m.radius + 2))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            add(ui);
        });
}

pub(crate) fn fl_divider(ui: &mut Ui, m: &Metrics, pal: &Palette) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter()
        .rect_filled(r.shrink2(vec2(m.pad * 0.8, 0.0)), CornerRadius::ZERO, pal.divider);
}

/// One setting: title and optional description on the left, `control` right-aligned beside them.
/// The text takes the width the control leaves free; when that would drop below a minimum (narrow
/// window) the control moves below the text instead, so nothing overlaps or gets squeezed.
pub(crate) fn fl_row(
    ui: &mut Ui,
    metrics: &Metrics,
    palette: &Palette,
    title: &str,
    description: Option<&str>,
    add_control: impl FnOnce(&mut Ui),
) {
    let scale = metrics.accessibility_scale;
    let row_width = ui.available_width();
    let padding_x = metrics.pad * 1.6;
    let padding_y = metrics.pad * 0.9;
    let title_description_gap = scale * 2.0;
    let min_text_width = scale * 180.0;
    let min_height = metrics.row_h * 1.55;

    // The control's natural width is only known after drawing it, so the layout decision uses the
    // width remembered from the previous frame (a changed width repaints right away).
    let width_id = ui.scope_id().with(("fl_row_control_width", title));
    let known_width = ui.data(|d| d.get_temp::<f32>(width_id)).unwrap_or(0.0);
    let stacked = row_width - padding_x * 3.0 - known_width < min_text_width;
    let text_width = if stacked {
        row_width - padding_x * 2.0
    } else {
        row_width - padding_x * 3.0 - known_width
    };

    let title_galley = ui.painter().layout(
        title.to_owned(),
        FontId::proportional(metrics.font),
        palette.text,
        text_width,
    );
    let description_galley = description.map(|text| {
        ui.painter().layout(
            text.to_owned(),
            FontId::proportional(metrics.font * 0.88),
            palette.text_secondary,
            text_width,
        )
    });
    let title_height = title_galley.size().y;
    let text_block_height = match &description_galley {
        Some(desc) => title_height + title_description_gap + desc.size().y,
        None => title_height,
    };

    let stack_gap = metrics.pad * 0.6;
    let row_height = if stacked {
        text_block_height + stack_gap + metrics.ctl_h + padding_y * 2.0
    } else {
        (text_block_height + padding_y * 2.0).max(min_height)
    };
    let (row_rect, _response) = ui.allocate_exact_size(vec2(row_width, row_height), Sense::hover());

    let text_top = if stacked {
        row_rect.top() + padding_y
    } else {
        row_rect.center().y - text_block_height / 2.0
    };
    let text_left = row_rect.left() + padding_x;
    ui.painter().galley(pos2(text_left, text_top), title_galley, palette.text);
    if let Some(desc) = description_galley {
        let desc_top = text_top + title_height + title_description_gap;
        ui.painter().galley(pos2(text_left, desc_top), desc, palette.text_secondary);
    }

    let control_rect = Rect::from_min_max(
        pos2(
            text_left,
            if stacked { row_rect.bottom() - padding_y - metrics.ctl_h } else { row_rect.top() },
        ),
        pos2(row_rect.right() - padding_x, if stacked { row_rect.bottom() - padding_y } else { row_rect.bottom() }),
    );
    // new_child does NOT advance the parent's cursor.
    let mut control_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(control_rect)
            .layout(Layout::right_to_left(Align::Center)),
    );
    add_control(&mut control_ui);
    let control_width = control_ui.min_rect().width();
    if (control_width - known_width).abs() > 0.5 {
        ui.data_mut(|d| d.insert_temp(width_id, control_width));
        ui.ctx().request_repaint();
    }
}

/// Fluent toggle switch with its "On" / "Off" label to the left.
pub(crate) fn fl_switch(ui: &mut Ui, m: &Metrics, pal: &Palette, on: &mut bool) {
    let size = vec2(m.font * 2.7, m.font * 1.35);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_with_time(resp.id, *on, 0.12);
    let radius = CornerRadius::same((size.y / 2.0).round() as u8);
    if *on {
        ui.painter().rect_filled(rect, radius, pal.accent);
    } else {
        let fill = if resp.hovered() { pal.subtle_hover } else { Color32::TRANSPARENT };
        ui.painter().rect_filled(rect, radius, fill);
        ui.painter().rect_stroke(
            rect,
            radius,
            Stroke::new(1.0_f32, pal.text_secondary),
            StrokeKind::Inside,
        );
    }
    let knob_r = egui::lerp((size.y * 0.2)..=(size.y * 0.27), t);
    let x = egui::lerp((rect.left() + size.y * 0.5)..=(rect.right() - size.y * 0.5), t);
    let knob = if *on { pal.on_accent } else { pal.text_secondary };
    ui.painter().circle_filled(pos2(x, rect.center().y), knob_r, knob);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    ui.label(RichText::new(if *on { "On" } else { "Off" }).color(pal.text_secondary));
}

/// Fluent slider: thin rounded track with an accent-filled part, a ring-and-dot thumb that grows on
/// hover and shrinks while pressed, and the current value (plus `suffix`) to its right.
/// `log` spreads the range logarithmically, `step` > 0 snaps the value. Check the returned response
/// for `changed()`, `dragged()` and `drag_stopped()` as with any egui widget.
/// Every slider reserves the same value and reset-button columns (the reset button only shows when
/// `value != default`), so the tracks of stacked rows line up and the track takes the leftover width.
pub(crate) fn fl_slider<T: egui::emath::Numeric>(
    ui: &mut Ui,
    m: &Metrics,
    pal: &Palette,
    value: &mut T,
    default: T,
    range: std::ops::RangeInclusive<T>,
    log: bool,
    step: f64,
    suffix: &str,
) -> egui::Response {
    let (lo, hi) = (range.start().to_f64(), range.end().to_f64());
    let to_t = |v: f64| {
        let t = if log { (v.max(lo).ln() - lo.ln()) / (hi.ln() - lo.ln()) } else { (v - lo) / (hi - lo) };
        t.clamp(0.0, 1.0)
    };
    // Snaps to the step / integer grid and into the range (also used for typed values).
    let snap = |mut v: f64| {
        if step > 0.0 {
            v = lo + ((v - lo) / step).round() * step;
        } else if T::INTEGRAL {
            v = v.round();
        }
        v.clamp(lo, hi)
    };
    let from_t = |t: f64| snap(if log { (lo.ln() + t * (hi.ln() - lo.ln())).exp() } else { lo + t * (hi - lo) });
    let plain = |v: f64| if T::INTEGRAL || step >= 1.0 { format!("{v:.0}") } else { format!("{v:.1}") };

    let enabled = ui.is_enabled();
    let thumb_r = m.font * 0.7;
    let label_w = m.font * 6.5;
    // One rect: [reset slot | track | value field], children placed by explicit coordinates.
    // Preferred width is fixed so rows can tell how much room the control needs; the track only
    // shrinks when even a stacked row is too narrow.
    let field_h = m.ctl_h * 0.9;
    let slot_w = m.ctl_h + m.pad * 0.5;
    let track_w = (m.accessibility_scale * 200.0)
        .min(ui.available_width() - slot_w - label_w - m.pad)
        .max(m.accessibility_scale * 60.0);
    let (rect, container) = ui.allocate_exact_size(
        vec2(slot_w + track_w + m.pad + label_w, (thumb_r * 2.0).max(field_h)),
        Sense::hover(),
    );
    let track_left = rect.left() + slot_w;
    // Only the track itself reacts to the pointer; the value field is a text edit.
    let track_hit = Rect::from_min_size(
        pos2(track_left, rect.center().y - thumb_r),
        vec2(track_w, thumb_r * 2.0),
    );
    let mut resp = ui.interact(
        track_hit,
        container.id.with("track"),
        if enabled { Sense::click_and_drag() } else { Sense::hover() },
    );
    let (x0, x1) = (track_left + thumb_r, track_left + track_w - thumb_r);

    let before = value.to_f64();
    let mut new = before;
    if enabled {
        if let Some(p) = resp.interact_pointer_pos().filter(|_| resp.is_pointer_button_down_on() || resp.clicked()) {
            new = from_t(((p.x - x0) / (x1 - x0)) as f64);
        }
        if resp.has_focus() {
            let nudge = if step > 0.0 { step } else if T::INTEGRAL { 1.0 } else { (hi - lo) / 100.0 };
            let dir = ui.input(|i| {
                (i.key_pressed(egui::Key::ArrowRight) || i.key_pressed(egui::Key::ArrowUp)) as i8
                    - (i.key_pressed(egui::Key::ArrowLeft) || i.key_pressed(egui::Key::ArrowDown)) as i8
            });
            if dir != 0 {
                new = (before + dir as f64 * nudge).clamp(lo, hi);
            }
        }
        if new != before {
            *value = T::from_f64(new);
            resp.mark_changed();
        }
    }

    // Track, then the filled part up to the thumb.
    let cx = egui::lerp(x0..=x1, to_t(value.to_f64()) as f32);
    let track = Rect::from_min_max(
        pos2(x0, rect.center().y - m.accessibility_scale * 2.0),
        pos2(x1, rect.center().y + m.accessibility_scale * 2.0),
    );
    let round = CornerRadius::same(255);
    let (rail, fill) = if enabled {
        (pal.text_secondary.gamma_multiply(0.5), pal.accent)
    } else {
        (pal.text_disabled, pal.text_disabled)
    };
    ui.painter().rect_filled(track, round, rail);
    ui.painter().rect_filled(Rect::from_min_max(track.min, pos2(cx, track.max.y)), round, fill);

    // Thumb: neutral ring with an accent dot.
    let center = pos2(cx, rect.center().y);
    let dot_target = if !enabled { 0.5 } else if resp.is_pointer_button_down_on() { 0.5 } else if resp.hovered() { 0.7 } else { 0.6 };
    let dot = ui.ctx().animate_value_with_time(resp.id, dot_target, 0.1);
    ui.painter().circle(center, thumb_r, pal.flyout, Stroke::new(1.0_f32, pal.control_stroke));
    ui.painter().circle_filled(center, thumb_r * dot, fill);
    if enabled && resp.hovered() {
        ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
    }

    // Value field: shows the number (unit beside it); typing and pressing Enter applies the value,
    // clamped into the range.
    let field = Rect::from_min_max(
        pos2(rect.right() - label_w, rect.center().y - field_h / 2.0),
        pos2(rect.right(), rect.center().y + field_h / 2.0),
    );
    let edit_id = container.id.with("edit");
    let buf_id = container.id.with("buf");
    let focused = ui.ctx().memory(|mem| mem.has_focus(edit_id));
    let radius = CornerRadius::same(m.radius);
    if enabled {
        ui.painter().rect_filled(field, radius, pal.control);
        ui.painter().rect_stroke(field, radius, Stroke::new(1.0_f32, pal.control_stroke), StrokeKind::Inside);
        if focused {
            let underline = Rect::from_min_max(pos2(field.left() + 2.0, field.bottom() - 2.0), pos2(field.right() - 2.0, field.bottom()));
            ui.painter().rect_filled(underline, CornerRadius::same(1), pal.accent);
        }
    }
    let text_col = if enabled { pal.text } else { pal.text_disabled };
    let inset = m.pad * 0.5;
    let unit = suffix.trim();
    let unit_w = if unit.is_empty() { 0.0 } else { text_width(ui, unit, m.font) + inset };
    if !unit.is_empty() {
        let unit_rect = Rect::from_min_max(field.min, pos2(field.right() - inset, field.bottom()));
        paint_text(ui, unit_rect, unit, FontId::proportional(m.font), if enabled { pal.text_secondary } else { pal.text_disabled }, true);
    }
    let edit_rect = Rect::from_min_max(
        pos2(field.left() + inset, field.top()),
        pos2(field.right() - inset - unit_w, field.bottom()),
    );
    let mut text: String =
        if focused { ui.data(|d| d.get_temp(buf_id)).unwrap_or_default() } else { plain(value.to_f64()) };
    // A right-to-left layout centres the edit vertically in the field.
    let edit = ui
        .scope_builder(
            egui::UiBuilder::new().max_rect(edit_rect).layout(Layout::right_to_left(Align::Center)),
            |ui| {
                // A text edit is at least `interact_size.y` tall; the app's is taller than the field,
                // which pushed the text down.
                ui.spacing_mut().interact_size.y = 0.0;
                ui.add(
                    egui::TextEdit::singleline(&mut text)
                        .id(edit_id)
                        .font(FontId::proportional(m.font))
                        .text_color(text_col)
                        .align(Align2::RIGHT_CENTER)
                        .frame(egui::Frame::NONE)
                        .margin(Margin::ZERO)
                        .desired_width(edit_rect.width()),
                )
            },
        )
        .inner;
    if edit.gained_focus() {
        // Select everything so typing replaces the number.
        if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), edit_id) {
            let end = egui::text::CCursor::new(text.chars().count());
            state.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), end)));
            state.store(ui.ctx(), edit_id);
        }
    }
    if edit.has_focus() || edit.gained_focus() {
        ui.data_mut(|d| d.insert_temp(buf_id, text.clone()));
    }
    if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        let typed = text.trim().trim_end_matches(unit).trim();
        if let Some(v) = typed.parse::<f64>().ok().filter(|v| v.is_finite()) {
            let v = snap(v);
            if v != value.to_f64() {
                *value = T::from_f64(v);
                resp.mark_changed();
            }
        }
    }

    // Reset button in its own slot at the left; the slot stays empty while at the default.
    if value.to_f64() != default.to_f64() {
        let slot = Rect::from_min_size(rect.min, vec2(m.ctl_h, rect.height()));
        let reset = ui
            .scope_builder(
                egui::UiBuilder::new().max_rect(slot).layout(Layout::top_down(Align::Center)),
                |ui| ui.centered_and_justified(|ui| icon_button(ui, m, pal, "refresh", "Reset to default", true, false)).inner,
            )
            .inner;
        if reset.clicked() {
            *value = default;
            resp.mark_changed();
        }
    }
    resp
}

pub(crate) fn settings_page(ui: &mut Ui, rect: Rect, m: &Metrics, pal: &Palette, tab_id: u64, sp: &mut SettingsPage) {
    // The footer's space is always reserved, so the button appearing never shifts the page.
    let footer_h = m.ctl_h + m.pad * 2.0;
    let scroll_rect = Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - footer_h));
    let avail = rect.width();
    let w = (avail - m.pad * 4.0).clamp(m.accessibility_scale * 240.0, m.accessibility_scale * 860.0);
    let left = ((avail - w) / 2.0).max(m.pad * 2.0);
    ui.scope_builder(
        egui::UiBuilder::new()
            .id_salt(("settings_page", tab_id))
            .max_rect(scroll_rect),
        |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        ui.add_space(left);
                        ui.vertical(|ui| {
                            ui.set_width(w);
                            settings_content(ui, m, pal, sp);
                        });
                    });
                });
        },
    );
    if !sp.s.page_fields_eq(&Settings::default()) {
        let footer = Rect::from_min_max(pos2(rect.left() + left, rect.bottom() - footer_h), pos2(rect.left() + left + w, rect.bottom()));
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(footer).layout(Layout::left_to_right(Align::Center)),
            |ui| {
                if secondary_button(ui, m, pal, "Restore all defaults").clicked() {
                    sp.s.copy_page_fields_from(&Settings::default());
                    sp.font = sp.s.font_size;
                    sp.font_edit = None;
                }
            },
        );
    }
}

pub(crate) fn settings_content(ui: &mut Ui, m: &Metrics, pal: &Palette, sp: &mut SettingsPage) {
    ui.spacing_mut().slider_width = m.accessibility_scale * 150.0;
    ui.add_space(m.pad * 1.5);
    ui.label(RichText::new("Settings").size(m.font * 2.0).strong().color(pal.text));
    ui.label(
        RichText::new(format!("FileFly v{}", env!("CARGO_PKG_VERSION"))).color(pal.text_secondary),
    );

    // ---------------------------------------------------------------- Appearance
    fl_section(ui, m, pal, "Appearance");
    fl_card(ui, m, pal, |ui| {
        fl_row(ui, m, pal, "App theme", Some("Choose how FileFly looks"), |ui| {
            let label = |t: ThemeChoice| match t {
                ThemeChoice::System => "Use system setting",
                ThemeChoice::Light => "Light",
                ThemeChoice::Dark => "Dark",
            };
            egui::ComboBox::from_id_salt("settings_theme")
                .width(m.accessibility_scale * 150.0)
                .selected_text(label(sp.s.theme))
                .show_ui(ui, |ui| {
                    for t in [ThemeChoice::System, ThemeChoice::Light, ThemeChoice::Dark] {
                        ui.selectable_value(&mut sp.s.theme, t, label(t));
                    }
                });
        });

        // Text scaling settings.
        fl_divider(ui, m, pal);
        fl_row(ui, m, pal, "Text size", Some("Scales text and interface elements"), |ui| {
            // The whole UI is sized from this value, so applying it mid-drag would move the slider
            // under the cursor. Preview in the handle, apply on release.
            ui.add_enabled_ui(!sp.s.follow_text_scale, |ui| {
                let resp = fl_slider(ui, m, pal, &mut sp.font, 14.0, 10.0..=24.0, false, 1.0, " pt");
                if resp.dragged() {
                    sp.font_edit = Some(sp.font);
                } else if resp.drag_stopped() || resp.changed() {
                    sp.s.font_size = sp.font;
                    sp.font_edit = None;
                }
            });
        });
        if cfg!(windows) {
            fl_divider(ui, m, pal);
            fl_row(
                ui,
                m,
                pal,
                "Follow system text size",
                Some("Also applies the “Text size” scaling setting from Windows Accessibility"),
                |ui| fl_switch(ui, m, pal, &mut sp.s.follow_text_scale),
            );
            fl_divider(ui, m, pal);
            fl_row(
                ui,
                m,
                pal,
                "Use Windows accent color",
                Some("Turn off to pick your own accent color"),
                |ui| fl_switch(ui, m, pal, &mut sp.s.use_system_accent),
            );
            if !sp.s.use_system_accent {
                fl_divider(ui, m, pal);
                fl_row(ui, m, pal, "Accent color", None, |ui| {
                    ui.color_edit_button_srgb(&mut sp.s.custom_accent);
                });
            }

            fl_divider(ui, m, pal);
            fl_row(ui, m, pal, "Transparency effects", Some("Choose from available transparency effects"), |ui| {
                egui::ComboBox::from_id_salt("transparency")
                    .width(m.accessibility_scale * 150.0)
                    .selected_text(sp.s.transparency_effect.label())
                    .show_ui(ui, |ui| {
                        for &t in TransparencyEffect::available() {
                            ui.selectable_value(&mut sp.s.transparency_effect, t, t.label());
                        }
                    });
            });
        }else {
            fl_divider(ui, m, pal);
            fl_row(ui, m, pal, "Accent color", None, |ui| {
                ui.color_edit_button_srgb(&mut sp.s.custom_accent);
            });
        }
        fl_divider(ui, m, pal);
        fl_row(
            ui,
            m,
            pal,
            "Drag space in tab bar",
            Some("Leave an empty area at the left of the tabs for moving the window"),
            |ui| fl_switch(ui, m, pal, &mut sp.s.left_drag_space),
        );
        fl_divider(ui, m, pal);
        fl_row(
            ui,
            m,
            pal,
            "Custom window frame",
            Some("Use the custom integrated window control title bar of the App. Changing this setting will restart the app."),
            |ui| fl_switch(ui, m, pal, &mut sp.s.custom_frame),
        );
        fl_divider(ui, m, pal);
        fl_row(
            ui,
            m,
            pal,
            "Paning blur effects",
            Some("Blurs the backdrop behind the paning overlay when dragging tabs.  Changing this setting will restart the app."),
            |ui| fl_switch(ui, m, pal, &mut sp.s.blur_paning),
        );
        fl_divider(ui, m, pal);
        fl_row(
            ui,
            m,
            pal,
            "Compact mode",
            Some("Reduces margins app-wide."),
            |ui| fl_switch(ui, m, pal, &mut sp.s.compact),
        );
    });

    // ---------------------------------------------------------------- Files
    fl_section(ui, m, pal, "Files & folders");
    fl_card(ui, m, pal, |ui| {
        fl_row(
            ui,
            m,
            pal,
            "Show hidden items",
            Some("List files and folders that are marked as hidden"),
            |ui| fl_switch(ui, m, pal, &mut sp.s.show_hidden),
        );
        fl_divider(ui, m, pal);
        fl_row(ui, m, pal, "Date format", Some("How modification dates are written"), |ui| {
            egui::ComboBox::from_id_salt("settings_date_format")
                .width(m.accessibility_scale * 150.0)
                .selected_text(sp.s.date_format.label())
                .show_ui(ui, |ui| {
                    for f in [DateFormat::European, DateFormat::Iso, DateFormat::American] {
                        ui.selectable_value(&mut sp.s.date_format, f, f.label());
                    }
                });
        });
    });

    // ---------------------------------------------------------------- Drives and Quick access
    fl_section(ui, m, pal, "Drives & Quick access");
    fl_card(ui, m, pal, |ui| {
        fl_row(
            ui,
            m,
            pal,
            "Network drives",
            Some("Show mapped network drives and network mounts in the sidebar. Turn off if an unreachable share slows things down."),
            |ui| fl_switch(ui, m, pal, &mut sp.s.network_drives),
        );
        fl_divider(ui, m, pal);
        fl_row(
            ui,
            m,
            pal,
            "Quick access",
            Some("Pin folders with the + button in the sidebar or a folder's right-click menu; unpin them from their own right-click menu."),
            |ui| {
                if secondary_button(ui, m, pal, "Restore defaults").clicked() {
                    sp.reset_quick = true;
                }
            },
        );
    });
    // ---------------------------------------------------------------- Default file manager
    if cfg!(windows) {
        fl_section(ui, m, pal, "Default file manager (experimental)");
        fl_card(ui, m, pal, |ui| {
            let desc = match &sp.default_error {
                Some(e) => format!("Could not change the setting: {e}"),
                None if sp.default_on => "FileFly opens folders from other apps, the Run dialog and “Show in folder”. Turn off to return to Windows Explorer. Warning: This also overrides shortcuts in your taskbar and Windows menu.".to_owned(),
                None => "Open folders from other apps, the Run dialog and “Show in folder” in FileFly instead of Windows Explorer. Applies to your account only.".to_owned(),
            };
            fl_row(ui, m, pal, "Use FileFly as default file manager", Some(desc.as_str()), |ui| {
                let mut on = sp.default_on;
                fl_switch(ui, m, pal, &mut on);
                if on != sp.default_on {
                    sp.default_request = Some(on);
                }
            });
        });
    }
    // ---------------------------------------------------------------- Search
    fl_section(ui, m, pal, "Search (experimental)");
    fl_card(ui, m, pal, |ui| {
        fl_row(
            ui,
            m,
            pal,
            "Maximum search results",
            Some("Default: 100,000"),
            |ui| {
                fl_slider(ui, m, pal, &mut sp.s.max_search_results, 100_000, 1..=10_000_000, true, 0.0, "");
            },
        );
        fl_divider(ui, m, pal);
        fl_row(ui, m, pal, "Search threads", Some("Default: 4"), |ui| {
            fl_slider(ui, m, pal, &mut sp.s.search_threads, 4, 1..=64, false, 0.0, "");
        });
    });

    // ---------------------------------------------------------------- Folder sizes
    fl_section(ui, m, pal, "Folder sizes");
    fl_card(ui, m, pal, |ui| {
        fl_row(
            ui,
            m,
            pal,
            "Calculate folder sizes",
            Some("Shows folder sizes in the size row. Activating this setting will increase CPU load."),
            |ui| fl_switch(ui, m, pal, &mut sp.s.calculate_folder_sizes),
        );
        // The rows below only do something while the indexer is turned on.
        let sizes_on = sp.s.calculate_folder_sizes;
        ui.add_enabled_ui(sizes_on, |ui| {
            fl_divider(ui, m, pal);
            let cached = format!(
                "{} folders cached. Changes are picked up automatically while the app is open.",
                sp.cache_len
            );
            fl_row(ui, m, pal, "Rescan all folders", Some(cached.as_str()), |ui| {
                if secondary_button(ui, m, pal, "Rescan all").clicked() {
                    sp.rescan = true;
                }
            });
            fl_divider(ui, m, pal);
            fl_row(
                ui,
                m,
                pal,
                "Delete index",
                Some("Removes all saved folder sizes from memory and disk; the folders on screen are measured again."),
                |ui| {
                    if danger_button(ui, m, pal, "Delete index").clicked() {
                        sp.delete_index = true;
                    }
                },
            );
        });
    });

    // ---------------------------------------------------------------- Extras
    fl_section(ui, m, pal, "Miscellaneous");
    fl_card(ui, m, pal, |ui| {
        fl_row(
            ui,
            m,
            pal,
            "Git integration",
            Some("Shows the current branch and allows you to checkout local branches within git repositories. Changing this setting will restart the app."),
            |ui| fl_switch(ui, m, pal, &mut sp.s.git_activated),
        );
        fl_divider(ui, m, pal);
        fl_row(
            ui,
            m,
            pal,
            "Restore tabs on launch",
            Some("Reopens the tabs and split layout of your previous session when the app starts."),
            |ui| fl_switch(ui, m, pal, &mut sp.s.restore_tabs),
        );
        fl_divider(ui, m, pal);
        fl_row(
            ui,
            m,
            pal,
            "Show \"What's new\" button",
            Some("Shows a button next to Settings that lists what changed in this version. A dot marks it until you have read the notes. Reading them loads them from GitHub."),
            |ui| fl_switch(ui, m, pal, &mut sp.s.show_whats_new),
        );
        // Do not show the updates section on msix versions of the app.
        #[cfg(feature = "github-updater")]
        {
            fl_divider(ui, m, pal);
            fl_row(
                ui,
                m,
                pal,
                "Check for updates on launch",
                Some("If active, the app will contact GitHub on launch to check for the newest release. If an update was found, it is shown as a notification."),
                |ui| fl_switch(ui, m, pal, &mut sp.s.startup_update_check),
            );
            fl_divider(ui, m, pal);
            let (hint, label) = match &sp.update {
                UpdateState::Idle => (
                    "Checks the github releases for whether the latest version is installed.".to_owned(),
                    "Check",
                ),
                UpdateState::Checking => ("Checking…".to_owned(), "Check"),
                UpdateState::UpToDate => ("You are up-to-date.".to_owned(), "Check"),
                UpdateState::Available(_) => (
                    "You can update to a newer version, click the Download button to start.".to_owned(),
                    "Download",
                ),
                UpdateState::Failed(e) => (format!("Couldn't check for updates: {e}."), "Check"),
            };
            let available = matches!(sp.update, UpdateState::Available(_));
            let busy = matches!(sp.update, UpdateState::Checking);

            fl_row(ui, m, pal, "Check for updates", Some(&hint), |ui| {
                let clicked = ui
                    .add_enabled_ui(!busy, |ui| {
                        if available {
                            primary_button(ui, m, pal, label).clicked()
                        } else {
                            secondary_button(ui, m, pal, label).clicked()
                        }
                    })
                    .inner;
                if clicked {
                    if available {
                        sp.download_update = true;
                    } else {
                        sp.check_update = true;
                    }
                }
            });
        }
    });

    // ---------------------------------------------------------------- About
    fl_section(ui, m, pal, "Licenses");
    fl_card(ui, m, pal, |ui| {
        fl_row(
            ui,
            m,
            pal,
            "Open-source licenses",
            Some("FileFly is built with open-source libraries. Their copyright notices and license terms are listed here."),
            |ui| {
                if secondary_button(ui, m, pal, "View licenses").clicked() {
                    sp.show_licenses = true;
                }
            },
        );
    });

    ui.add_space(m.pad * 3.0);
}

impl Explorer {
    /// Opens the Settings tab (or focuses it if it is already open; pressing the button again closes it).
    pub(crate) fn open_settings(&mut self) {
        self.default_on = default_app::is_default();
        self.default_error = None;
        if let Some(i) = self.tabs.iter().position(|t| t.page == Some(Page::Settings)) {
            if i == self.active && self.tabs.len() > 1 {
                self.apply(Action::CloseTab(i));
            } else {
                self.activate_tab(i);
            }
            return;
        }
        self.open_page(Page::Settings);
    }

    /// Opens `page` in a tab of its own (or focuses the one that is already open).
    pub(crate) fn open_page(&mut self, page: Page) {
        if let Some(i) = self.tabs.iter().position(|t| t.page == Some(page)) {
            self.activate_tab(i);
            return;
        }
        let ti = self.push_special_tab();
        self.tabs[ti].page = Some(page);
    }

    /// Adds a tab for a page or the Recycle Bin (the caller sets which) and makes it the active one.
    /// In tiled mode it takes over the focused pane. Returns its index.
    pub(crate) fn push_special_tab(&mut self) -> usize {
        let dir = self.tabs[self.active].dir.clone();
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        if let Some(tree) = self.tiles.as_mut() {
            let cur = self.tabs[self.active].id;
            tree.replace(cur, id);
        }
        let mut tab = Tab::new(id, dir);
        tab.load_gen = 1; // nothing to read: it is not a folder listing
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        self.on_active_changed();
        self.active
    }

    /// Applies what the Settings tab changed this frame (it works on a copy of the settings).
    pub(crate) fn finish_settings_page(&mut self, ui: &mut Ui, sp: SettingsPage) {
        let SettingsPage { old, s, font_edit, rescan, choose, delete_index, reset_quick, default_request, show_licenses, check_update, download_update, .. } = sp;
        self.font_edit = font_edit;
        if show_licenses {
            self.open_page(Page::Licenses);
        }
        if s != old {
            // Only the fields the page edits: window geometry, columns and the session belong to the app.
            self.settings.copy_page_fields_from(&s);
            self.settings_dirty = true;
            if s.theme != old.theme {
                ui.set_theme(theme_pref(s.theme));
            }
            // For settings requiring restarts. Will be deprecated once logic is moved out of eframe.
            let restart_fields = |x: &Settings| (x.custom_frame, x.git_activated, x.blur_paning);
            if restart_fields(&s) != restart_fields(&old) {
                (self.settings.custom_frame, self.settings.git_activated, self.settings.blur_paning) = restart_fields(&s);
                self.require_restart = true;
            }
            if s.show_hidden != old.show_hidden || s.date_format != old.date_format {
                self.reload_all();
            }
            if s.network_drives != old.network_drives {
                self.refresh_drives();
            }
            if s.calculate_folder_sizes != old.calculate_folder_sizes {
                self.set_folder_sizes(s.calculate_folder_sizes);
            }
        }
        if self.settings_dirty && !ui.input(|i| i.pointer.any_down()) {
            save_settings(&self.settings);
            self.settings_dirty = false;
        }
        //Sets new default app with registry key, or reverts to default windows explorer.
        if let Some(on) = default_request {
            let result = if on { default_app::set_default() } else { default_app::restore_default() };
            self.default_error = result.err().map(|e| e.to_string());
            self.default_on = default_app::is_default();
        }
        if reset_quick {
            self.settings.quick_access = None;
            self.rebuild_quick();
            save_settings(&self.settings);
        }
        if rescan {
            if let Some(ix) = &self.indexer {
                ix.clear();
            }
            self.start_indexing(true);
        }
        if delete_index {
            if let Some(ix) = &self.indexer {
                ix.clear();
            }
            for t in &mut self.tabs {
                for e in t.entries.iter_mut().filter(|e| e.is_dir) {
                    e.dir_size = None;
                }
            }
            self.start_indexing(true);
        }
        if choose {
            if let Some(p) = rfd::FileDialog::new().set_directory(&self.index_dir).pick_folder() {
                self.index_dir = p.clone();
                self.settings.index_dir = Some(p.clone());
                // Without the indexer only the location is remembered; it is used when it is turned on.
                if let Some(ix) = self.indexer.as_mut() {
                    ix.set_dir(&p);
                    self.start_indexing(true);
                }
                save_settings(&self.settings);
            }
        }
        if check_update {
            self.apply(Action::CheckForUpdate);
        }
        if download_update {
            self.apply(Action::DownloadUpdate);
        }
    }
}
