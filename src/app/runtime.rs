//! The `eframe::App` implementation (frame loop).

use crate::prelude::*;

impl Explorer{

    /// Function to draw the Apps entire interface.
     pub(crate) fn draw_interface(&mut self, ui: &mut Ui, actions: &mut Vec<Action>){
         // Background painting.
         let rect = ui.content_rect();

         // "Try as administrator" is only offered while the error it belongs to is still showing
         // (the error bar of the table reads which tab that is).
        if self.win.admin_retry.as_ref().is_some_and(|(m, _)| {
            self.win.tabs[self.win.active].notice.as_ref().map(|n| &n.text) != Some(m)
        }) {
            self.win.admin_retry = None;
        }
         let retry_tab = if self.win.admin_retry.is_some() { self.win.tabs[self.win.active].id } else { u64::MAX };
         ui.data_mut(|d| d.insert_temp(win_id("admin_retry_tab"), retry_tab));
         let painter = ui.layer_painter(egui::LayerId::background());

        // Draws chrome layer depending on desktop environment
        let corner_radius = match get_desktop() {
            d if d.contains("KDE") => 0.0,
            d if d.contains("GNOME") => 15.0,
            d if d.is_empty() => 8.0,
            _ => 15.0,
        };

         // Removes rounded corners on maximization for Linux installs.
        // On Windows, corner_radius is already 0.0.
        let radius = if !Self::is_maximized(ui.ctx()) { corner_radius } else { 0.0 };
        // Applies the chrome to achieve a proper Mica effect for transparency effects.
        // Although Acrylic looks different, for now we will use the same layer.
        let bg = if self.core.transparency { self.win.pal.chrome } else { self.win.pal.base };
        painter.rect_filled(rect, radius, bg);

        // Draw the interface.
        self.ui_tabs(ui, actions);
        self.ui_nav(ui, actions);
        self.ui_command(ui, actions);
        self.ui_sidebar(ui, actions);
        self.ui_status(ui);
        self.ui_central(ui, actions);

         // Draw the interface (time measurement debug version).
        /*
        let t_tabs = Instant::now();
         self.ui_tabs(ui, actions);
        let delta = t_tabs.elapsed();
        if delta.as_millis() > 30 { eprintln!("slow tabs: {delta:?}"); }
        let t_nav = Instant::now();
         self.ui_nav(ui, actions);
        let delta = t_nav.elapsed();
        if delta.as_millis() > 30 { eprintln!("slow nav: {delta:?}"); }
        let t_command = Instant::now();
         self.ui_command(ui, actions);
        let delta = t_command.elapsed();
        if delta.as_millis() > 30 { eprintln!("slow command: {delta:?}"); }
        let t_sidebar = Instant::now();
         self.ui_sidebar(ui, actions);
        let delta = t_sidebar.elapsed();
        if delta.as_millis() > 30 { eprintln!("slow sidebar: {delta:?}"); }
        let t_status = Instant::now();
         self.ui_status(ui);
        let delta = t_status.elapsed();
        if delta.as_millis() > 30 { eprintln!("slow status: {delta:?}"); }
        let t_central = Instant::now();
         self.ui_central(ui, actions);
        let delta = t_central.elapsed();
        if delta.as_millis() > 30 { eprintln!("slow central: {delta:?}"); }
        */


         // Rounded inner corner where the band above meets the file
         // view, to the right of the sidebar.
         if let Some(r) = self.win.sidebar_rect {
             let painter = ui.layer_painter(egui::LayerId::background());

             corner_fillet(
                 &painter,
                 pos2(r.right(), r.top()),
                 self.win.m.radius as f32 * 2.0,
                 -1.0,
                 1.0,
                 self.win.pal.layer,
             );
         }
    }

    pub(crate) fn draw_backdrop_blur(&mut self, ui: &Ui) {
        let accent = self.core.sys_accent.unwrap_or_else(Accent::fallback);
        let Some(mut backdrop) = self.blur.take() else { return };

        apply_style(backdrop.ctx(), &self.win.m, accent);

        let clear = self.win.pal.layer;
        backdrop.render(ui.ctx(), clear, |ui2| {
            self.win.is_backdrop = true;
            let mut discard = Vec::new();
            self.draw_interface(ui2, &mut discard);
            self.win.is_backdrop = false;
        });
        self.blur = Some(backdrop);
    }
}

impl Explorer {
    /// The clean-quit sequence. The order matters: settings and index are saved first, then running operations
    /// are cancelled and given a moment to remove their temp items, and only then is the journal closed
    /// (it keeps its entries if an operation is still mid-write, so the next start can recover).
    /// Runs once, when the last window closed (`Explorer::quit`) or the host ended on its own (`Host::on_exit`).
    /// Must be the last thing the process does besides ending; it is not repeatable after `peers.shutdown`.
    /// Closing any other window does not come through here: operations belong to the core, not to a window.
    pub(crate) fn shutdown(&mut self) {
        save_settings(&self.core.settings);
        self.core.peers.shutdown();
        if let Some(ix) = &self.core.indexer {
            ix.save_if_dirty(true);
        }
        self.stop_ops_for_exit();
        remove_archive_cache(); // unpacked zips are temporary
        finish_journal(); // (keeps its entries if a copy was still running)
    }
}
