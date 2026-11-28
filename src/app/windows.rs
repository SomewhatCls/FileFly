//! Several windows over one core: swapping a window into `Explorer::win`, creating and closing windows,
//! and the host frame that shows them all.
//!
//! The root OS window is hidden and only hosts egui: every real window is an immediate viewport of it, so
//! closing any one of them (the first included) never ends the app. The last window to close runs the quit
//! sequence (`Explorer::shutdown`).

use crate::prelude::*;
use std::cell::Cell as StdCell;

thread_local! {
    /// Serial of the window being drawn (set by `with_window`, 0 outside of one).
    static DRAWING: StdCell<u64> = const { StdCell::new(0) };
    /// True while `show_viewport` runs: a failed window creation is expected and handled there.
    static VIEWPORT_MAY_FAIL: StdCell<bool> = const { StdCell::new(false) };
}

/// What egui panics with when eframe could not run a viewport's callback (see `show_viewport`).
const NO_CALLBACK: &str = "the user callback was never called";

/// Whether the panic described by `message` is the failed window creation that `show_viewport` handles itself.
/// The panic hook uses it to stay quiet about that one (and only that one: it would otherwise fill `crash.log`).
pub(crate) fn is_handled_viewport_failure(message: &str) -> bool {
    message.contains(NO_CALLBACK) && VIEWPORT_MAY_FAIL.with(|f| f.get())
}

/// `Context::show_viewport_immediate` that survives a frame in which the OS window cannot be created. eframe
/// creates windows only while it holds the event loop, and a frame painted for the invisible host window does not
/// always (a timer woke the loop instead of an event): egui then panics ("the user callback was never called").
/// Here that frame shows nothing, a repaint is requested (it arrives as an event, with the event loop), and the
/// window is created in the frame after. None when that happened. Any other panic goes on as it was.
pub(crate) fn show_viewport<T>(
    ctx: &egui::Context,
    id: egui::ViewportId,
    builder: egui::ViewportBuilder,
    callback: impl FnMut(&mut Ui, egui::ViewportClass) -> T,
) -> Option<T> {
    let outer = VIEWPORT_MAY_FAIL.with(|f| f.replace(true)); // dialogs nest inside windows: restore, don't clear
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ctx.show_viewport_immediate(id, builder, callback)));
    VIEWPORT_MAY_FAIL.with(|f| f.set(outer));
    match result {
        Ok(value) => Some(value),
        Err(payload) => {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .unwrap_or_default();
            if !message.contains(NO_CALLBACK) {
                std::panic::resume_unwind(payload);
            }
            ctx.request_repaint();
            None
        }
    }
}

/// An egui id for temp data of the window being drawn. egui's data store is shared by all windows, so a key that
/// is not namespaced by window would be clobbered by the next window drawn. Only valid inside `with_window`.
pub(crate) fn win_id(key: impl Hash + std::fmt::Debug) -> Id {
    Id::unique((DRAWING.with(|d| d.get()), key))
}

/// The start title of window `serial`: unique, so its OS window can be found (`find_window`) and given the
/// backdrop and OS drag. Replaced by the real title as soon as the handle is known.
pub(crate) fn window_token(serial: u64) -> String {
    format!("FileFly #{serial}")
}

/// What the OS window of a new window is created with. `pos` overrides the saved position; only the first
/// window (the session owner) restores its saved position and maximized state.
pub(crate) fn window_builder(
    saved: &Settings,
    pos: Option<[f32; 2]>,
    icon: Arc<egui::IconData>,
    first: bool,
    serial: u64,
) -> egui::ViewportBuilder {
    let size = saved
        .window_size
        .filter(|s| s[0] >= 520.0 && s[1] >= 340.0 && s[0] <= 10000.0 && s[1] <= 10000.0)
        .unwrap_or([1180.0, 760.0]);
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(window_token(serial))
        .with_inner_size(size)
        .with_min_inner_size([520.0, 340.0])
        .with_resizable(true)
        .with_decorations(!saved.custom_frame)
        .with_transparent(true) // required for the Mica backdrop
        .with_icon(icon);
    if let Some(p) = pos {
        viewport = viewport.with_position(p);
    } else if first {
        if let Some(p) = saved.window_pos.filter(|p| window_pos_is_sane(*p)) {
            viewport = viewport.with_position(p);
        }
        if saved.maximized {
            viewport = viewport.with_maximized(true);
        }
    }
    viewport
}

/// Every window, the swapped-in one (`cur`) included. A free function so callers can keep borrowing
/// `Explorer::core` at the same time.
pub(crate) fn windows_mut<'a>(cur: Option<usize>, win: &'a mut Window, windows: &'a mut [Window]) -> Vec<&'a mut Window> {
    let mut current = Some(win);
    windows
        .iter_mut()
        .enumerate()
        .map(|(i, w)| if Some(i) == cur { current.take().expect("one swapped-in window") } else { w })
        .collect()
}

impl Explorer {
    /// Swaps window `i` into `self.win`. Not nestable: pair with `end_window`.
    pub(crate) fn begin_window(&mut self, i: usize) {
        debug_assert!(self.cur.is_none(), "windows cannot be nested");
        std::mem::swap(&mut self.win, &mut self.windows[i]);
        self.cur = Some(i);
        DRAWING.with(|d| d.set(self.win.serial));
    }

    /// Puts the window back into `windows` and leaves the placeholder in `win`.
    pub(crate) fn end_window(&mut self) {
        if let Some(i) = self.cur.take() {
            std::mem::swap(&mut self.win, &mut self.windows[i]);
        }
        DRAWING.with(|d| d.set(0));
    }

    /// Runs `f` with window `i` as `self.win`. Nothing in `f` may call `with_window` again: work that
    /// involves another window goes through `Explorer::pending`.
    pub(crate) fn with_window<R>(&mut self, i: usize, f: impl FnOnce(&mut Self) -> R) -> R {
        self.begin_window(i);
        let r = f(self);
        self.end_window();
        r
    }

    /// Every window, the swapped-in one included.
    pub(crate) fn each_window(&self) -> Vec<&Window> {
        let cur = self.cur;
        self.windows.iter().enumerate().map(|(i, w)| if Some(i) == cur { &self.win } else { w }).collect()
    }

    /// Every window, the swapped-in one included.
    pub(crate) fn each_window_mut(&mut self) -> Vec<&mut Window> {
        windows_mut(self.cur, &mut self.win, &mut self.windows)
    }

    pub(crate) fn window_index(&self, serial: u64) -> Option<usize> {
        self.each_window().iter().position(|w| w.serial == serial)
    }

    /// Index of the window that owns tab `id` (tab ids are unique across windows: `Core::next_tab_id`).
    pub(crate) fn tab_owner(&self, id: u64) -> Option<usize> {
        self.each_window().iter().position(|w| w.tabs.iter().any(|t| t.id == id))
    }

    /// Whether the window being drawn is the one that last had the focus (see `Explorer::focus`).
    pub(crate) fn is_focus_window(&self) -> bool {
        self.win.serial == self.focus
    }

    /// The first window owns the saved session and window geometry. When it closes the next one takes over.
    pub(crate) fn owns_session(&self) -> bool {
        self.cur == Some(0)
    }

    /// Opens a new window showing `dir`, at `at` (screen points) when given. Takes the focus.
    pub(crate) fn open_window(&mut self, dir: PathBuf, at: Option<(f32, f32)>) {
        let serial = self.next_window;
        self.next_window += 1;
        let id = self.core.next_tab_id;
        self.core.next_tab_id += 1;
        self.push_window(serial, Tab::new(id, dir), at);
    }

    /// Adds window `serial` holding `tab`. Safe to call from inside a window frame.
    fn push_window(&mut self, serial: u64, tab: Tab, at: Option<(f32, f32)>) {
        let icon = self.icon.clone().unwrap_or_default();
        let builder = window_builder(&self.core.settings, at.map(|(x, y)| [x, y]), icon, false, serial);
        let w = Window::new(serial, tab, &self.core.settings, self.ctx.clone(), builder);
        self.windows.push(w);
        self.focus = serial;
        // Its first frame loads the tab and registers its folder (`fresh`, see `window_frame`).
        self.ctx.request_repaint();
    }

    /// Removes window `serial`. Operations are not touched: they belong to the core. Runs the quit
    /// sequence when it was the last window.
    pub(crate) fn close_window(&mut self, serial: u64) {
        let Some(i) = self.window_index(serial) else { return };
        if self.windows.len() == 1 {
            self.quit();
        }
        self.windows.remove(i);
        trim_memory_soon();
        if self.focus == serial {
            self.focus = self.windows[0].serial;
        }
        // The union of watched and scanned folders changed.
        self.start_indexing(false);
        self.ctx.request_repaint();
    }

    /// The clean quit: vanish first, then save and stop everything, then end the process. The same sequence
    /// whatever route ends the app (last window closed, last tab closed). Never returns.
    pub(crate) fn quit(&mut self) -> ! {
        for w in self.each_window() {
            if let Some(n) = &w.native {
                hide_window(n);
            }
        }
        self.shutdown();
        terminate_now();
    }

    /// Starts every window's own state over around the same core (for settings that are only read when a window
    /// is set up). Nothing is stopped: operations keep running and keep their locks. Safe to call from inside a
    /// window frame: the work is done at the start of the next host frame.
    pub(crate) fn refresh_windows(&mut self) {
        self.core.settings.session = self.session_snapshot();
        save_settings(&self.core.settings);
        self.refresh_pending = true;
        self.ctx.request_repaint();
    }

    /// Work queued by windows that involves other windows (tab moves, merges).
    fn run_pending(&mut self) {
        for cmd in std::mem::take(&mut self.pending) {
            match cmd {
                WinCmd::MoveTab { from, tab, to, at } => self.transfer_tab(from, tab, to, at),
                WinCmd::MergeWindow { from, to } => {
                    let Some(fi) = self.window_index(from) else { continue };
                    let ids: Vec<u64> = self.windows[fi].tabs.iter().map(|t| t.id).collect();
                    for id in ids {
                        self.transfer_tab(from, id, Some(to), None);
                    }
                    self.close_window(from);
                }
            }
        }
    }

    /// Moves a tab, with all its state, between windows (or into a new one). Does nothing for a special tab
    /// (Settings, Recycle Bin: they belong to their window) or when either window is gone.
    fn transfer_tab(&mut self, from: u64, tab: u64, to: Option<u64>, at: Option<(f32, f32)>) {
        let Some(fi) = self.window_index(from) else { return };
        let target = match to {
            Some(s) => match self.window_index(s) {
                Some(i) => Some(i),
                None => return,
            },
            None => None,
        };
        if target == Some(fi) {
            return;
        }
        let Some(ti) = self.windows[fi].tabs.iter().position(|t| t.id == tab) else { return };
        if self.windows[fi].tabs[ti].special() {
            return;
        }
        let only = self.windows[fi].tabs.len() == 1;
        if only && target.is_none() {
            return; // the only tab already is a window of its own
        }
        // Take the tab out; the source keeps at least one tab (a merge closes it right after).
        let moved = {
            let w = &mut self.windows[fi];
            let t = w.tabs.remove(ti);
            if let Some(tiles) = w.tiles.take() {
                w.tiles = remove_leaf(tiles, t.id);
            }
            if w.tabs.is_empty() {
                w.tabs.push(Tab::new(0, t.dir.clone())); // placeholder: the window is closed by the caller
                w.active = 0;
            } else {
                w.active = w.active.min(w.tabs.len() - 1);
                if ti < w.active {
                    w.active -= 1;
                }
            }
            t
        };
        if !only {
            self.with_window(fi, |ex| {
                ex.on_active_changed();
                ex.fix_tiles();
            });
        }
        match target {
            Some(i) => {
                let serial = self.windows[i].serial;
                self.windows[i].tabs.push(moved);
                self.windows[i].active = self.windows[i].tabs.len() - 1;
                self.focus = serial;
                self.with_window(i, |ex| {
                    ex.on_active_changed();
                    ex.fix_tiles();
                    ex.win.ui.send_viewport_cmd_to(ex.win.vid, egui::ViewportCommand::Focus);
                });
            }
            None => {
                let serial = self.next_window;
                self.next_window += 1;
                self.push_window(serial, moved, at);
            }
        }
    }

    /// One frame for every window. Called by the host app (`startup::Host`) with the root context.
    pub(crate) fn host_frame(&mut self, ctx: &egui::Context) {
        self.ctx = ctx.clone();
        self.run_pending();
        self.pump_core();
        icons_begin_frame(ctx);
        sync_fonts(ctx);
        // Sets number dividers globally.
        ctx.all_styles_mut(|style| {
            style.number_formatter = egui::style::NumberFormatter::new(|val, _decimal_range| {
                // This needs to be changed when different locales are supported.
                (val as i64).to_formatted_string(&Locale::en)
            });
        });

        if std::mem::take(&mut self.refresh_pending) {
            for w in &mut self.windows {
                w.refresh_shell(&self.core.settings);
            }
            self.apply_render_settings();
        }

        // Decided before any window is drawn, so exactly one window hosts the shared dialogs this frame.
        for w in &self.windows {
            if ctx.input_for(w.vid, |i| i.focused) {
                self.focus = w.serial;
            }
        }
        let mut closing = Vec::new();
        for i in 0..self.windows.len() {
            let (vid, builder) = (self.windows[i].vid, self.windows[i].builder.clone());
            let mut close = false;
            show_viewport(ctx, vid, builder, |ui, _class| {
                close = self.with_window(i, |ex| ex.window_frame(ui));
            });
            if close {
                closing.push(self.windows[i].serial);
            }
        }
        for serial in closing {
            self.close_window(serial);
        }
    }

    /// Looks the OS window up by its start title, once it exists, and applies the real title.
    fn track_native(&mut self) {
        if self.win.native.is_some() {
            return;
        }
        if let Some(n) = find_window(&window_token(self.win.serial)) {
            set_taskbar_icon(&n);
            self.win.native = Some(n);
            self.win.last_title.clear(); // makes the title code set the real title
        } else {
            self.win.ui.request_repaint();
        }
    }

    /// After a live frame change (`Window::refresh_shell`) the OS window's client area changes without any resize
    /// event reaching eframe, so its GL surface keeps the old size and the desktop compositor stretches the
    /// picture (and the pointer no longer matches what is drawn). A real size change makes eframe resize the
    /// surface: once the frame change has landed, grow the window by one point and put it back.
    fn settle_surface(&mut self, ctx: &egui::Context) {
        let Some(state) = self.win.reshape.take() else { return };
        let (size, maximized) = ctx.input(|i| {
            let v = i.viewport();
            (v.inner_rect.map(|r| r.size()), v.maximized.unwrap_or(false) || v.fullscreen.unwrap_or(false))
        });
        let Some(size) = size.filter(|_| !maximized) else { return }; // a maximized window keeps its client size
        self.win.reshape = match state {
            Reshape::Wait { before: None, .. } => {
                ctx.request_repaint();
                Some(Reshape::Wait { before: Some(size), frames: 0 })
            }
            Reshape::Wait { before: Some(b), frames } => {
                if size != b {
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size + egui::vec2(1.0, 1.0)));
                    ctx.request_repaint();
                    Some(Reshape::Restore(size))
                } else if frames < 30 {
                    ctx.request_repaint_after(Duration::from_millis(50));
                    Some(Reshape::Wait { before: Some(b), frames: frames + 1 })
                } else {
                    None // the client area did not change: nothing to fix
                }
            }
            Reshape::Restore(original) => {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(original));
                None
            }
        };
    }

    /// One frame of the window that is swapped in, inside its viewport. Returns whether the user closed it.
    fn window_frame(&mut self, ui: &mut Ui) -> bool {
        let ctx = ui.ctx().clone();
        self.track_native();
        self.settle_surface(&ctx);
        if std::mem::take(&mut self.win.fresh) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.on_active_changed();
        }
        // The OS resize loop swallows the button release: forget the pointer state instead (see `chrome.rs`).
        if ctx.data_mut(|d| d.remove_temp::<bool>(win_id("pointer_reset"))).unwrap_or(false) {
            ctx.input_mut(|i| i.pointer = Default::default());
        }

        let native = self.win.native.take();
        self.sync_system(&ctx, native.as_ref());
        self.win.native = native;
        self.track_window(&ctx);
        if self.win.use_custom_window_frame {
            handle_resize(&ctx, &self.win.m);
        }
        if ctx.input(|i| i.viewport().close_requested()) {
            // This frame draws nothing, and eframe presents it before it applies any viewport command (so
            // `Visible(false)` would be one frame late): hide the OS window right now, or an empty backdrop shows.
            if let Some(n) = &self.win.native {
                hide_window(n);
            }
            return true;
        }

        // Load a fallback font the first time a script that needs one is on screen
        // (names are noted as listings arrive; typed text is checked here).
        {
            let tab = &self.win.tabs[self.win.active];
            note_name(&tab.address);
            note_name(&tab.search);
            if let Some(r) = &tab.rename {
                note_name(&r.text);
            }
        }

        // Load tabs that are on screen but have never been read (restored session).
        for ti in self.visible_tabs() {
            if self.win.tabs[ti].load_gen == 0 {
                self.load_tab(ti);
            }
        }

        // Collect actions triggered by the UI and shortcuts.
        let mut actions: Vec<Action> = Vec::new();
        self.collect_shortcuts(ui, &mut actions);

        if self.win.tab_drag.is_some() {
            self.draw_backdrop_blur(ui);
        } else if let Some(b) = self.blur.as_mut() {
            b.release(); // frees the textures when no drag is active
        }
        self.draw_interface(ui, &mut actions);
        self.drive_item_drag(ui, &mut actions);
        self.handle_dropped_files(ui);

        self.ui_props(ui);
        self.ui_confirm(ui);
        self.ui_picker(ui);
        self.ui_whats_new(ui);
        // Dialogs that belong to no window (operations, paste and restore questions): shown by the focused
        // window only, as its child windows, so a question never appears twice. They are not drawn
        // from the hidden root: egui does not run the callback of a viewport nested in a hidden window.
        if self.is_focus_window() {
            self.ui_ops(&ctx);
            self.ui_conflict(&ctx);
            self.ui_restore_conflict(&ctx);
        }

        for action in actions {
            self.apply(action);
        }

        self.save_session_if_owner(ui);

        // Save column widths as soon as a resize drag ends, not only on a clean exit.
        {
            let id = win_id("saved_col_widths");
            let cols = self.core.settings.col_widths;
            match ui.data(|d| d.get_temp::<[f32; 3]>(id)) {
                None => {
                    ui.data_mut(|d| d.insert_temp(id, cols));
                }
                Some(prev) if prev != cols && !ui.input(|i| i.pointer.any_down()) => {
                    save_settings(&self.core.settings);
                    ui.data_mut(|d| d.insert_temp(id, cols));
                }
                _ => {}
            }
        }

        // Update the title after applying actions, so tab changes can update the title in the same UI pass.
        // Until the OS window is found the start title stays (it is how the window is found).
        if self.win.native.is_some() {
            let title = format!(
                "{} – FileFly{}",
                self.win.tabs[self.win.active].title(),
                if is_elevated() { " (Administrator)" } else { "" }
            );
            if title != self.win.last_title {
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
                self.win.last_title = title;
            }
        }
        false
    }

    /// Keeps the session current, and writes it to disk a second after the last change (the clean-exit save
    /// also includes it). Only the first window does: a session is one list of tabs.
    fn save_session_if_owner(&mut self, ui: &Ui) {
        if self.win.session_ended || self.core.settings.session_readonly || !self.owns_session() {
            return;
        }
        let snap = self.session_snapshot();
        let id = win_id("saved_session");
        let (mut seen, mut at, mut saved) = ui
            .data(|d| d.get_temp::<(Session, Instant, Session)>(id))
            .unwrap_or_else(|| (snap.clone(), Instant::now(), snap.clone()));
        if snap != seen {
            seen = snap.clone();
            at = Instant::now();
        }
        self.core.settings.session = snap;
        if seen != saved {
            if at.elapsed() >= Duration::from_secs(1) {
                save_settings(&self.core.settings);
                saved = seen.clone();
            } else {
                ui.ctx().request_repaint_after(Duration::from_millis(1100));
            }
        }
        ui.data_mut(|d| {
            d.insert_temp(id, (seen, at, saved));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(serial: u64) -> Window {
        let s = Settings::default();
        Window::new(serial, Tab::new(serial, PathBuf::from("C:/")), &s, egui::Context::default(), egui::ViewportBuilder::default())
    }

    #[test]
    fn the_swapped_in_window_is_listed_in_its_own_slot() {
        let mut current = window(2);
        let mut rest = vec![window(1), window(0), window(3)]; // slot 1 holds the placeholder of `current`
        let serials: Vec<u64> = windows_mut(Some(1), &mut current, &mut rest).iter().map(|w| w.serial).collect();
        assert_eq!(serials, [1, 2, 3]);
    }

    #[test]
    fn without_a_swapped_in_window_every_slot_is_real() {
        let mut placeholder = window(0);
        let mut all = vec![window(1), window(2)];
        let serials: Vec<u64> = windows_mut(None, &mut placeholder, &mut all).iter().map(|w| w.serial).collect();
        assert_eq!(serials, [1, 2]);
    }

    /// Runs `show_viewport` for one viewport inside a real egui pass (egui has no fonts outside of one).
    fn viewport(ctx: &egui::Context) -> Option<()> {
        ctx.set_embed_viewports(false); // as under eframe: viewports are real windows made by the backend
        let mut shown = None;
        let mut out = ctx.run_ui(egui::RawInput::default(), |_ui| {
            shown = show_viewport(ctx, egui::ViewportId::from_hash_of("t"), egui::ViewportBuilder::default(), |_ui, _class| ());
        });
        out.textures_delta.clear();
        shown
    }

    #[test]
    fn a_window_that_could_not_be_created_this_frame_is_skipped_not_fatal() {
        let ctx = egui::Context::default();
        // eframe without the event loop: the viewport's callback is never run.
        egui::Context::set_immediate_viewport_renderer(|_ctx, _viewport| {});
        assert!(viewport(&ctx).is_none());
        assert!(!is_handled_viewport_failure("the user callback was never called"), "outside show_viewport nothing is handled");
        assert!(viewport(&ctx).is_none(), "and it can be tried again");
    }

    #[test]
    #[should_panic(expected = "boom")]
    fn any_other_panic_still_goes_on() {
        let ctx = egui::Context::default();
        egui::Context::set_immediate_viewport_renderer(|_ctx, _viewport| panic!("boom"));
        let _ = viewport(&ctx);
    }

    #[test]
    fn window_ids_are_namespaced_by_the_window_being_drawn() {
        DRAWING.with(|d| d.set(1));
        let a = win_id("saved_session");
        DRAWING.with(|d| d.set(2));
        let b = win_id("saved_session");
        DRAWING.with(|d| d.set(0));
        assert_ne!(a, b);
        assert_eq!(a, { DRAWING.with(|d| d.set(1)); let x = win_id("saved_session"); DRAWING.with(|d| d.set(0)); x });
    }
}
