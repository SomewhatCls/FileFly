//! Drag and drop of files: inside the app, out of the window and in from outside.

use crate::prelude::*;

/// 1x1 transparent PNG used as the OS drag preview (the in-window ghost is drawn by egui itself).
#[cfg(target_os = "windows")]
pub(crate) const DRAG_PREVIEW_PNG: &[u8] = &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0xDA, 0x63, 0xFC, 0xCF, 0xC0, 0x50, 0x0F, 0x00, 0x04, 0x85, 0x01, 0x80, 0x84, 0xA9, 0x8C, 0x21, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82];

/// State of one item drag. Lives in egui's temp data under `item_drag_id()` so the file table, the
/// sidebar and `Explorer::drive_item_drag` can all see it.
#[derive(Clone, Default)]
pub(crate) struct ItemDrag {
    /// Tab (by id) the drag started in.
    pub(crate) tab: u64,
    /// The row that was pressed.
    pub(crate) row: PathBuf,
    /// Where the button went down (screen space).
    pub(crate) press: egui::Pos2,
    /// False until the pointer has moved far enough for this to count as a drag (not a click).
    pub(crate) active: bool,
    /// What is being dragged (the whole selection, or just the pressed row).
    pub(crate) paths: Arc<Vec<PathBuf>>,
    /// Folder under the pointer this frame, filled in by `drag_drop_zone`.
    pub(crate) target: Option<PathBuf>,
}

pub(crate) fn item_drag_id() -> Id {
    Id::new("item_drag_state")
}

/// The paths being dragged right now (None when no drag is in progress).
pub(crate) fn item_drag_paths(ui: &Ui) -> Option<Arc<Vec<PathBuf>>> {
    ui.data(|d| d.get_temp::<ItemDrag>(item_drag_id()))
        .filter(|s| s.active)
        .map(|s| s.paths)
}

/// Marks `rect` as a place the dragged items can be dropped into `dir`: highlights it and records
/// it as the target while the pointer is over it.
pub(crate) fn drag_drop_zone(
    ui: &Ui,
    pal: &Palette,
    m: &Metrics,
    rect: Rect,
    dir: &Path,
    dragged: &[PathBuf],
) -> bool {
    if !ui.rect_contains_pointer(rect) {
        return false;
    }
    // A folder can't be dropped onto itself, and dropping into the folder the items already live
    // in would only create duplicates by accident.
    if dragged.iter().any(|p| p == dir) || dragged.iter().all(|p| p.parent() == Some(dir)) {
        return false;
    }
    let id = item_drag_id();
    if let Some(mut st) = ui.data(|d| d.get_temp::<ItemDrag>(id)) {
        st.target = Some(dir.to_path_buf());
        ui.data_mut(|d| d.insert_temp(id, st));
    }
    let r = CornerRadius::same(m.radius);
    ui.painter()
        .rect_filled(rect, r, pal.accent.gamma_multiply(0.22));
    ui.painter()
        .rect_stroke(rect, r, Stroke::new(1.5_f32, pal.accent), StrokeKind::Inside);
    true
}

/// `\\?\C:\dir` -> `C:\dir`, `\\?\UNC\server\share` -> `\\server\share`; other paths unchanged.
#[cfg(target_os = "windows")]
pub(crate) fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        p.to_path_buf()
    }
}

impl Explorer {
    /// Runs the drag-to-copy gesture: a press on a row (recorded by `table_ui`) becomes a drag once
    /// the pointer has moved a few pixels. Releasing over a folder row, a sidebar entry or another
    /// pane copies the dragged items there; leaving the window hands them to the OS as a file drag,
    /// so any other program (or FileFly window) can take them.
    pub(crate) fn drive_item_drag(&mut self, frame: &eframe::Frame, ui: &mut Ui, actions: &mut Vec<Action>) {
        let id = item_drag_id();
        let ctx = ui.ctx().clone();
        let (down, released, pressed, latest, esc) = ui.input(|i| {
            (
                i.pointer.primary_down(),
                i.pointer.primary_released(),
                i.pointer.primary_pressed(),
                i.pointer.latest_pos(),
                i.key_pressed(egui::Key::Escape),
            )
        });
        let clear = |ctx: &egui::Context| ctx.data_mut(|d| d.remove_temp::<ItemDrag>(id));

        // After an OS drag the button-up was swallowed by the OS, so egui may still think the
        // button is held. Ignore everything until the button is seen up or pressed again.
        if self.drag_lock {
            if pressed || !down {
                self.drag_lock = false;
            } else {
                clear(&ctx);
                return;
            }
        }
        let Some(mut st) = ctx.data(|d| d.get_temp::<ItemDrag>(id)) else {
            return;
        };

        if st.active && esc {
            clear(&ctx);
            self.drag_lock = down; // keep ignoring the still-held button
            return;
        }

        // Press -> drag: only once the pointer has really moved (egui's click limit is 6 px).
        if !st.active && down && !released {
            let Some(p) = latest else {
                return;
            };
            if (p - st.press).length() < 6.0 {
                return;
            }
            let Some(tab) = self.tabs.iter_mut().find(|t| t.id == st.tab) else {
                clear(&ctx);
                return;
            };
            // Dragging an unselected row drags just that row (and selects it), like Explorer.
            if !tab.selected.contains(&st.row) {
                tab.select_only(st.row.clone());
            }
            let paths = tab.selected_paths();
            if paths.is_empty() {
                clear(&ctx);
                return;
            }
            st.paths = Arc::new(paths);
            st.active = true;
        }

        // Where would a release land? A folder row / sidebar entry wins; otherwise the pane under
        // the pointer (unless the items already live in that folder).
        let mut target: Option<PathBuf> = None;
        let mut pane_rect: Option<Rect> = None;
        if st.active {
            target = st.target.take();
            if target.is_none() {
                if let Some(p) = latest {
                    if let Some((tid, r)) = self.pane_rects.iter().find(|(_, r)| r.contains(p)) {
                        if let Some(t) = self.tabs.iter().find(|t| t.id == *tid) {
                            let same = st
                                .paths
                                .iter()
                                .all(|s| s.parent() == Some(t.dir.as_path()));
                            if !t.special() && !same {
                                target = Some(t.dir.clone());
                                pane_rect = Some(*r);
                            }
                        }
                    }
                }
            }
        }

        if !down || released {
            clear(&ctx);
            if st.active {
                if let Some(dest) = target {
                    actions.push(Action::DropCopy((*st.paths).clone(), dest));
                }
            }
            return;
        }
        if !st.active {
            return;
        }

        // Left the window: hand the files to the OS.
        #[cfg(target_os = "windows")]
        let outside = self.cursor_outside_window().unwrap_or_else(|| {
            latest.map_or(true, |p| !ui.content_rect().contains(p))
        });
        #[cfg(target_os = "windows")]
        if outside {
            clear(&ctx);
            self.drag_lock = true;
            self.start_os_drag(frame, (*st.paths).clone());
            return;
        }

        // Feedback inside the window.
        let (m, pal) = (self.m, self.pal);
        ui.set_cursor_icon(if target.is_some() {
            egui::CursorIcon::Copy
        } else {
            egui::CursorIcon::Grabbing
        });
        ui.request_repaint();
        if let Some(r) = pane_rect {
            ui.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                Id::new("item_drop_preview"),
            ))
            .rect_stroke(
                r.shrink(1.5),
                CornerRadius::same(m.radius),
                Stroke::new(3.0_f32, pal.accent),
                StrokeKind::Inside,
            );
        }
        if let Some(p) = latest {
            let what = if st.paths.len() == 1 {
                st.paths[0]
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            } else {
                format!("{} items", st.paths.len())
            };
            let label = match &target {
                Some(d) => {
                    let name = d
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| d.display().to_string());
                    format!("Copy {what} to {name}")
                }
                None => what,
            };
            let tip = ui.layer_painter(egui::LayerId::new(
                egui::Order::Tooltip,
                Id::new("item_drag_ghost"),
            ));
            let galley = tip.layout_no_wrap(label, FontId::proportional(m.font), pal.text);
            let r = Rect::from_min_size(
                p + vec2(14.0, 14.0),
                galley.size() + vec2(m.pad * 2.0, m.pad),
            );
            tip.rect_filled(r, CornerRadius::same(m.radius), pal.layer);
            tip.rect_stroke(
                r,
                CornerRadius::same(m.radius),
                Stroke::new(1.0_f32, pal.accent),
                StrokeKind::Inside,
            );
            tip.galley(r.min + vec2(m.pad, m.pad * 0.5), galley, pal.text);
        }
        ctx.data_mut(|d| d.insert_temp(id, st)); // `target` was taken above: recomputed next frame
    }

    /// Starts a real OS file drag on Windows. Blocks until the user drops or cancels.
    #[cfg(target_os = "windows")]
    pub(crate) fn start_os_drag(&mut self, frame: &eframe::Frame, paths: Vec<PathBuf>) {
        // Shell paths only: `\\?\C:\x` and `\\?\UNC\wsl.localhost\...` (verbatim forms, which
        // canonicalising produces) can't be turned into shell items and made the drag code fail
        // for WSL / network files.
        let paths: Vec<PathBuf> = paths
            .into_iter()
            .map(|p| strip_verbatim(&p))
            .filter(|p| p.exists())
            .collect();
        if paths.is_empty() {
            return;
        }
        let Some(window) = frame.winit_window() else {
            return;
        };
        // A panic inside the drag crate (e.g. on a path the shell can't parse) must not take the
        // whole app down: turn it into an error message instead.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            drag::start_drag(
                window,
                drag::DragItem::Files(paths),
                drag::Image::Raw(DRAG_PREVIEW_PNG.to_vec()),
                |_, _| {},
                drag::Options::default(),
            )
        }));
        let msg = match result {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(format!("Couldn't drag the items out of the window: {e:?}")),
            Err(_) => Some(
                "Couldn't drag these items out of the window (the system rejected the location). \
                 Copy and paste them instead."
                    .to_owned(),
            ),
        };
        if let Some(msg) = msg {
            let ti = self.active.min(self.tabs.len().saturating_sub(1));
            if let Some(t) = self.tabs.get_mut(ti) {
                t.error = Some(msg);
            }
        }
        self.ui.request_repaint();
    }

    /// Files dropped onto the window from outside (Explorer, another FileFly window, ...) are
    /// copied into the folder of the pane they landed on.
    pub(crate) fn handle_dropped_files(&mut self, ui: &Ui) {
        let files: Vec<PathBuf> = ui.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if files.is_empty() {
            return;
        }
        // While an OS drag hovers the window egui gets no pointer events, so ask the OS where the
        // cursor is and convert it to window coordinates.
        let ppp = ui.ctx().pixels_per_point();
        let local = match (cursor_screen_pos(), self.own_info) {
            (Some((cx, cy)), Some(o)) => {
                Some(pos2((cx - o.rect[0]) / ppp, (cy - o.rect[1]) / ppp))
            }
            _ => ui.input(|i| i.pointer.latest_pos()),
        };
        let hit = local
            .and_then(|p| self.pane_rects.iter().find(|(_, r)| r.contains(p)))
            .map(|(t, _)| *t);
        let ti = hit
            .and_then(|id| self.tabs.iter().position(|t| t.id == id))
            .unwrap_or(self.active);
        if self.tabs[ti].special() {
            return;
        }
        let dest = self.tabs[ti].dir.clone();
        if files.iter().all(|f| f.parent() == Some(dest.as_path())) {
            return; // dropped back where it came from
        }
        self.paste_into(dest, files, false);
    }

    // ------------------------------------------------------------------ dialogs
}
