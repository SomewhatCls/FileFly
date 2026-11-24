//! Cut / copy / paste.

use crate::prelude::*;

impl Explorer {
    /// Runs the decided paste items as one background job with a progress dialog.
    pub(crate) fn run_paste(&mut self, items: Vec<PasteItem>, cut: bool, merged_dirs: Vec<PathBuf>) {
        if items.is_empty() {
            if cut {
                cleanup_merged_dirs(&merged_dirs);
            }
            return;
        }
        if cut {
            self.clipboard = None;
        }
        let n = items.len();
        let (tx, ui) = (self.msg_tx.clone(), self.ui.clone());
        let verb = if cut { "Moving" } else { "Copying" };
        let title = if n == 1 {
            let name = items[0].dst.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            format!("{verb} “{}”", truncate(&name, 34))
        } else {
            format!("{verb} {n} items")
        };
        let locked: Vec<PathBuf> = items.iter().flat_map(|it| [it.src.clone(), it.dst.clone()]).collect();
        let dir_of = |p: &Path| p.parent().map(|d| display_path(d).display().to_string()).unwrap_or_default();
        let spec = OpSpec {
            title,
            from: dir_of(&items[0].src),
            to: dir_of(&items[0].dst),
            cancelable: true,
        };
        let op = self.run_op(spec, move |prog| {
            prog.total_items.store(n as u64, AO::Relaxed);
            // Size of every item (copies only), so a skipped item can take its share back out.
            let mut sizes: Vec<u64> = vec![0; n];
            if !cut {
                // Measure first, so the bar and the time estimate are real from the start.
                // (A move is mostly renames; its copy sizes are added only where one is needed.)
                prog.preparing.store(true, AO::Relaxed);
                let mut total = 0u64;
                for (i, it) in items.iter().enumerate() {
                    match tree_bytes(&it.src, &prog) {
                        Ok(b) => {
                            sizes[i] = b;
                            total += b;
                        }
                        Err(_) => return Ok(String::new()), // cancelled while measuring
                    }
                }
                prog.total_bytes.store(total, AO::Relaxed);
                prog.preparing.store(false, AO::Relaxed);
            }

            // Items that don't overlap run side by side (up to PASTE_WORKERS at a time); an item that
            // overlaps one that is running or queued ahead of it waits for its turn. Cancelling one
            // item (its button in the dialog) stops only that item; cancelling the whole operation
            // stops everything that has not finished.
            // Waiting and running items are indices into `items`. A failure is (item, message, refused for
            // lack of access rights).
            let items = &items;
            let queue: std::sync::Mutex<(VecDeque<usize>, Vec<usize>)> =
                std::sync::Mutex::new(((0..n).collect(), Vec::new()));
            let cv = std::sync::Condvar::new();
            let errors: std::sync::Mutex<Vec<(usize, String, bool)>> = std::sync::Mutex::new(Vec::new());
            let sizes = &sizes;
            let prog = &*prog;
            std::thread::scope(|sc| {
                for _ in 0..PASTE_WORKERS.min(n).max(1) {
                    sc.spawn(|| loop {
                        let idx = {
                            let mut g = queue.lock().unwrap_or_else(|e| e.into_inner());
                            loop {
                                if prog.cancelled() {
                                    g.0.clear();
                                    cv.notify_all();
                                    return;
                                }
                                if g.0.is_empty() {
                                    return;
                                }
                                let pick = (0..g.0.len()).find(|&k| {
                                    let cand = &items[g.0[k]];
                                    g.1.iter().all(|&r| !paste_overlaps(cand, &items[r]))
                                        && g.0.iter().take(k).all(|&e| !paste_overlaps(cand, &items[e]))
                                });
                                if let Some(k) = pick {
                                    if let Some(idx) = g.0.remove(k) {
                                        g.1.push(idx);
                                        break idx;
                                    }
                                }
                                g = cv.wait_timeout(g, Duration::from_millis(100)).unwrap_or_else(|e| e.into_inner()).0;
                            }
                        };
                        let it = &items[idx];
                        let id = idx as u64 + 1;
                        let name = it
                            .dst
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        prog.begin_item(id, &name);
                        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| paste_one(it, cut, prog)))
                            .unwrap_or_else(|_| Pasted::Failed(format!("Could not paste “{name}”: unexpected failure")));
                        let (done_by_it, total_by_it) = prog.end_item(id);
                        let denied = matches!(outcome, Pasted::Denied(_));
                        match outcome {
                            Pasted::Done => {
                                prog.done_items.fetch_add(1, AO::Relaxed);
                            }
                            Pasted::Failed(e) | Pasted::Denied(e) => {
                                errors.lock().unwrap_or_else(|e| e.into_inner()).push((idx, e, denied));
                                prog.done_items.fetch_add(1, AO::Relaxed);
                            }
                            Pasted::Stopped if prog.cancelled() => {} // the whole operation was cancelled
                            Pasted::Stopped => {
                                // Only this item was cancelled: its partial copy is gone, so its bytes
                                // no longer count towards the bar, and the others carry on.
                                prog.forgo(done_by_it, sizes[idx] + total_by_it);
                                prog.done_items.fetch_add(1, AO::Relaxed);
                            }
                        }
                        let mut g = queue.lock().unwrap_or_else(|e| e.into_inner());
                        g.1.retain(|&i| i != idx);
                        cv.notify_all();
                    });
                }
            });

            if cut {
                // Only empty folders go; ones still holding skipped or failed items stay.
                cleanup_merged_dirs(&merged_dirs);
            }

            let mut errors = errors.into_inner().unwrap_or_else(|e| e.into_inner());
            if errors.is_empty() {
                return Ok(String::new());
            }
            errors.sort_by_key(|e| e.0);
            let mut text = errors.iter().take(2).map(|e| e.1.as_str()).collect::<Vec<_>>().join("  |  ");
            if errors.len() > 2 {
                text.push_str(&format!("  (+{} more)", errors.len() - 2));
            }
            // Items refused for lack of rights can be redone as administrator.
            let denied: Vec<(PathBuf, PathBuf, bool)> = errors
                .iter()
                .filter(|e| e.2)
                .map(|e| (items[e.0].src.clone(), items[e.0].dst.clone(), items[e.0].replace))
                .collect();
            if !denied.is_empty() {
                let retry = AdminRetry::Op(AdminOp::Paste { items: denied, cut });
                let _ = tx.send(AppMsg::AdminOffer { msg: text.clone(), retry });
                ui.request_repaint();
            }
            Err(text)
        });
        self.lock_op_paths(op, locked);
    }

    pub(crate) fn paste(&mut self) {
        // DO NOT USE THE SYSTEM CLIPBOARD ON NON-WINDOWS
        #[cfg(windows)]
        let clip = get_clipboard();
        #[cfg(not(windows))]
        let clip = self.clipboard.clone();

        let Some((sources, cut)) = clip else {
            return;
        };
        let dir = self.tabs[self.active].dir.clone();
        self.paste_into(dir, sources, cut);
    }

    /// Pastes (or drops) `sources` into `dir`; name clashes are asked about like in a normal paste.
    pub(crate) fn paste_into(&mut self, dir: PathBuf, sources: Vec<PathBuf>, cut: bool) {
        if self.conflict.is_some() {
            return; // a question is already on screen
        }
        let mut plan = PastePlan::default();
        let mut claimed: HashSet<PathBuf> = HashSet::new();
        for source in sources {
            let Some(name) = source.file_name() else {
                continue;
            };
            let dest = dir.join(name);
            if cut && source == dest {
                continue; // moving an item onto itself is a no-op
            }
            if source.is_dir() && dir.starts_with(&source) {
                self.tabs[self.active].error(format!(
                    "Can't {} “{}” into itself or one of its subfolders.",
                    if cut { "move" } else { "copy" },
                    name.to_string_lossy()
                ));
                continue;
            }
            let label = name.to_string_lossy().into_owned();
            // Same-named folders are merged (their children are planned one by one); only real
            // clashes (file vs file, file vs folder, same-folder copies) end up in `todo`.
            // Plan into a scratch plan so a folder that can't be read adds nothing half-done.
            let mut sub = PastePlan::default();
            match plan_item(source, dest, &mut claimed, &mut sub) {
                Ok(()) => {
                    plan.ready.extend(sub.ready);
                    plan.todo.extend(sub.todo);
                    plan.merged_dirs.extend(sub.merged_dirs);
                }
                Err(e) => {
                    self.tabs[self.active].error(format!("Can't read “{label}”: {e}"));
                }
            }
        }
        if plan.todo.is_empty() {
            self.run_paste(plan.ready, cut, plan.merged_dirs);
        } else {
            self.conflict = Some(PendingPaste {
                cut,
                ready: plan.ready,
                todo: plan.todo,
                apply_all: false,
                merged_dirs: plan.merged_dirs,
            });
        }
    }

    /// Checking whether pasting is possible has to be done differently
    /// for Windows and Linux because unlike Windows, Linux does not
    /// offer cheap OS clipboard calls. This is why we have to either
    /// significantly throttle clipboard performance to get an acceptable
    /// result or use our own clipboard.
    /// Since FileFly does NOT offer support for cross-app drag and drop on
    /// Linux systems, the second option is my choice.
    pub(crate) fn can_paste(&self) -> bool {
        #[cfg(windows)]
        {
            get_clipboard().is_some()
        }
        #[cfg(not(windows))]
        {
            self.clipboard.is_some()
        }
    }
}
