//! Keyboard shortcuts.

use crate::prelude::*;

impl Explorer {
    pub(crate) fn collect_shortcuts(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        use egui::{Key, Modifiers as M};
        let typing = ui.egui_wants_keyboard_input();
        let modal = self.core.conflict.is_some()
            || self.core.restore_conflict.is_some()
            || self.win.confirm.is_some()
            || self.win.picker.is_some()
            || self.win.whats_new.is_some();

        let in_trash = self.win.tabs[self.win.active].trash.is_some();
        let in_settings = self.win.tabs[self.win.active].page.is_some();
        let sel: Vec<PathBuf> = if in_trash || in_settings {
            Vec::new()
        } else {
            self.win.tabs[self.win.active].selected_paths()
        };

        let active = self.win.active;
        let key_down = self.paste_key_down();
        let paste_edge = key_down && !self.win.prev_paste_key.get();
        self.win.prev_paste_key.set(key_down);

        //Keyboard Shortcuts
        let mut split = false;
        let mut close_active = false;
        ui.input_mut(|i| {
            // Always keep more specific shortcuts above more generic ones to prevent
            // supersets from consuming their actions.
            if i.consume_key(M::CTRL | M::SHIFT, Key::Tab) {
                actions.push(Action::StepTab(-1));
            } else if i.consume_key(M::CTRL, Key::Tab) {
                actions.push(Action::StepTab(1));
            }
            if i.consume_key(M::CTRL | M::SHIFT, Key::F) {
                actions.push(Action::NewFile);
            }
            if i.consume_key(M::CTRL | M::SHIFT, Key::N) {
                actions.push(Action::NewFolder);
            }
            if i.consume_key(M::COMMAND, Key::N) {
                actions.push(Action::NewWindow(None, None));
            }
            if i.consume_key(M::CTRL, Key::T) {
                actions.push(Action::NewTab);
            }
            if i.consume_key(M::CTRL, Key::W) {
                close_active = true;
            }
            if i.consume_key(M::CTRL, Key::S) && !modal {
                // Handled after this closure: splitting touches egui state, which must not happen
                // while the input is locked.
                split = true;
            }
            if i.consume_key(M::CTRL, Key::L) || i.consume_key(M::ALT, Key::D) {
                actions.push(Action::FocusAddress);
            }
            if i.consume_key(M::CTRL, Key::F) {
                self.win.tabs[active].chosen_search_scope = SearchScope::CurrentDir;
                actions.push(Action::FocusSearch);
            }
            if i.consume_key(M::CTRL, Key::G) {
                self.win.tabs[active].chosen_search_scope = SearchScope::System;
                actions.push(Action::FocusSearch);
            }
            if i.consume_key(M::NONE, Key::F5) {
                actions.push(Action::Refresh);
            }
            if i.consume_key(M::ALT, Key::ArrowLeft) {
                actions.push(Action::Back);
            }
            if i.consume_key(M::ALT, Key::ArrowRight) {
                actions.push(Action::Forward);
            }
            if i.consume_key(M::ALT, Key::ArrowUp) {
                actions.push(Action::Up);
            }
            // Mouse "back"/"forward" buttons: no need to travel to the toolbar.
            if i.pointer.button_pressed(egui::PointerButton::Extra1) {
                actions.push(Action::Back);
            }
            if i.pointer.button_pressed(egui::PointerButton::Extra2) {
                actions.push(Action::Forward);
            }
            if i.consume_key(M::CTRL, Key::Comma) {
                actions.push(Action::OpenPage(Page::Settings));
            }

            if typing || modal || in_settings {
                return;
            }
            if in_trash {
                if i.consume_key(M::NONE, Key::Delete) {
                    actions.push(Action::TrashPurgeSelected);
                }
                if i.consume_key(M::CTRL, Key::A) {
                    actions.push(Action::TrashSelectAll);
                }
            }
            // Typing (no shortcut) selects by name.
            if !in_trash && !i.modifiers.command && !i.modifiers.alt {
                let typed: String = i
                    .events
                    .iter()
                    .filter_map(|e| if let egui::Event::Text(t) = e { Some(t.as_str()) } else { None })
                    .collect();
                if !typed.is_empty() {
                    actions.push(Action::TypeAhead(typed));
                }
            }
            if !in_trash && i.consume_key(M::CTRL, Key::A) {
                actions.push(Action::SelectAll);
            }
            let ev_copy = i.events.iter().any(|e| matches!(e, egui::Event::Copy));
            let ev_cut = i.events.iter().any(|e| matches!(e, egui::Event::Cut));
            let ev_paste = i.events.iter().any(|e| matches!(e, egui::Event::Paste(_)));
            // On Windows egui-winit reports Shift+Delete as Event::Cut. That is the permanent delete,
            // not a cut. (Ctrl+Shift+X is a real cut: it has `command` set.)
            let ev_delete_permanently = cfg!(windows) && ev_cut && i.modifiers.shift && !i.modifiers.command;
            if !sel.is_empty() {
                if ev_copy || (i.modifiers.command && i.key_pressed(Key::C)) {
                    actions.push(Action::Copy(sel.clone()));
                }
                if (ev_cut && !ev_delete_permanently) || (i.modifiers.command && i.key_pressed(Key::X)) {
                    actions.push(Action::Cut(sel.clone()));
                }
                // Before plain Enter, which would otherwise take Alt+Enter as well.
                if i.consume_key(M::ALT, Key::Enter) {
                    actions.push(Action::Properties(sel[0].clone()));
                }
                // So that permanent delete isnt captured by moving to recycle bin.
                if ev_delete_permanently || i.consume_key(M::SHIFT, Key::Delete) {
                    actions.push(Action::DeletePermanently(sel.clone()));
                }
                if i.consume_key(M::NONE, Key::Enter) {
                    if sel.len() == 1 {
                        actions.push(Action::Open(sel[0].clone()));
                    } else {
                        // With several items selected, Enter opens the files (not the folders).
                        for p in sel.iter().filter(|p| !p.is_dir()).take(20) {
                            actions.push(Action::Open(p.clone()));
                        }
                    }
                }
                if sel.len() == 1 && i.consume_key(M::NONE, Key::F2) {
                    actions.push(Action::Rename(sel[0].clone()));
                }
                if i.consume_key(M::NONE, Key::Delete) {
                    actions.push(Action::Delete(sel.clone()));
                }
            }
            if (ev_paste || (paste_edge && i.focused))
                && self.win.last_paste.get().elapsed() > Duration::from_millis(300)
            {
                self.win.last_paste.set(Instant::now());
                actions.push(Action::Paste);
            }
            if i.consume_key(M::NONE, Key::Backspace) {
                actions.push(Action::Back);
            }
            if i.consume_key(M::NONE, Key::ArrowDown) {
                actions.push(Action::MoveSelection(1));
            }
            if i.consume_key(M::NONE, Key::ArrowUp) {
                actions.push(Action::MoveSelection(-1));
            }
            if i.consume_key(M::NONE, Key::PageDown) {
                actions.push(Action::MoveSelection(10));
            }
            if i.consume_key(M::NONE, Key::PageUp) {
                actions.push(Action::MoveSelection(-10));
            }
            if i.consume_key(M::NONE, Key::Home) {
                actions.push(Action::SelectEdge(false));
            }
            if i.consume_key(M::NONE, Key::End) {
                actions.push(Action::SelectEdge(true));
            }
        });
        if split {
            self.split_focused();
        }

        // Adds smooth closing animation to ctrl+w close tab shortcut.
        if close_active {
            if self.win.tabs.len() > 1 {
                let tab_id = self.win.tabs[active].id;
                let closing_id = win_id("tabs_closing");
                ui.data_mut(|d| {
                    let mut v: Vec<u64> = d.get_temp(closing_id).unwrap_or_default();
                    if !v.contains(&tab_id) {
                        v.push(tab_id);
                    }
                    d.insert_temp(closing_id, v);
                });
                ui.request_repaint();
            } else {
                actions.push(Action::CloseTab(active)); // last tab: nothing to animate
            }
        }
    }

    // ------------------------------------------------------------------ actions
}
