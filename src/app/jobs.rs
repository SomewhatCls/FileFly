//! Background jobs: archives and generic file operations.

use crate::prelude::*;
use std::io;

impl Explorer {
    pub(crate) fn job_started(&mut self, label: &str) {
        self.jobs_running += 1;
        self.job_label = label.to_owned();
    }

    pub(crate) fn job_finished(&mut self) {
        self.jobs_running = self.jobs_running.saturating_sub(1);
        if self.jobs_running == 0 {
            self.job_label.clear();
        }
    }

    /// Unpacks `zip` in the background; when done, tab `ti` opens `zip\rel` (see `AppMsg::ArchiveReady`).
    pub(crate) fn open_archive(&mut self, ti: usize, zip: PathBuf, rel: PathBuf) {
        if !self.extracting.insert(zip.clone()) {
            return; // already being opened (double click on a big archive)
        }
        self.job_started("Opening archive…");
        let tab = self.tabs[ti].id;
        let target = if rel.as_os_str().is_empty() { zip.clone() } else { zip.join(&rel) };
        let (tx, ui) = (self.msg_tx.clone(), self.ui.clone());
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                mount_archive(&zip).map(|_| ()).map_err(|e| format!("Could not open the archive: {e}"))
            }))
            .unwrap_or_else(|_| Err("Could not open the archive: unexpected failure.".to_owned()));
            let _ = tx.send(AppMsg::ArchiveReady { tab, zip, target, result });
            ui.request_repaint();
        });
    }

    /// "Compress to ZIP file": one zip next to the items, named after the item (or the folder when
    /// several are selected), built by the operating system's own tools.
    pub(crate) fn compress(&mut self, ti: usize, items: Vec<PathBuf>) {
        let Some(parent) = items.first().and_then(|p| p.parent()).map(Path::to_path_buf) else {
            return;
        };
        let base: Option<std::ffi::OsString> = if items.len() == 1 {
            items[0].file_stem().map(|s| s.to_os_string())
        } else {
            parent.file_name().map(|s| s.to_os_string())
        };
        let mut name = base.unwrap_or_else(|| "Archive".into());
        name.push(".zip");
        let dest = unique_copy_name(&parent.join(name));
        let tab = self.tabs[ti].id;
        self.job_started("Compressing…");
        let (tx, ui) = (self.msg_tx.clone(), self.ui.clone());
        std::thread::spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| compress_items(&items, &dest)))
                .unwrap_or_else(|_| Err(io::Error::other("unexpected failure")));
            let msg = match r {
                Ok(()) => AppMsg::Made { tab, created: vec![dest], errors: Vec::new(), rename: true, open: false },
                Err(e) => AppMsg::Made {
                    tab,
                    created: Vec::new(),
                    errors: vec![format!("Could not create the ZIP file: {e}")],
                    rename: false,
                    open: false,
                },
            };
            let _ = tx.send(msg);
            ui.request_repaint();
        });
    }

    /// "Extract all": every zip goes into a new folder (never an existing one) next to it.
    pub(crate) fn extract(&mut self, ti: usize, zips: Vec<PathBuf>) {
        let mut jobs: Vec<(PathBuf, PathBuf)> = Vec::new();
        for zip in zips.into_iter().filter(|z| z.is_file()) {
            let (Some(parent), Some(stem)) = (zip.parent(), zip.file_stem()) else {
                continue;
            };
            let dest = unique_copy_name(&folder_outside(parent).join(stem));
            jobs.push((zip, dest));
        }
        self.run_extract_new_folder(ti, jobs);
    }

    /// Unpacks each `(zip, new folder)` pair in the background. The folder never exists beforehand
    /// (see `extract`), so nothing can clash and no question is needed.
    fn run_extract_new_folder(&mut self, ti: usize, jobs: Vec<(PathBuf, PathBuf)>) {
        if jobs.is_empty() {
            return;
        }
        let tab = self.tabs[ti].id;
        self.job_started("Extracting…");
        let (tx, ui) = (self.msg_tx.clone(), self.ui.clone());
        std::thread::spawn(move || {
            let mut created = Vec::new();
            let mut errors = Vec::new();
            for (zip, dest) in jobs {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| extract_zip(&zip, &dest)))
                    .unwrap_or_else(|_| Err(io::Error::other("unexpected failure")));
                match r {
                    Ok(()) => created.push(dest),
                    Err(e) => errors.push(format!("Could not extract “{}”: {e}", file_name_lossy(&zip))),
                }
            }
            let _ = tx.send(AppMsg::Made { tab, created, errors, rename: false, open: false });
            ui.request_repaint();
        });
    }

    /// "Extract to…": unpacks each `(zip, target folder)` into the (possibly non-empty) target.
    /// The zip is unpacked into a hidden staging folder inside the target first; its contents are
    /// then planned and moved in exactly like a paste, so clashes raise the same question dialog.
    /// `open`: show the (first) target in a new tab ("Show extracted files when complete"). That
    /// tab opens as soon as the contents are staged and refreshes when the move has finished.
    pub(crate) fn run_extract(&mut self, ti: usize, jobs: Vec<(PathBuf, PathBuf)>, open: bool) {
        if jobs.is_empty() {
            return;
        }
        let open: Option<PathBuf> = if open { jobs.first().map(|(_, t)| t.clone()) } else { None };
        let tab = self.tabs[ti].id;
        self.job_started("Extracting…");
        let (tx, ui) = (self.msg_tx.clone(), self.ui.clone());
        std::thread::spawn(move || {
            let mut plan = PastePlan::default();
            let mut claimed: HashSet<PathBuf> = HashSet::new();
            let mut stagings: Vec<PathBuf> = Vec::new();
            let mut errors: Vec<String> = Vec::new();
            for (zip, target) in jobs {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    stage_zip(&zip, &target, &mut plan, &mut claimed, &mut stagings)
                }))
                .unwrap_or_else(|_| Err(io::Error::other("unexpected failure")));
                if let Err(e) = r {
                    errors.push(format!("Could not extract “{}”: {e}", file_name_lossy(&zip)));
                }
            }
            let msg = if stagings.is_empty() {
                // Nothing was unpacked: report like any other failed job.
                AppMsg::Made { tab, created: Vec::new(), errors, rename: false, open: false }
            } else {
                AppMsg::ExtractStaged { tab, plan, stagings, errors, open }
            };
            let _ = tx.send(msg);
            ui.request_repaint();
        });
    }

    /// Handles `AppMsg::ExtractStaged` on the UI thread: asks about clashes (same dialog as a
    /// paste), or moves everything straight in when nothing clashes.
    pub(crate) fn extract_staged(
        &mut self,
        tab: u64,
        plan: PastePlan,
        stagings: Vec<PathBuf>,
        errors: Vec<String>,
        open: Option<PathBuf>,
    ) {
        self.job_finished();
        self.extract_staging.extend(stagings);
        if !errors.is_empty() {
            if let Some(t) = self.tabs.iter_mut().find(|t| t.id == tab) {
                t.error = Some(errors.join("\n"));
            }
        }
        if let Some(dir) = open {
            self.new_tab(dir);
        }
        if self.conflict.is_some() {
            self.deferred_extracts.push(plan); // a question is already on screen: ask afterwards
        } else {
            self.start_staged_paste(plan);
        }
    }

    /// cut = true: items are moved out of staging, which sits on the same drive as the target.
    fn start_staged_paste(&mut self, plan: PastePlan) {
        if plan.todo.is_empty() {
            self.run_paste(plan.ready, true, plan.merged_dirs);
        } else {
            self.conflict = Some(PendingPaste {
                cut: true,
                ready: plan.ready,
                todo: plan.todo,
                apply_all: false,
                merged_dirs: plan.merged_dirs,
            });
        }
    }

    /// Called once per frame (from `pump_messages`): starts an extraction that had to wait for
    /// another question, and deletes finished staging folders. The latter waits until no question is
    /// open and no operation is running, so it can never pull files out from under a paste.
    pub(crate) fn pump_staging(&mut self) {
        if self.conflict.is_none() && !self.deferred_extracts.is_empty() {
            let plan = self.deferred_extracts.remove(0);
            self.start_staged_paste(plan);
        }
        if self.extract_staging.is_empty()
            || self.conflict.is_some()
            || !self.ops.is_empty()
            || !self.deferred_extracts.is_empty()
        {
            return;
        }
        let dirs = std::mem::take(&mut self.extract_staging);
        std::thread::spawn(move || dirs.iter().for_each(|d| discard_staging(d)));
    }

    // ------------------------------------------------------------------ background work

    /// Runs `f` on a worker thread. It shows up in a progress dialog (after half a second, so quick
    /// operations never flash one) and reports through the `OpProgress` it is handed.
    pub(crate) fn run_op(
        &mut self,
        spec: OpSpec,
        f: impl FnOnce(Arc<OpProgress>) -> Result<String, String> + Send + 'static,
    ) {
        let id = self.next_op;
        self.next_op += 1;
        let progress = Arc::new(OpProgress::default());
        self.ops.push(ActiveOp::new(id, spec, progress.clone()));
        let (tx, ui) = (self.msg_tx.clone(), self.ui.clone());
        std::thread::spawn(move || {
            // A panic in the job must still report back, otherwise the dialog stays forever.
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(progress))).unwrap_or_else(|_| {
                Err("The operation failed unexpectedly (details in crash.log).".to_owned())
            });
            let _ = tx.send(AppMsg::Job(id, r));
            ui.request_repaint();
        });
    }

    /// `run_op` for jobs that can't report progress: the dialog shows just the title and a moving bar.
    pub(crate) fn run_job(
        &mut self,
        label: &str,
        f: impl FnOnce() -> Result<String, String> + Send + 'static,
    ) {
        let spec = OpSpec {
            title: label.trim_end_matches('…').to_owned(),
            from: String::new(),
            to: String::new(),
            cancelable: false,
        };
        self.run_op(spec, move |_| f());
    }
}

/// Unpacks `zip` into a staging folder inside `target` and plans the move of its contents into
/// `target`, exactly like `paste_into` plans a paste (same-named folders are merged, only real
/// clashes end up in `plan.todo`).
fn stage_zip(
    zip: &Path,
    target: &Path,
    plan: &mut PastePlan,
    claimed: &mut HashSet<PathBuf>,
    stagings: &mut Vec<PathBuf>,
) -> io::Result<()> {
    let staging = extract_zip_staged(zip, target)?;
    stagings.push(staging.clone()); // registered first, so it is cleaned up even if planning fails
    let mut sub = PastePlan::default(); // scratch plan: an unreadable folder adds nothing half-done
    for entry in fs::read_dir(&staging)? {
        let entry = entry?;
        plan_item(entry.path(), target.join(entry.file_name()), claimed, &mut sub)?;
    }
    plan.ready.extend(sub.ready);
    plan.todo.extend(sub.todo);
    plan.merged_dirs.extend(sub.merged_dirs);
    Ok(())
}

fn file_name_lossy(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}
