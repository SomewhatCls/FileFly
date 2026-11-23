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

/// The state a breadcrumb address bar edits: the text while typing, whether it is being edited, and
/// whether the text is to be selected on the next frame. A tab and the folder picker each own one.
pub(crate) struct AddrState<'a> {
    pub(crate) text: &'a mut String,
    pub(crate) editing: &'a mut bool,
    pub(crate) focus: &'a mut bool,
}

/// What a breadcrumb address bar reports.
pub(crate) enum AddrEvent {
    /// A folder was picked: a breadcrumb, or an entry of the "…" list.
    Go(PathBuf),
    /// Enter was pressed in the edit field.
    Submit(String),
    /// The edit field was left without Enter (Esc, or a click elsewhere).
    Cancelled,
}

/// The buttons at the left of a navigation row.
pub(crate) enum NavCmd {
    Back,
    Forward,
    Up,
    Refresh,
}

/// Back / Forward / Up / Refresh. Used by the explorer's navigation row and the folder picker's.
pub(crate) fn nav_buttons(
    ui: &mut Ui,
    m: &Metrics,
    pal: &Palette,
    can_back: bool,
    can_fwd: bool,
    can_up: bool,
) -> Option<NavCmd> {
    let mut cmd = None;
    if icon_button(ui, m, pal, "back", "Back (Alt+←)", can_back, false).clicked() {
        cmd = Some(NavCmd::Back);
    }
    if icon_button(ui, m, pal, "forward", "Forward (Alt+→)", can_fwd, false).clicked() {
        cmd = Some(NavCmd::Forward);
    }
    if icon_button(ui, m, pal, "up", "Up (Alt+↑)", can_up, false).clicked() {
        cmd = Some(NavCmd::Up);
    }
    if icon_button(ui, m, pal, "refresh", "Refresh (F5)", true, false).clicked() {
        cmd = Some(NavCmd::Refresh);
    }
    cmd
}

/// (address bar width, search box width) for what is left of a navigation row after its buttons.
/// `search_base` is the search box's width at normal scale.
pub(crate) fn nav_widths(ui: &Ui, m: &Metrics, search_base: f32) -> (f32, f32) {
    let gap = ui.spacing().item_spacing.x + m.pad * 0.5;
    let search_w = (m.accessibility_scale * search_base).min(ui.available_width() * 0.38);
    let addr_w = (ui.available_width() - search_w - gap).max(m.accessibility_scale * 120.0);
    (addr_w, search_w)
}

pub(crate) fn address_bar(
    ui: &mut Ui,
    m: &Metrics,
    pal: &Palette,
    tab: &mut Tab,
    width: f32,
    actions: &mut Vec<Action>,
) {
    let dir = tab.shown_dir().to_path_buf();
    let state = AddrState { text: &mut tab.address, editing: &mut tab.editing_addr, focus: &mut tab.addr_focus };
    match breadcrumb_bar(ui, m, pal, Id::unique(("addr_edit", tab.id)), &dir, state, width) {
        Some(AddrEvent::Go(path)) => actions.push(Action::Navigate(path)),
        Some(AddrEvent::Submit(text)) => actions.push(Action::GoToAddress(text)),
        Some(AddrEvent::Cancelled) | None => {}
    }
}

/// Breadcrumb address bar showing `dir`: click a folder to go there, the "…" for the folders that
/// do not fit, the empty part to type a path. `id` is the id of the edit field (other code may want
/// to focus it); the other widgets of the bar derive their ids from it.
pub(crate) fn breadcrumb_bar(
    ui: &mut Ui,
    m: &Metrics,
    pal: &Palette,
    id: Id,
    dir: &Path,
    st: AddrState<'_>,
    width: f32,
) -> Option<AddrEvent> {
    let (pill, pill_resp) = ui.allocate_exact_size(vec2(width, m.ctl_h), Sense::click());
    let r = CornerRadius::same(m.radius);
    ui.painter().rect_filled(pill, r, pal.control);
    ui.painter().rect_stroke(
        pill,
        r,
        Stroke::new(1.0_f32, pal.control_stroke),
        StrokeKind::Inside,
    );
    // The empty parts of the bar behave like a text field (click to type a path), so they show the
    // text cursor. The breadcrumbs and the "…" button sit on top of the bar and keep the arrow.
    let pill_resp = pill_resp.on_hover_cursor(egui::CursorIcon::Text);
    let inner = pill.shrink2(vec2(m.pad * 0.4, 0.0));

    if *st.editing {
        let te = egui::TextEdit::singleline(&mut *st.text)
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
                .layout(Layout::left_to_right(Align::Center)),
        );
        let resp = child.add(te);
        if *st.focus {
            resp.request_focus();
            select_range(ui, id, 0, st.text.chars().count());
            *st.focus = false;
        }
        ui.painter().line_segment(
            [
                pill.left_bottom() + vec2(m.accessibility_scale * 3.0, -1.0),
                pill.right_bottom() + vec2(-m.accessibility_scale * 3.0, -1.0),
            ],
            Stroke::new(2.0 * m.accessibility_scale, pal.accent),
        );
        if resp.lost_focus() {
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            let event = if enter { AddrEvent::Submit(st.text.clone()) } else { AddrEvent::Cancelled };
            *st.editing = false;
            *st.text = dir.display().to_string();
            return Some(event);
        }
        return None;
    }

    if pill_resp.clicked() {
        *st.editing = true;
        *st.focus = true;
        *st.text = dir.display().to_string();
    }

    let mut out = None;
    let segs = crumbs(dir);
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
        let resp = ui.interact(rect, id.with("crumb_more"), Sense::click());
        hover_fill(ui, rect, &resp, pal, m);
        // Three dots drawn by hand, centred in the button both ways (the "…" character sits on the
        // text baseline and is not centred in its box).
        let (dot, step) = ((m.font * 0.085).max(1.0), (m.font * 0.085).max(1.0) * 3.6);
        for dx in [-step, 0.0, step] {
            ui.painter().circle_filled(pos2(rect.center().x + dx, rect.center().y), dot, pal.text);
        }
        // The folders that do not fit, closest first (like the overflow list of Windows Explorer).
        // The rest of the bar still edits the address when clicked.
        egui::Popup::menu(&resp.on_hover_text("Earlier folders"))
            .align(egui::RectAlign::BOTTOM_START)
            .show(|ui| {
                // As wide as the widest entry, with the same padding left of the icon and right of
                // the text (and not wider than a sensible maximum).
                let (icon, gap) = (m.icon, m.pad * 0.6);
                let widest = segs[..first].iter().map(|(l, _)| text_width(ui, l, m.font)).fold(0.0_f32, f32::max);
                let w = (m.pad + icon + gap + widest + m.pad).min(m.accessibility_scale * 520.0);
                ui.set_width(w);
                for i in (0..first).rev() {
                    let (row, resp) = ui.allocate_exact_size(vec2(w, m.row_h), Sense::click());
                    hover_fill(ui, row, &resp, pal, m);
                    draw_icon(ui, pos2(row.left() + m.pad + icon / 2.0, row.center().y), icon, "folder", Color32::WHITE);
                    let text = Rect::from_min_max(pos2(row.left() + m.pad + icon + gap, row.top()), pos2(row.right() - m.pad, row.bottom()));
                    paint_text(ui, text, &segs[i].0, FontId::proportional(m.font), pal.text, false);
                    if resp.clicked() {
                        out = Some(AddrEvent::Go(segs[i].1.clone()));
                        ui.close();
                    }
                }
            });
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
        let resp = ui.interact(rect, id.with(("crumb", i)), Sense::click());
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
            out = Some(AddrEvent::Go(segs[i].1.clone()));
        }
        x += rect.width();
    }
    out
}

pub(crate) fn search_box(ui: &mut Ui, m: &Metrics, pal: &Palette, tab: &mut Tab, width: f32) {
    let hint = match tab.chosen_search_scope {
        SearchScope::CurrentDir => format!("Search {}", tab.title()),
        SearchScope::System => "Search entire system".to_owned(),
    };
    if search_pill(ui, m, pal, Id::unique("search_box"), &mut tab.search, hint, None, Some(&mut tab.chosen_search_scope), false, width) {
        tab.view_dirty = true; // text or scope changed: run the search again
    }
}

/// Search box: text field, clear button and, at the right end, either a plain magnifier or (with
/// `scope`) the magnifier as a menu button that picks where to search. Returns true when the search
/// has to run again (text cleared or edited, scope switched). `eat_escape` makes Esc inside the
/// field not reach anything else (a dialog would otherwise close). `id` is the id of the text field
/// (other code may want to focus it); the buttons derive their ids from it.
pub(crate) fn search_pill(
    ui: &mut Ui,
    m: &Metrics,
    pal: &Palette,
    id: Id,
    text: &mut String,
    hint: String,
    tooltip: Option<&str>,
    scope: Option<&mut SearchScope>,
    eat_escape: bool,
    width: f32,
) -> bool {
    let (pill, _) = ui.allocate_exact_size(vec2(width, m.ctl_h), Sense::hover());
    let r = CornerRadius::same(m.radius);
    ui.painter().rect_filled(pill, r, pal.control);
    ui.painter().rect_stroke(
        pill,
        r,
        Stroke::new(1.0_f32, pal.control_stroke),
        StrokeKind::Inside,
    );

    // Right-most slot: the magnifier (a menu button with a chevron when there is a scope to pick)
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
    let te = egui::TextEdit::singleline(&mut *text)
        .id(id)
        .frame(egui::Frame::NONE)
        .margin(Margin::symmetric(4, 0))
        .desired_width(te_rect.width())
        .align(Align2::LEFT_CENTER)
        .hint_text(hint);
    let mut resp = ui.put(te_rect, te);
    if let Some(tip) = tooltip {
        resp = resp.on_hover_text(tip);
    }
    let mut changed = resp.changed();

    // Fluent-style underline on focus.
    if resp.has_focus() {
        ui.painter().line_segment(
            [
                pill.left_bottom() + vec2(m.accessibility_scale * 3.0, -1.0),
                pill.right_bottom() + vec2(-m.accessibility_scale * 3.0, -1.0),
            ],
            Stroke::new(2.0 * m.accessibility_scale, pal.accent),
        );
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            if eat_escape {
                ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
            }
            if !text.is_empty() {
                text.clear();
                changed = true;
            }
            resp.surrender_focus();
        }
    }

    // Clear button
    if !text.is_empty() {
        let cr = ui.interact(clear_rect, id.with("clear"), Sense::click());
        hover_fill(ui, clear_rect, &cr, pal, m);
        draw_icon(ui, clear_rect.center(), m.small_icon * 0.8, "close", pal.text);
        if cr.clicked() {
            text.clear();
            changed = true;
        }
    }

    let Some(scope) = scope else {
        draw_icon(ui, pos2(menu_rect.center().x, pill.center().y), m.small_icon, "search", pal.text_secondary);
        return changed;
    };

    // Scope dropdown menu
    let mr = ui.interact(menu_rect, id.with("scope"), Sense::click());
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

    let mr = mr.on_hover_text(format!("Search scope: {}", scope.label()));
    egui::Popup::menu(&mr)
        .align(egui::RectAlign::BOTTOM_END)
        .show(|ui| {
            ui.set_min_width(140.0);
            for choice in [SearchScope::CurrentDir, SearchScope::System] {
                if ui.selectable_value(&mut *scope, choice, choice.label()).clicked() {
                    changed = true; // re-run the search with the new scope
                    ui.close();
                }
            }
        });
    changed
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
                    match nav_buttons(ui, &m, &pal, can_back, can_fwd, can_up) {
                        Some(NavCmd::Back) => actions.push(Action::Back),
                        Some(NavCmd::Forward) => actions.push(Action::Forward),
                        Some(NavCmd::Up) => actions.push(Action::Up),
                        Some(NavCmd::Refresh) => actions.push(Action::Refresh),
                        None => {}
                    }
                    if let Some(page) = tab.page {
                        ui.add_space(m.pad * 0.5);
                        let w = ui.available_width().max(m.accessibility_scale * 120.0);
                        static_address(ui, &m, &pal, w, page.icon(), page.title());
                        return;
                    }
                    ui.add_space(m.pad * 0.5);
                    let (addr_w, search_w) = nav_widths(ui, &m, 260.0);
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
        let in_licenses = self.tabs[self.active].page == Some(Page::Licenses);
        let can_paste = self.can_paste();
        let selection = self.tabs[self.active].selected_paths();
        let (can_paste, hidden, settings_open) = (
            can_paste,
            self.settings.show_hidden,
            self.tabs[self.active].page == Some(Page::Settings),
        );
        // The button stays unless it is turned off in the settings; its dot marks release notes not read yet.
        let whats_new_visible = self.settings.show_whats_new;
        let whats_new_unread = self.settings.whats_new_seen != env!("CARGO_PKG_VERSION");
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
                    if in_trash {
                        if Btn::new().icon("restore").label("Restore").tip("Restore the selected items").enabled(trash_sel > 0).show(ui, &m, &pal)
                        .clicked()
                        {
                            actions.push(Action::TrashRestoreSelected);
                        }
                        if Btn::new().icon("delete").label("Delete permanently").tip("Delete the selected items permanently (Del)").enabled(trash_sel > 0).show(ui, &m, &pal)
                        .clicked()
                        {
                            actions.push(Action::TrashPurgeSelected);
                        }
                        vsep(ui, &m, &pal);
                        if Btn::new().icon("empty_bin").label("Empty Recycle Bin").tip("Permanently delete everything in the Recycle Bin").enabled(trash_any).show(ui, &m, &pal)
                        .clicked()
                        {
                            actions.push(Action::TrashEmpty);
                        }
                    }
                    if !in_trash && !in_licenses && !settings_open {
                        let wide = ui.available_width() > m.accessibility_scale * 640.0;
                        if Btn::new().icon("new_folder").label("New folder").tip("New folder (Ctrl+Shift+F)").show(ui, &m, &pal)
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
                        if {
                            let b = Btn::new().icon("eye").tip("Show hidden items").toggled(hidden);
                            if wide { b.label("Hidden items") } else { b }
                        }.show(ui, &m, &pal)
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
                                git.git_async(ui.ctx(), GitCommand::Checkout { branch: name });
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
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if settings_open{
                            if icon_button(ui, &m, &pal, "settings", "Close Settings", true, true)
                                .clicked()
                            {
                                actions.push(Action::OpenSettings);
                            }
                        } else {
                            if icon_button(ui, &m, &pal, "settings", "Settings", true, settings_open)
                                .clicked()
                            {
                                actions.push(Action::OpenSettings);
                            }
                        }
                        if whats_new_visible {
                            let resp = icon_button(ui, &m, &pal, "whats_new", "What's new", true, false);
                            if whats_new_unread {
                                let dot = resp.rect.center() + vec2(m.icon * 0.42, -m.icon * 0.42);
                                ui.painter().circle_filled(dot, m.font * 0.18, pal.accent);
                            }
                            if resp.clicked() {
                                actions.push(Action::OpenWhatsNew);
                            }
                        }
                    });
                });
            });
    }
}
