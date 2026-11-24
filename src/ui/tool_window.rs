//! Separate native windows for tools (file operation progress, Properties): the same look as the main
//! window, with its own caption buttons, the backdrop effect, and a height that fits the content.

use crate::prelude::*;

pub(crate) struct ToolWindow<'a> {
    /// Identifies the window (and what is remembered about it).
    pub(crate) key: &'a str,
    /// The caption drawn in the title bar.
    pub(crate) caption: &'a str,
    pub(crate) width: f32,
    /// Taller content than this scrolls.
    pub(crate) max_height: f32,
    pub(crate) transparency: TransparencyEffect,
    pub(crate) custom_frame: bool,
    /// The window's own close button / Alt+F4 only reports a request; the owner decides what it means.
    /// Set to hold the window open while the owner works on it (for example, to wait for a cancel).
    pub(crate) hold_open: bool,
}

/// Keys of the state remembered about a window: the backdrop that was applied to it, and the height it was
/// last sized to.
fn state_keys(key: &str) -> (Id, Id) {
    (Id::unique((key, "backdrop")), Id::unique((key, "height")))
}

/// Where the window opened (screen position, points). Decided once, when the window is created: a window
/// that already exists keeps its place, and moving it again every frame would fight the user's dragging.
fn position_id(key: &str) -> Id {
    Id::unique((key, "position"))
}

/// Next to the mouse pointer, when it is known and over this app (None: the system decides).
fn near_pointer(ctx: &egui::Context, m: &Metrics, window_width: f32) -> Option<egui::Pos2> {
    let (inner, pointer, monitor) =
        ctx.input(|i| (i.viewport().inner_rect, i.pointer.latest_pos(), i.viewport().monitor_size));
    let mut at = inner?.min + pointer?.to_vec2() + egui::Vec2::splat(m.pad * 1.5);
    // Keep it on screen: only judged on the monitor at the origin, where the monitor size says enough.
    if let Some(monitor) = monitor.filter(|mon| at.x >= 0.0 && at.y >= 0.0 && at.x < mon.x && at.y < mon.y) {
        at.x = at.x.min(monitor.x - window_width).max(0.0);
        at.y = at.y.min(monitor.y * 0.5).max(0.0);
    }
    Some(at)
}

/// A native window is destroyed by egui as soon as a frame passes without `show_viewport_immediate` for it.
/// The next one gets a brand new window that has neither the backdrop effect nor the fitted size, so what was
/// remembered about the old one must go, or the new window would be taken for already set up.
pub(crate) fn forget_tool_window(ctx: &egui::Context, key: &str) {
    let (backdrop_key, height_key) = state_keys(key);
    ctx.data_mut(|d| {
        d.remove_temp::<(bool, TransparencyEffect)>(backdrop_key);
        d.remove_temp::<f32>(height_key);
        d.remove_temp::<egui::Pos2>(position_id(key));
    });
}

/// The native window title for a caption (also how the window is found to apply the backdrop).
pub(crate) fn tool_window_title(caption: &str) -> String {
    format!("{caption} – FileFly")
}

/// Shows the window and fills it with `content`. Returns whether the user asked to close it.
pub(crate) fn tool_window(
    ctx: &egui::Context,
    m: &Metrics,
    pal: &Palette,
    spec: &ToolWindow,
    content: impl FnOnce(&mut Ui),
) -> bool {
    let (backdrop_key, height_key) = state_keys(spec.key);
    let win_w = spec.width + m.pad * 4.0;
    let bar_h = (m.accessibility_scale * 32.0).round();
    let win_title = tool_window_title(spec.caption);
    // Same look as the main window: no native frame, transparent so the backdrop shows, with our own
    // caption buttons.
    let mut builder = egui::ViewportBuilder::default()
        .with_title(win_title.clone())
        .with_inner_size([win_w, m.accessibility_scale * 220.0])
        .with_resizable(false)
        .with_maximize_button(false)
        .with_decorations(!spec.custom_frame)
        .with_transparent(true);
    let position = ctx.data(|d| d.get_temp::<egui::Pos2>(position_id(spec.key))).or_else(|| {
        let at = near_pointer(ctx, m, win_w)?;
        ctx.data_mut(|d| d.insert_temp(position_id(spec.key), at));
        Some(at)
    });
    if let Some(at) = position {
        builder = builder.with_position(at);
    }
    let pad = (m.pad * 2.0) as i8;
    let frame = egui::Frame::new().fill(Color32::TRANSPARENT).inner_margin(Margin {
        left: pad,
        right: pad,
        top: (bar_h + m.pad) as i8,
        bottom: pad,
    });
    let max_content_h = (spec.max_height - bar_h - m.pad * 3.0).max(m.accessibility_scale * 120.0);
    let vid = egui::ViewportId::from_hash_of(spec.key);

    let mut close_requested = false;
    let mut content = Some(content);
    ctx.show_viewport_immediate(vid, builder, |ui, _class| {
        // Backdrop over the whole window (the native window only exists once it has been shown, so this is
        // retried until it works and redone when the theme changes).
        let want = (pal.dark, spec.transparency);
        if ui.ctx().data(|d| d.get_temp::<(bool, TransparencyEffect)>(backdrop_key)) != Some(want)
            && backdrop_by_title(&win_title, pal.dark, spec.transparency)
        {
            ui.ctx().data_mut(|d| d.insert_temp(backdrop_key, want));
        }
        let full = ui.max_rect();
        ui.layer_painter(egui::LayerId::background()).rect_filled(full, 0.0, pal.layer);

        // Custom title bar: caption text, drag area, minimize / close.
        let caption = Rect::from_min_size(
            pos2(full.right() - m.accessibility_scale * 46.0 * 2.0, full.top()),
            vec2(m.accessibility_scale * 46.0 * 2.0, bar_h),
        );
        let drag = ui.interact(
            Rect::from_min_max(full.min, pos2(caption.left(), full.top() + bar_h)),
            Id::unique((spec.key, "drag")),
            Sense::click_and_drag(),
        );
        if drag.drag_started_by(egui::PointerButton::Primary) {
            ui.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        ui.painter().text(
            pos2(full.left() + m.pad * 2.0, full.top() + bar_h * 0.5),
            Align2::LEFT_CENTER,
            spec.caption,
            FontId::proportional(m.font * 0.95),
            pal.text,
        );
        if spec.custom_frame {
            caption_buttons(ui, m, pal, false, false);
        }

        close_requested = ui.ctx().input(|i| i.viewport().close_requested());
        if close_requested && spec.hold_open {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }

        let panel = egui::CentralPanel::default().frame(frame.clone()).show(ui, |ui| {
            ui.set_width(spec.width);
            // `CentralPanel` stretches to fill the window, so the content height comes from the scroll
            // area (otherwise the window could only ever grow).
            let scroll = egui::ScrollArea::vertical()
                .max_height(max_content_h)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.set_width(spec.width);
                    if let Some(content) = content.take() {
                        content(ui);
                    }
                });
            scroll.content_size.y
        });
        // Fit the window to its content.
        let need = (panel.inner + bar_h + m.pad * 3.0).min(spec.max_height);
        let last = ui.ctx().data(|d| d.get_temp::<f32>(height_key)).unwrap_or(0.0);
        // A minimized window ignores resize commands, so wait: remembering the height as applied
        // now would mean it is never sent once the window is restored.
        let minimized = ui.ctx().input(|i| i.viewport().minimized).unwrap_or(false);
        if !minimized && (need - last).abs() > 1.0 {
            ui.ctx().data_mut(|d| d.insert_temp(height_key, need));
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(win_w, need)));
        }
    });
    close_requested
}
