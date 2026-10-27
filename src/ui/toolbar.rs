//! Navigation bar, command bar, address bar and search box.

use crate::prelude::*;

pub(crate) fn crumbs(path: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let mut acc = PathBuf::new();
    for c in path.components() {
        acc.push(c.as_os_str());
        match c {
            Component::Prefix(_) | Component::CurDir | Component::ParentDir => {}
            Component::RootDir => out.push((acc.display().to_string(), acc.clone())),
            Component::Normal(n) => out.push((n.to_string_lossy().into_owned(), acc.clone())),
        }
    }
    if out.is_empty() {
        out.push((path.display().to_string(), path.to_path_buf()));
    }
    out
}

pub(crate) fn address_bar(
    ui: &mut Ui,
    m: &Metrics,
    pal: &Palette,
    tab: &mut Tab,
    width: f32,
    actions: &mut Vec<Action>,
) {
    let (pill, pill_resp) = ui.allocate_exact_size(vec2(width, m.ctl_h), Sense::click());
    let r = CornerRadius::same(m.radius);
    ui.painter().rect_filled(pill, r, pal.control);
    ui.painter().rect_stroke(
        pill,
        r,
        Stroke::new(1.0_f32, pal.control_stroke),
        StrokeKind::Inside,
    );
    let inner = pill.shrink2(vec2(m.pad * 0.4, 0.0));

    if tab.editing_addr {
        let id = Id::unique(("addr_edit", tab.id));
        let te = egui::TextEdit::singleline(&mut tab.address)
            .id(id)
            .frame(egui::Frame::NONE)
            .margin(Margin::symmetric(4, 0))
            .desired_width(inner.width())
            .align(Align2::LEFT_CENTER)
            .hint_text("Type a folder path and press Enter");

        //Layout shift fix: Using a child instead of put prevents
        //the manipulation of other values such as margins which were affected
        //by the inner puts smaller size.
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::left_to_right(Align::Center)),
        );
        let resp = child.add(te);
        if tab.addr_focus {
            resp.request_focus();
            select_range(ui, id, 0, tab.address.chars().count());
            tab.addr_focus = false;
        }
        ui.painter().line_segment(
            [
                pill.left_bottom() + vec2(m.accessibility_scale * 3.0, -1.0),
                pill.right_bottom() + vec2(-m.accessibility_scale * 3.0, -1.0),
            ],
            Stroke::new(2.0 * m.accessibility_scale, pal.accent),
        );
        if resp.lost_focus() {
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                actions.push(Action::GoToAddress(tab.address.clone()));
            }
            tab.editing_addr = false;
            tab.address = tab.shown_dir().display().to_string();
        }
        return;
    }

    if pill_resp.clicked() {
        tab.editing_addr = true;
        tab.addr_focus = true;
        tab.address = tab.shown_dir().display().to_string();
    }

    let segs = crumbs(&tab.shown_dir());
    let seg_pad = m.pad * 0.7;
    let chev = m.small_icon * 0.7;
    let sep_w = chev + m.accessibility_scale * 4.0;
    let more_w = m.ctl_h * 0.7;
    let widths: Vec<f32> = segs
        .iter()
        .map(|(l, _)| text_width(ui, l, m.font) + seg_pad * 2.0)
        .collect();

    // Always show the current folder; add parents from the right while they fit.
    let mut used = 0.0;
    let mut first = segs.len();
    for i in (0..segs.len()).rev() {
        let need = widths[i] + if i + 1 < segs.len() { sep_w } else { 0.0 };
        let reserve = if i > 0 { more_w + sep_w } else { 0.0 };
        if first < segs.len() && used + need + reserve > inner.width() {
            break;
        }
        used += need;
        first = i;
    }

    let top = inner.top() + m.accessibility_scale * 3.0;
    let h = inner.height() - m.accessibility_scale * 6.0;
    let mut x = inner.left();
    if first > 0 {
        let rect = Rect::from_min_size(pos2(x, top), vec2(more_w, h));
        let resp = ui.interact(rect, Id::unique(("crumb_more", tab.id)), Sense::click());
        hover_fill(ui, rect, &resp, pal, m);
        paint_text(ui, rect, "…", FontId::proportional(m.font), pal.text, false);
        if resp.clicked() {
            tab.editing_addr = true;
            tab.addr_focus = true;
            tab.address = tab.shown_dir().display().to_string();
        }
        x += more_w;
        draw_icon(
            ui,
            pos2(x + sep_w / 2.0, inner.center().y),
            chev,
            "chevron_right",
            pal.text_secondary,
        );
        x += sep_w;
    }
    for i in first..segs.len() {
        if i > first {
            draw_icon(
                ui,
                pos2(x + sep_w / 2.0, inner.center().y),
                chev,
                "chevron_right",
                pal.text_secondary,
            );
            x += sep_w;
        }
        let rect = Rect::from_min_size(
            pos2(x, top),
            vec2(widths[i].min((inner.right() - x).max(0.0)), h),
        );
        let resp = ui.interact(rect, Id::unique(("crumb", tab.id, i)), Sense::click());
        hover_fill(ui, rect, &resp, pal, m);
        paint_text(
            ui,
            rect.shrink2(vec2(seg_pad, 0.0)),
            &segs[i].0,
            FontId::proportional(m.font),
            pal.text,
            false,
        );
        if resp.clicked() {
            actions.push(Action::Navigate(segs[i].1.clone()));
        }
        x += rect.width();
    }
}

pub(crate) fn search_box(ui: &mut Ui, m: &Metrics, pal: &Palette, tab: &mut Tab, width: f32) {
    let (pill, _) = ui.allocate_exact_size(vec2(width, m.ctl_h), Sense::hover());
    let r = CornerRadius::same(m.radius);
    ui.painter().rect_filled(pill, r, pal.control);
    ui.painter().rect_stroke(
        pill,
        r,
        Stroke::new(1.0_f32, pal.control_stroke),
        StrokeKind::Inside,
    );

    // Right-most slot: scope dropdown (icon + chevron)
    let menu_w = m.small_icon + m.pad * 2.0;
    let menu_rect = Rect::from_min_max(
        pos2(pill.right() - menu_w, pill.top()),
        pill.right_bottom(),
    );
    // Slot to its left: clear button (only used when there's text)
    let clear_w = m.ctl_h * 0.8;
    let clear_rect = Rect::from_center_size(
        pos2(menu_rect.left() - clear_w / 2.0, pill.center().y),
        vec2(clear_w, clear_w),
    );
    // Always reserve both slots so the layout never shifts while typing
    let right_reserved = menu_w + clear_w;
    let te_rect = Rect::from_min_max(
        pill.min + vec2(m.pad * 0.4, 0.0),
        pill.max - vec2(right_reserved, 0.0),
    );
    let hint = match tab.chosen_search_scope {
        SearchScope::CurrentDir => format!("Search {}", tab.title()),
        SearchScope::System => "Search entire system".to_owned(),
    };
    let te = egui::TextEdit::singleline(&mut tab.search)
        .id(Id::unique("search_box"))
        .frame(egui::Frame::NONE)
        .margin(Margin::symmetric(4, 0))
        .desired_width(te_rect.width())
        .align(Align2::LEFT_CENTER)
        .hint_text(hint);
    let resp = ui.put(te_rect, te);
    if resp.changed() {
        tab.view_dirty = true;
    }
    if resp.has_focus() {
        ui.painter().line_segment(
            [
                pill.left_bottom() + vec2(m.accessibility_scale * 3.0, -1.0),
                pill.right_bottom() + vec2(-m.accessibility_scale * 3.0, -1.0),
            ],
            Stroke::new(2.0 * m.accessibility_scale, pal.accent),
        );
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            tab.search.clear();
            tab.view_dirty = true;
            resp.surrender_focus();
        }
    }

    // Clear button
    if !tab.search.is_empty() {
        let cr = ui.interact(clear_rect, Id::unique("search_clear"), Sense::click());
        hover_fill(ui, clear_rect, &cr, pal, m);
        draw_icon(ui, clear_rect.center(), m.small_icon * 0.8, "close", pal.text);
        if cr.clicked() {
            tab.search.clear();
            tab.view_dirty = true;
        }
    }

    // SearchScope dropdown menu
    let mr = ui.interact(menu_rect, Id::unique("search_scope_btn"), Sense::click());
    hover_fill(ui, menu_rect, &mr, pal, m);
    let icon_c = pos2(
        menu_rect.left() + m.pad * 0.5 + m.small_icon / 2.0,
        pill.center().y,
    );
    draw_icon(ui, icon_c, m.small_icon, "search", pal.text_secondary);

    // Small chevron to the right of the icon
    let cx = icon_c.x + m.small_icon / 2.0 + m.pad * 0.5;
    let cy = pill.center().y;
    let s = m.small_icon * 0.18;
    ui.painter().add(egui::Shape::line(
        vec![
            pos2(cx - s, cy - s * 0.5),
            pos2(cx, cy + s * 0.5),
            pos2(cx + s, cy - s * 0.5),
        ],
        Stroke::new(1.2_f32, pal.text_secondary),
    ));

    let mr = mr.on_hover_text(format!("Search scope: {}", tab.chosen_search_scope.label()));
    egui::Popup::menu(&mr)
        .align(egui::RectAlign::BOTTOM_END)
        .show(|ui| {
            ui.set_min_width(140.0);
            for scope in [SearchScope::CurrentDir, SearchScope::System] {
                if ui
                    .selectable_value(&mut tab.chosen_search_scope, scope, scope.label())
                    .clicked()
                {
                    tab.view_dirty = true; // re-run search with the new scope
                    ui.close();
                }
            }
        });
}

///static_address determines the look of the static, immutable address bar for the settings tab.
pub(crate) fn static_address(ui: &mut Ui, m: &Metrics, pal: &Palette, width: f32, icon: &str, label: &str) {
    let (pill, _) = ui.allocate_exact_size(vec2(width, m.ctl_h), Sense::hover());
    let r = CornerRadius::same(m.radius);
    ui.painter().rect_filled(pill, r, pal.control);
    ui.painter().rect_stroke(
        pill,
        r,
        Stroke::new(1.0_f32, pal.control_stroke),
        StrokeKind::Inside,
    );
    //Settings icon drawn in the settings adress bar
    if label == "Settings"{
        draw_icon(
            ui,
            pos2(pill.left() + m.pad + m.small_icon / 2.0, pill.center().y),
            m.small_icon,
            icon,
            pal.text_secondary,
        );
    }else{
        draw_icon(
            ui,
            pos2(pill.left() + m.pad + m.small_icon / 2.0, pill.center().y),
            m.small_icon,
            icon,
            Color32::WHITE,
        );
    }

    let tr = Rect::from_min_max(
        pos2(pill.left() + m.pad * 1.8 + m.small_icon, pill.top()),
        pill.max,
    );
    paint_text(ui, tr, label, FontId::proportional(m.font), pal.text, false);
}

impl Explorer {
    //ui_nav represents the navbar found below the tabs.
    pub(crate) fn ui_nav(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let (m, pal) = (self.m, self.pal);
        let nw = (((m.radius + 2) as f32) * (1.0 - self.first_anim)).round() as u8;
        let tab = &mut self.tabs[self.active];
        let (can_back, can_fwd, can_up) = (
            !tab.back.is_empty(),
            !tab.fwd.is_empty(),
            !tab.special() && tab.dir.parent().is_some(),
        );
        egui::Panel::top("nav")
            .show_separator_line(false)
            .frame(band_frame(&pal, nw, m.pad, m.pad, m.pad * 0.8, m.pad * 0.2))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = m.accessibility_scale * 2.0;
                    if icon_button(ui, &m, &pal, "back", "Back (Alt+←)", can_back, false).clicked()
                    {
                        actions.push(Action::Back);
                    }
                    if icon_button(ui, &m, &pal, "forward", "Forward (Alt+→)", can_fwd, false)
                        .clicked()
                    {
                        actions.push(Action::Forward);
                    }
                    if icon_button(ui, &m, &pal, "up", "Up (Alt+↑)", can_up, false).clicked() {
                        actions.push(Action::Up);
                    }
                    if icon_button(ui, &m, &pal, "refresh", "Refresh (F5)", true, false).clicked() {
                        actions.push(Action::Refresh);
                    }
                    if tab.settings {
                        ui.add_space(m.pad * 0.5);
                        let w = ui.available_width().max(m.accessibility_scale * 120.0);
                        static_address(ui, &m, &pal, w, "settings", "Settings");
                        return;
                    }
                    ui.add_space(m.pad * 0.5);
                    let gap = ui.spacing().item_spacing.x + m.pad * 0.5;
                    let search_w = (m.accessibility_scale * 260.0).min(ui.available_width() * 0.38);
                    let addr_w = (ui.available_width() - search_w - gap).max(m.accessibility_scale * 120.0);
                    if tab.trash.is_some() {
                        static_address(ui, &m, &pal, addr_w, "recycle_bin", "Recycle Bin");
                    } else {
                        address_bar(ui, &m, &pal, tab, addr_w, actions);
                    }
                    ui.add_space(m.pad * 0.5);
                    search_box(ui, &m, &pal, tab, search_w);
                });
            });
    }

    //ui_command is the bottom section of the navbar, where you can find quick commands and the settings button
    pub(crate) fn ui_command(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let (m, pal) = (self.m, self.pal);
        let (in_trash, trash_sel, trash_any) = match &self.tabs[self.active].trash {
            Some(t) => (true, t.selected.len(), !t.rows.is_empty()),
            None => (false, 0, false),
        };
        let selection = self.tabs[self.active].selected_paths();
        let (can_paste, hidden, settings_open) = (
            get_clipboard().is_some(),
            self.settings.show_hidden,
            self.tabs[self.active].settings,
        );
        // Checks for the existence of a git repository.
        if let Some(git) = &mut self.git {
            git.update(ui.ctx(), &self.tabs[self.active].dir);
        }
        egui::Panel::top("command")
            .show_separator_line(false)
            .frame(layer_frame(
                &pal,
                m.radius,
                m.pad,
                m.pad,
                0.0,
                m.pad * 0.3,
                false,
            ))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = m.accessibility_scale * 2.0;
                    if settings_open {
                        // Remove all file interaction buttons for settings tab.
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if icon_button(ui, &m, &pal, "settings", "Close Settings", true, true).clicked() {
                                actions.push(Action::OpenSettings);
                            }
                        });
                        return;
                    }
                    if in_trash {
                        if cmd_button(
                            ui,
                            &m,
                            &pal,
                            "restore",
                            Some("Restore"),
                            "Restore the selected items",
                            trash_sel > 0,
                            false,
                        )
                        .clicked()
                        {
                            actions.push(Action::TrashRestoreSelected);
                        }
                        if cmd_button(
                            ui,
                            &m,
                            &pal,
                            "delete",
                            Some("Delete permanently"),
                            "Delete the selected items permanently (Del)",
                            trash_sel > 0,
                            false,
                        )
                        .clicked()
                        {
                            actions.push(Action::TrashPurgeSelected);
                        }
                        vsep(ui, &m, &pal);
                        if cmd_button(
                            ui,
                            &m,
                            &pal,
                            "empty_bin",
                            Some("Empty Recycle Bin"),
                            "Permanently delete everything in the Recycle Bin",
                            trash_any,
                            false,
                        )
                        .clicked()
                        {
                            actions.push(Action::TrashEmpty);
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if icon_button(
                                ui,
                                &m,
                                &pal,
                                "settings",
                                "Settings",
                                true,
                                settings_open,
                            )
                            .clicked()
                            {
                                actions.push(Action::OpenSettings);
                            }
                        });
                        return;
                    }
                    let wide = ui.available_width() > m.accessibility_scale * 640.0;
                    if cmd_button(
                        ui,
                        &m,
                        &pal,
                        "new_folder",
                        Some("New folder"),
                        "New folder (Ctrl+Shift+N)",
                        true,
                        false,
                    )
                    .clicked()
                    {
                        actions.push(Action::NewFolder);
                    }
                    vsep(ui, &m, &pal);
                    if !selection.is_empty() {
                        if icon_button(ui, &m, &pal, "cut", "Cut (Ctrl+X)", true, false).clicked() {
                            actions.push(Action::Cut(selection.clone()));
                        }
                        if icon_button(ui, &m, &pal, "copy", "Copy (Ctrl+C)", true, false).clicked()
                        {
                            actions.push(Action::Copy(selection.clone()));
                        }
                    } else {
                        icon_button(ui, &m, &pal, "cut", "Cut (Ctrl+X)", false, false);
                        icon_button(ui, &m, &pal, "copy", "Copy (Ctrl+C)", false, false);
                    }
                    if icon_button(ui, &m, &pal, "paste", "Paste (Ctrl+V)", can_paste, false)
                        .clicked()
                    {
                        actions.push(Action::Paste);
                    }
                    if !selection.is_empty() {
                        if icon_button(ui, &m, &pal, "rename", "Rename (F2)", selection.len() == 1, false)
                            .clicked()
                        {
                            actions.push(Action::Rename(selection[0].clone()));
                        }
                        if icon_button(ui, &m, &pal, "delete", "Delete (Del)", true, false)
                            .clicked()
                        {
                            actions.push(Action::Delete(selection.clone()));
                        }
                    } else {
                        icon_button(ui, &m, &pal, "rename", "Rename (F2)", false, false);
                        icon_button(ui, &m, &pal, "delete", "Delete (Del)", false, false);
                    }
                    vsep(ui, &m, &pal);
                    if cmd_button(
                        ui,
                        &m,
                        &pal,
                        "eye",
                        if wide { Some("Hidden items") } else { None },
                        "Show hidden items",
                        true,
                        hidden,
                    )
                    .clicked()
                    {
                        actions.push(Action::ToggleHidden);
                    }
                    // Display git toolbar options if in a git repository.
                    let mut picked: Option<String> = None;
                    if let Some(git) = &self.git {
                        if let Some(snap) = &git.snapshot {
                            let (full, current): (String, Option<&str>) = match &snap.head {
                                Some(Head::Branch(n)) => (n.clone(), Some(n.as_str())),
                                Some(Head::Detached(h)) => {
                                    (format!("detached @ {}", &h[..h.len().min(7)]), None)
                                }
                                None => ("no HEAD".to_owned(), None),
                            };
                            vsep(ui, &m, &pal);
                            cmd_dropdown(
                                ui,
                                &m,
                                &pal,
                                "git_branch",
                                if wide { Some(full.as_str()) } else { None },
                                &format!("Branch: {full}"),
                                !git.busy(),
                                m.accessibility_scale * 140.0, // max label width before "…"
                                |ui| {
                                    egui::ScrollArea::vertical()
                                        .max_height(m.accessibility_scale * 320.0)
                                        .show(ui, |ui| {
                                            for b in &snap.branches {
                                                let is_cur = Some(b.name.as_str()) == current;
                                                let r = ui
                                                    .add(egui::Button::selectable(is_cur, &b.name).truncate())
                                                    .on_hover_text(&b.name);
                                                if r.clicked() && !is_cur {
                                                    picked = Some(b.name.clone());
                                                }
                                            }
                                        });
                                },
                            );
                            /*
                            if !selection.is_empty() {
                                if icon_button(ui, &m, &pal, "git_add", "add to staging", true, false).clicked() {
                                    actions.push(Action::GitAdd(selection.clone()));
                                }
                            }
                            if snap.staged_files > 0 {
                                if icon_button(ui, &m, &pal, "git_commit", "commit staged changes", true, false).clicked() {
                                    actions.push(Action::GitCommit(snap.root.clone()));
                                }
                            }*/
                        }
                    }
                    // Outside the block due to borrowing logic, refreshes the folder contents after
                    // switching branches.
                    if let Some(git) = &mut self.git {
                        if let Some(name) = picked {
                            git.git_async(ui.ctx(), GitCommand::Checkout {branch: name});
                        }
                        if std::mem::take(&mut git.switched) {
                            actions.push(Action::Refresh);
                        }

                        git.update(ui.ctx(), &*self.tabs[self.active].dir);

                        // Push error out to the error display.
                        if let Some(err) = git.error.take() {
                            self.tabs[self.active].git_error = Some(err);
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(ui, &m, &pal, "settings", "Settings", true, settings_open)
                            .clicked()
                        {
                            actions.push(Action::OpenSettings);
                        }
                    });
                });
            });
    }
}
