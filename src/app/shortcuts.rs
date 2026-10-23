//! Keyboard shortcuts.

use crate::prelude::*;

impl Explorer {
    pub(crate) fn collect_shortcuts(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        use egui::{Key, Modifiers as M};
        let typing = ui.egui_wants_keyboard_input();
        let modal = self.props.is_some()
            || self.conflict.is_some()
            || self.confirm.is_some()
            || self.picker.is_some()
            || self.licenses_open;

        let in_trash = self.tabs[self.active].trash.is_some();
        let in_settings = self.tabs[self.active].settings;
        let sel: Vec<PathBuf> = if in_trash || in_settings {
            Vec::new()
        } else {
            self.tabs[self.active].selected_paths()
        };

        let active = self.active;
        let key_down = self.paste_key_down();
        let paste_edge = key_down && !self.prev_paste_key.get();
        self.prev_paste_key.set(key_down);

        //Keyboard Shortcuts
        let mut split = false;
        let mut close_active = false;
        ui.input_mut(|i| {
            if i.consume_key(M::CTRL | M::SHIFT, Key::Tab) {
                actions.push(Action::StepTab(-1));
            } else if i.consume_key(M::CTRL, Key::Tab) {
                actions.push(Action::StepTab(1));
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
            if i.consume_key(M::CTRL, Key::F) || i.consume_key(M::CTRL, Key::E) {
                self.tabs[active].chosen_search_scope = SearchScope::CurrentDir;
                actions.push(Action::FocusSearch);
            }
            if i.consume_key(M::CTRL, Key::G) || i.consume_key(M::CTRL, Key::E) {
                self.tabs[active].chosen_search_scope = SearchScope::System;
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
            if i.consume_key(M::CTRL | M::SHIFT, Key::F) {
                actions.push(Action::NewFolder);
            }
            if i.consume_key(M::CTRL | M::SHIFT, Key::N) {
                actions.push(Action::NewFile);
            }
            if i.consume_key(M::CTRL, Key::Comma) {
                actions.push(Action::OpenSettings);
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
            if !in_trash && i.consume_key(M::CTRL, Key::A) {
                actions.push(Action::SelectAll);
            }
            let ev_copy = i.events.iter().any(|e| matches!(e, egui::Event::Copy));
            let ev_cut = i.events.iter().any(|e| matches!(e, egui::Event::Cut));
            let ev_paste = i.events.iter().any(|e| matches!(e, egui::Event::Paste(_)));
            if !sel.is_empty() {
                if ev_copy || (i.modifiers.command && i.key_pressed(Key::C)) {
                    actions.push(Action::Copy(sel.clone()));
                }
                if ev_cut || (i.modifiers.command && i.key_pressed(Key::X)) {
                    actions.push(Action::Cut(sel.clone()));
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
                if i.consume_key(M::ALT, Key::Enter) {
                    actions.push(Action::Properties(sel[0].clone()));
                }
            }
            if (ev_paste || (paste_edge && i.focused))
                && self.last_paste.get().elapsed() > Duration::from_millis(300)
            {
                self.last_paste.set(Instant::now());
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
            if self.tabs.len() > 1 {
                let tab_id = self.tabs[active].id;
                let closing_id = Id::unique("tabs_closing");
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
