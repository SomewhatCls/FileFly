//! Background jobs: archives and generic file operations.

use crate::prelude::*;

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
                .unwrap_or_else(|_| Err(std::io::Error::other("unexpected failure")));
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
        self.run_extract(ti, jobs, false);
    }

    /// Unpacks each `(zip, new folder)` pair in the background. `open`: show the first result in a
    /// new tab when done ("Extract to…" with "Show extracted files when complete").
    pub(crate) fn run_extract(&mut self, ti: usize, jobs: Vec<(PathBuf, PathBuf)>, open: bool) {
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
                    .unwrap_or_else(|_| Err(std::io::Error::other("unexpected failure")));
                match r {
                    Ok(()) => created.push(dest),
                    Err(e) => errors.push(format!(
                        "Could not extract “{}”: {e}",
                        zip.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
                    )),
                }
            }
            let _ = tx.send(AppMsg::Made { tab, created, errors, rename: false, open });
            ui.request_repaint();
        });
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
