//! Settings page.

use crate::prelude::*;

/// Working copy of the settings the Settings tab edits during one frame, plus the buttons that
/// were pressed. `Explorer::finish_settings_page` applies the result.
pub(crate) struct SettingsPage {
    pub(crate) old: Settings,
    pub(crate) s: Settings,
    pub(crate) font: f32,
    pub(crate) font_edit: Option<f32>,
    pub(crate) index_dir: String,
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
}

impl SettingsPage {
    pub(crate) fn new(
        settings: &Settings,
        font_edit: Option<f32>,
        index_dir: String,
        cache_len: usize,
        default_on: bool,
        default_error: Option<String>,
    ) -> Self {
        Self {
            old: settings.clone(),
            s: settings.clone(),
            font: font_edit.unwrap_or(settings.font_size),
            font_edit,
            index_dir,
            cache_len,
            rescan: false,
            choose: false,
            delete_index: false,
            reset_quick: false,
            default_on,
            default_error,
            default_request: None,
            show_licenses: false,
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

/// One setting: title and optional description on the left, `control` vertically centred on the right.
pub(crate) fn fl_row(
    ui: &mut Ui,
    metrics: &Metrics,
    palette: &Palette,
    title: &str,
    description: Option<&str>,
    add_control: impl FnOnce(&mut Ui),
) {
    // ---- Layout constants ----
    let row_width = ui.available_width();
    let padding_x = metrics.pad * 1.6;
    let padding_y = metrics.pad * 0.9;
    let title_description_gap = metrics.s * 2.0;

    // Left ~55% of the row is the text column; the control goes to its right.
    let text_column_width = ((row_width - padding_x * 3.0) * 0.55).max(metrics.s * 120.0);

    // ---- Measure the text ----
    let title_galley = ui.painter().layout(
        title.to_owned(),
        FontId::proportional(metrics.font),
        palette.text,
        text_column_width,
    );
    let description_galley = description.map(|text| {
        ui.painter().layout(
            text.to_owned(),
            FontId::proportional(metrics.font * 0.88),
            palette.text_secondary,
            text_column_width,
        )
    });

    let title_height = title_galley.size().y;
    let text_block_height = match &description_galley {
        Some(desc) => title_height + title_description_gap + desc.size().y,
        None => title_height,
    };

    // ---- Allocate the row ----
    let row_height = (text_block_height + padding_y * 2.0).max(metrics.row_h * 1.55);
    let (row_rect, _response) = ui.allocate_exact_size(vec2(row_width, row_height), Sense::hover());

    // ---- Paint the text, vertically centered in the row ----
    let text_left = row_rect.left() + padding_x;
    let text_top = row_rect.center().y - text_block_height / 2.0;

    ui.painter()
        .galley(pos2(text_left, text_top), title_galley, palette.text);

    if let Some(desc) = description_galley {
        let desc_top = text_top + title_height + title_description_gap;
        ui.painter()
            .galley(pos2(text_left, desc_top), desc, palette.text_secondary);
    }

    // ---- Control area: right of the text column, full row height ----
    let control_rect = Rect::from_min_max(
        pos2(row_rect.left() + padding_x * 2.0 + text_column_width, row_rect.top()),
        pos2(row_rect.right() - padding_x, row_rect.bottom()),
    );

    // new_child does NOT advance the parent's cursor (scope_builder did).
    let mut control_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(control_rect)
            .layout(Layout::right_to_left(Align::Center)),
    );
    add_control(&mut control_ui);
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

pub(crate) fn settings_page(ui: &mut Ui, rect: Rect, m: &Metrics, pal: &Palette, tab_id: u64, sp: &mut SettingsPage) {
    ui.scope_builder(
        egui::UiBuilder::new()
            .id_salt(("settings_page", tab_id))
            .max_rect(rect),
        |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let avail = ui.available_width();
                    let w = (avail - m.pad * 4.0).clamp(m.s * 240.0, m.s * 860.0);
                    let left = ((avail - w) / 2.0).max(m.pad * 2.0);
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
}

pub(crate) fn settings_content(ui: &mut Ui, m: &Metrics, pal: &Palette, sp: &mut SettingsPage) {
    ui.spacing_mut().slider_width = m.s * 150.0;
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
                .width(m.s * 150.0)
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
                let resp = ui.add(
                    egui::Slider::new(&mut sp.font, 10.0..=24.0)
                        .step_by(1.0)
                        .suffix(" pt"),
                );
                if resp.dragged() {
                    sp.font_edit = Some(sp.font);
                } else if resp.drag_stopped() || resp.changed() {
                    eprintln!("commit: sp.font = {}", sp.font);
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

            let is_windows_11:bool =
                if cfg!(target_os = "windows") && System::kernel_version()
                    .unwrap().as_str()
                    .parse::<i32>()
                    .expect("Failed to parse kernel version") >= 22000 {
                    true
                }else{
                    false
                };

            fl_divider(ui, m, pal);
            fl_row(ui, m, pal, "Transparency effects", Some("Choose from available transparency effects"), |ui| {
                let label = |t: TransparencyEffect| match t {
                    TransparencyEffect::Mica => "Mica",
                    TransparencyEffect::Acrylic => "Acrylic",
                    TransparencyEffect::None => "None",
                };
                // Makes selectable transparency effects dependent on Windows version.
                egui::ComboBox::from_id_salt("transparency")
                    .width(m.s * 150.0)
                    .selected_text(label(sp.s.transparency_effect))
                    .show_ui(ui, |ui| {
                        let effects = if is_windows_11 {
                            vec![
                                TransparencyEffect::Mica,
                                TransparencyEffect::Acrylic,
                                TransparencyEffect::None,
                            ]
                        } else {
                            vec![
                                TransparencyEffect::Acrylic,
                                TransparencyEffect::None,
                            ]
                        };

                        for t in effects {
                            ui.selectable_value(
                                &mut sp.s.transparency_effect,
                                t,
                                label(t),
                            );
                        }
                    });
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
                .width(m.s * 150.0)
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
                if secondary_button(ui, m, "Restore defaults").clicked() {
                    sp.reset_quick = true;
                }
            },
        );
    });

    // ---------------------------------------------------------------- Default file manager
    fl_section(ui, m, pal, "Default file manager");
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
                ui.add(
                    egui::Slider::new(&mut sp.s.max_search_results, 1_usize..=10_000_000_usize)
                        .logarithmic(true),
                );
                if sp.s.max_search_results != 100_000_usize && ui.button("⟲").on_hover_text("Reset to default").clicked() {
                    sp.s.max_search_results = 100_000_usize;
                }
            },
        );
        fl_divider(ui, m, pal);
        fl_row(ui, m, pal, "Search threads", Some("Default: 4"), |ui| {
            ui.add(egui::Slider::new(&mut sp.s.search_threads, 1_usize..=64_usize));
            if sp.s.search_threads != 4_usize && ui.button("⟲").on_hover_text("Reset to default").clicked() {
                sp.s.search_threads = 4_usize;
            }
        });
    });

    // ---------------------------------------------------------------- Folder sizes
    fl_section(ui, m, pal, "Folder sizes");
    fl_card(ui, m, pal, |ui| {
        let loc = format!("Index location: {}", sp.index_dir);
        fl_row(ui, m, pal, "Index location", Some(loc.as_str()), |ui| {
            if secondary_button(ui, m, "Choose location…").clicked() {
                sp.choose = true;
            }
        });
        fl_divider(ui, m, pal);
        let cached = format!(
            "{} folders cached. Changes are picked up automatically while the app is open.",
            sp.cache_len
        );
        fl_row(ui, m, pal, "Rescan all folders", Some(cached.as_str()), |ui| {
            if secondary_button(ui, m, "Rescan all").clicked() {
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
                if danger_button(ui, m, "Delete index").clicked() {
                    sp.delete_index = true;
                }
            },
        );
        fl_divider(ui, m, pal);
        fl_row(ui, m, pal, "Cache size",  Some("Sets the maximum ram allowed for caching folder sizes.\n Default: 64MB"), |ui| {
            // The whole UI is sized from this value, so applying it mid-drag would move the slider
            // under the cursor. Preview in the handle, apply on release.
            ui.add_enabled_ui(!sp.s.unlimited_cache, |ui| {
                ui.add(
                    egui::Slider::new(&mut sp.s.index_ram, 1_usize..=10_000_usize)
                        .logarithmic(true)
                        .suffix("MB"),
                );
                if sp.s.index_ram != 64_usize && ui.button("⟲").on_hover_text("Reset to default").clicked() {
                    sp.s.index_ram = 64_usize;
                }
            });
        });
        fl_divider(ui, m, pal);
        fl_row(
            ui,
            m,
            pal,
            "Unlimited cache size",
            Some("Removes the above RAM-limit for loading the cached folder sizes."),
            |ui| fl_switch(ui, m, pal, &mut sp.s.unlimited_cache),
        );
    });

    // ---------------------------------------------------------------- Extras
    fl_section(ui, m, pal, "Extras");
    fl_card(ui, m, pal, |ui| {
        fl_row(
            ui,
            m,
            pal,
            "Git integration",
            Some("Shows the current branch and allows you to checkout local branches within git repositories. Changing this settings requires a restart."),
            |ui| fl_switch(ui, m, pal, &mut sp.s.git_activated),
        );
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
                if secondary_button(ui, m, "View licenses").clicked() {
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
        if let Some(i) = self.tabs.iter().position(|t| t.settings) {
            if i == self.active && self.tabs.len() > 1 {
                self.apply(Action::CloseTab(i));
            } else {
                self.activate_tab(i);
            }
            return;
        }
        let dir = self.tabs[self.active].dir.clone();
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        if let Some(tree) = self.tiles.as_mut() {
            // In tiled mode the new tab takes over the focused pane.
            let cur = self.tabs[self.active].id;
            tree.replace(cur, id);
        }
        let mut tab = Tab::new(id, dir);
        tab.settings = true;
        tab.load_gen = 1; // nothing to read: the page is not a folder listing
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        self.on_active_changed();
    }

    /// Applies what the Settings tab changed this frame (it works on a copy of the settings).
    pub(crate) fn finish_settings_page(&mut self, ui: &mut Ui, sp: SettingsPage) {
        let SettingsPage { old, s, font_edit, rescan, choose, delete_index, reset_quick, default_request, show_licenses, .. } = sp;
        self.font_edit = font_edit;
        if show_licenses {
            self.licenses_open = true;
        }
        if s != old {
            // Only the fields the page edits: window geometry, columns and the session belong to the app.
            let st = &mut self.settings;
            st.theme = s.theme;
            st.font_size = s.font_size;
            st.follow_text_scale = s.follow_text_scale;
            st.left_drag_space = s.left_drag_space;
            st.use_system_accent = s.use_system_accent;
            st.custom_accent = s.custom_accent;
            st.transparency_effect = s.transparency_effect;
            st.show_hidden = s.show_hidden;
            st.date_format = s.date_format;
            st.network_drives = s.network_drives;
            st.max_search_results = s.max_search_results;
            st.search_threads = s.search_threads;
            st.index_ram = s.index_ram;
            st.unlimited_cache = s.unlimited_cache;
            st.git_activated = s.git_activated;
            self.settings_dirty = true;
            if s.theme != old.theme {
                ui.set_theme(theme_pref(s.theme));
            }
            if s.show_hidden != old.show_hidden || s.date_format != old.date_format {
                self.reload_all();
            }
            if s.network_drives != old.network_drives {
                self.refresh_drives();
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
            self.indexer.clear();
            self.start_indexing(true);
        }
        if delete_index {
            self.indexer.delete_index(&self.index_dir);
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
                self.indexer.set_dir(&p);
                self.start_indexing(true);
                save_settings(&self.settings);
            }
        }
    }
}
