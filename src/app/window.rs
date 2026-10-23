//! Window tracking (position, other windows, cursor).

use crate::prelude::*;

impl Explorer {
    /// True when the OS cursor is outside this window (only known on Windows).
    pub(crate) fn cursor_outside_window(&self) -> Option<bool> {
        let (cx, cy) = cursor_screen_pos()?;
        let o = self.own_info?;
        Some(!(cx >= o.rect[0] && cx < o.rect[2] && cy >= o.rect[1] && cy < o.rect[3]))
    }

    /// Keeps the saved window size current and tells other windows where this one is.
    pub(crate) fn track_window(&mut self, ctx: &egui::Context) {
        let (inner, outer, maxi, mini, full, focused) = ctx.input(|i| {
            let v = i.viewport();
            (
                v.inner_rect,
                v.outer_rect,
                v.maximized.unwrap_or(false),
                v.minimized.unwrap_or(false),
                v.fullscreen.unwrap_or(false),
                i.focused,
            )
        });
        // Once at startup: if the saved position is on a monitor that is no longer connected
        // the (undecorated) window would be unreachable, so bring it back to the primary one.
        let mut off_screen = false;
        if !maxi && !mini && !full {
            let id = Id::new("onscreen_checked");
            let done = ctx.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
            if !done {
                if let Some(r) = outer.or(inner) {
                    ctx.data_mut(|d| d.insert_temp(id, true));
                    let ppp = ctx.pixels_per_point();
                    // The strip of title bar a user would grab: 120 x 32 pt around its middle.
                    let cx = r.center().x * ppp;
                    let (l, t) = ((cx - 60.0 * ppp) as i32, r.top() * ppp);
                    let visible = rect_touches_a_monitor(
                        l,
                        t as i32,
                        l + (120.0 * ppp) as i32,
                        (t + 32.0 * ppp) as i32,
                    );
                    if !visible {
                        off_screen = true;
                        // Primary monitor's top-left is always (0, 0).
                        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos2(
                            80.0, 80.0,
                        )));
                        self.settings.window_pos = None;
                    }
                }
            }
        }
        if !mini && !full {
            self.settings.maximized = maxi;
            if !maxi {
                if let Some(r) = inner {
                    let s = [r.width().round(), r.height().round()];
                    if s[0] >= 520.0 && s[1] >= 340.0 {
                        self.settings.window_size = Some(s);
                    }
                }
                // Position: only while the window is in its normal state, so restoring from
                // maximised or minimised (Windows reports -32000 there) never overwrites it.
                if let Some(r) = outer.or(inner).filter(|_| !off_screen) {
                    let p = [r.left().round(), r.top().round()];
                    if window_pos_is_sane(p) {
                        self.settings.window_pos = Some(p);
                    }
                }
            }
        }

        // Write the geometry to disk shortly after the window stops moving or resizing, so a
        // crash, kill or power loss doesn't lose it (the clean-exit save still runs too).
        {
            type Geo = (Option<[f32; 2]>, Option<[f32; 2]>, bool);
            let id = Id::new("saved_window_geo");
            let geo: Geo = (
                self.settings.window_pos,
                self.settings.window_size,
                self.settings.maximized,
            );
            let (mut seen, mut at, mut saved) = ctx
                .data(|d| d.get_temp::<(Geo, Instant, Geo)>(id))
                .unwrap_or((geo, Instant::now(), geo));
            if geo != seen {
                seen = geo;
                at = Instant::now();
            }
            if seen != saved {
                if at.elapsed() >= Duration::from_secs(1) {
                    save_settings(&self.settings);
                    saved = seen;
                } else {
                    ctx.request_repaint_after(Duration::from_millis(1100));
                }
            }
            ctx.data_mut(|d| d.insert_temp(id, (seen, at, saved)));
        }
        if let Some(r) = inner {
            let ppp = ctx.pixels_per_point();
            let rect = [r.left() * ppp, r.top() * ppp, r.right() * ppp, r.bottom() * ppp];
            let moved = self
                .own_info
                .map_or(true, |o| o.rect.iter().zip(rect.iter()).any(|(a, b)| (a - b).abs() > 0.5));
            let refocus = focused && self.info_stamp.elapsed() > Duration::from_secs(1);
            if moved || refocus {
                let focus_ms = if focused {
                    now_ms()
                } else {
                    self.own_info.map_or(0, |o| o.focus_ms)
                };
                let info = WinInfo { rect, focus_ms };
                self.peers.set_info(info);
                self.own_info = Some(info);
                self.info_stamp = Instant::now();
            }
        }
    }
}
