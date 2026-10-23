//! The `eframe::App` implementation (frame loop).

use crate::prelude::*;

impl eframe::App for Explorer {
    fn logic(
        &mut self,
        ctx: &egui::Context,
        frame: &mut eframe::Frame,
    ) {
        // System/window state and background processing.
        self.sync_system(ctx, frame);
        self.track_window(ctx);
        handle_resize(ctx, &self.m);

        self.pump_messages();
        self.pump_search(ctx);

        // Load a fallback font the first time a script that needs one is on screen
        // (names are noted as listings arrive; typed text is checked here).
        {
            let tab = &self.tabs[self.active];
            note_name(&tab.address);
            note_name(&tab.search);
            if let Some(r) = &tab.rename {
                note_name(&r.text);
            }
        }
        sync_fonts(ctx);
        icons_begin_frame(ctx);

        // Sets number dividers globally.
        ctx.all_styles_mut(|style| {
            style.number_formatter = egui::style::NumberFormatter::new(|val, _decimal_range| {
                // This needs to be changed when different locales are supported.
                (val as i64).to_formatted_string(&Locale::en)
            });
        });

        // Handle closing the app before the UI is drawn.
        if ctx.input(|i| i.viewport().close_requested()) {
            // Vanish first, then save.
            hide_window(frame);
            save_settings(&self.settings);
            self.peers.shutdown();
            self.indexer.save_if_dirty(&self.index_dir, true);
            remove_archive_cache(); // unpacked zips are temporary
            finish_journal(); // (keeps its entries if a copy was still running)

            // The OS cleans up at once.
            terminate_now();
        }
    }

    fn ui(
        &mut self,
        ui: &mut Ui,
        frame: &mut eframe::Frame
    ) {
        // Background painting.
        let rect = ui.content_rect();

        // "Try as administrator" is only offered while the error it belongs to is still showing
        // (the error bar of the table reads which tab that is).
        if self.admin_retry.as_ref().is_some_and(|(m, _)| self.tabs[self.active].error.as_ref() != Some(m)) {
            self.admin_retry = None;
        }
        let retry_tab = if self.admin_retry.is_some() { self.tabs[self.active].id } else { u64::MAX };
        ui.data_mut(|d| d.insert_temp(Id::unique("admin_retry_tab"), retry_tab));
        let painter = ui.layer_painter(egui::LayerId::background());

        if self.mica {
            painter.rect_filled(
                rect,
                8.0,
                self.pal.chrome,
            );
        } else {
            painter.rect_filled(rect, 8.0, self.pal.layer);
        }

        // Load tabs that are on screen but have never been read (restored session).
        for ti in self.visible_tabs() {
            if self.tabs[ti].load_gen == 0 {
                self.load_tab(ti);
            }
        }

        // Collect actions triggered by the UI and shortcuts.
        let mut actions: Vec<Action> = Vec::new();
        self.collect_shortcuts(ui, &mut actions);

        // Draw the interface.
        self.ui_tabs(ui, &mut actions);
        self.ui_nav(ui, &mut actions);
        self.ui_command(ui, &mut actions);
        self.ui_sidebar(ui, &mut actions);
        self.ui_status(ui);
        self.ui_central(ui, &mut actions);
        self.drive_item_drag(frame, ui, &mut actions);
        self.handle_dropped_files(ui);

        // Rounded inner corner where the band above meets the file
        // view, to the right of the sidebar.
        if let Some(r) = self.sidebar_rect {
            let painter = ui.layer_painter(egui::LayerId::background());

            corner_fillet(
                &painter,
                pos2(r.right(), r.top()),
                self.m.radius as f32 * 2.0,
                -1.0,
                1.0,
                self.pal.layer,
            );
        }

        self.ui_props(ui);
        self.ui_conflict(ui);
        self.ui_confirm(ui);
        self.ui_ops(ui);
        self.ui_picker(ui);
        self.ui_licenses(ui);

        // Apply actions after rendering, as in your original code.
        for action in actions {
            self.apply(action);
        }

        // Keep the session current, and write it to disk a second after the last change
        // (the clean-exit save also includes it).
        if !SESSION_READONLY.load(AtomicOrdering::Relaxed) {
            let snap = self.session_snapshot();
            let id = Id::unique("saved_session");
            let (mut seen, mut at, mut saved) = ui
                .data(|d| d.get_temp::<(Session, Instant, Session)>(id))
                .unwrap_or_else(|| (snap.clone(), Instant::now(), snap.clone()));
            if snap != seen {
                seen = snap.clone();
                at = Instant::now();
            }
            self.settings.session = snap;
            if seen != saved {
                if at.elapsed() >= Duration::from_secs(1) {
                    save_settings(&self.settings);
                    saved = seen.clone();
                } else {
                    ui.ctx().request_repaint_after(Duration::from_millis(1100));
                }
            }
            ui.data_mut(|d| {
                d.insert_temp(id, (seen, at, saved));
            });
        }

        // Save column widths as soon as a resize drag ends, not only on a clean exit.
        {
            let id = Id::unique("saved_col_widths");
            let cols = self.settings.col_widths;
            match ui.data(|d| d.get_temp::<[f32; 3]>(id)) {
                None => {
                    ui.data_mut(|d| d.insert_temp(id, cols));
                }
                Some(prev) if prev != cols && !ui.input(|i| i.pointer.any_down()) => {
                    save_settings(&self.settings);
                    ui.data_mut(|d| d.insert_temp(id, cols));
                }
                _ => {}
            }
        }

        // Update the title after applying actions, so tab changes
        // can update the title in the same UI pass.
        let title = format!(
            "{} – FileFly{}",
            self.tabs[self.active].title(),
            if is_elevated() { " (Administrator)" } else { "" }
        );

        if title != self.last_title {
            ui.ctx().send_viewport_cmd(
                egui::ViewportCommand::Title(title.clone()),
            );
            self.last_title = title;
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Every panel paints its own background; Mica shows through
        // the transparent parts.
        [0.0, 0.0, 0.0, 0.0]
    }
}
