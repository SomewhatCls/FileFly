//! Folder listing and (recursive) search.

use crate::prelude::*;

#[derive(Default)]
pub(crate) struct Debounce {
    pub(crate) first: Option<Instant>,
    pub(crate) last: Option<Instant>,
}

impl Debounce {
    pub(crate) fn poke(&mut self) {
        let now = Instant::now();
        self.first.get_or_insert(now);
        self.last = Some(now);
    }
    pub(crate) fn pending(&self) -> bool {
        self.first.is_some()
    }
    pub(crate) fn ready(&mut self, quiet: Duration, max: Duration) -> bool {
        if let (Some(f), Some(l)) = (self.first, self.last) {
            if l.elapsed() >= quiet || f.elapsed() >= max {
                self.first = None;
                self.last = None;
                return true;
            }
        }
        false
    }
}

// ==============================================================================================
// File Listing for dir
pub(crate) fn read_listing(dir: &Path, show_hidden: bool, fmt: DateFormat) -> Result<Vec<Entry>, String> {
    let rd = fs::read_dir(dir).map_err(|e| format!("Could not read this folder: {e}"))?;
    let mut out = Vec::with_capacity(256);
    for item in rd.flatten() {
        let Ok(md) = item.metadata() else { continue };
        let name = item.file_name().to_string_lossy().into_owned();
        if !show_hidden && (is_hidden_entry(&name, &md)) {
            continue;
        }
        out.push(Entry::from_path(name, item.path(), &md, fmt));
    }
    Ok(out)
}

pub struct SearchJob {
    pub(crate) rx: Receiver<Vec<Entry>>,
    pub(crate) cancel: Arc<AtomicBool>,
    pub done: bool,
}

impl Drop for SearchJob {
    fn drop(&mut self) {
        self.cancel.store(true, AtomicOrdering::Relaxed); // dropping = cancelling
    }
}

//Executes recursive search, called when search field input changes
pub(crate) fn start_search(roots: Vec<PathBuf>, query: String, show_hidden: bool, fmt: DateFormat, ui: egui::Context, ) -> SearchJob {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    let file_ending_search = if query.starts_with("*.") {true} else {false};
    let cfg = load_settings(); // (one read of settings.json, not two)
    let (max_results, threads) = (cfg.max_search_results, cfg.search_threads);

    std::thread::spawn(move || {
        let mut batch: Vec<Entry> = Vec::new();
        let mut sent = 0usize;
        let mut last_flush = Instant::now();
        for root in roots {
            let c_walk = c.clone();
            let walker = WalkDir::new(&root)
                .skip_hidden(false)
                .follow_links(false)
                .parallelism(Parallelism::RayonNewPool(threads))
                .process_read_dir(move |_, _, _, children| {
                    if c_walk.load(AtomicOrdering::Relaxed) {
                        children.clear(); // stop descending
                        return;
                    }
                    if !show_hidden {
                        // don't walk into dot-folders (.git etc.) at all
                        children.retain(|c| match c {
                            Ok(dir) => !dir.file_name().to_string_lossy().starts_with('.'),
                            //This is overtly aggressive since it targets all system folders, including Program Files etc.
                            //leading to root search being impossible.
                            //Ok(dir) => !is_hidden_entry(&dir.file_name().to_string_lossy(), &dir.metadata().unwrap()),
                            Err(_) => false,
                        });
                    }
                });

            for res in walker {
                if c.load(AtomicOrdering::Relaxed) {
                    return;
                }
                let Ok(de) = res else { continue };
                if de.depth() == 0 {
                    continue;
                }
                let name = de.file_name().to_string_lossy().into_owned();
                //Check if object name contains query, or if searching for file extensions with
                //*.ext syntax, check if file has ending.
                if !name.to_lowercase().contains(&query) && !file_ending_search ||
                    file_ending_search && !name.to_lowercase().ends_with(&query[1..]) {
                        continue;
                }
                // only matches pay for a metadata call
                let Ok(md) = de.metadata() else { continue };
                if !show_hidden && is_hidden_entry(&name, &md) {
                    continue;
                }
                batch.push(Entry::from_path(name, de.path(), &md, fmt));

                if batch.len() >= 256 || last_flush.elapsed() > Duration::from_millis(50) {
                    sent += batch.len();
                    if tx.send(std::mem::take(&mut batch)).is_err() {
                        return;
                    }
                    ui.request_repaint();
                    last_flush = Instant::now();
                    if sent >= max_results {
                        break; // fall through so the channel is closed and `done` gets set
                    }
                }
            }
            if sent >= max_results {
                break; // the cap is for the whole search: don't start on the next drive
            }
        }
        if !batch.is_empty() {
            let _ = tx.send(batch);
        }
        drop(tx); // disconnect first, so the repaint below sees `done`
        ui.request_repaint();
    });

    SearchJob { rx, cancel, done: false }
}

//Restarts search when input in Search field changes.
pub(crate) fn on_search_changed(tab: &mut Tab, show_hidden: bool, fmt: DateFormat, ctx: &egui::Context) {
    tab.search_job = None; // dropping the old job cancels its walk
    tab.search_results.clear();
    tab.search_lookup.clear();
    tab.rename = None;
    if !tab.search_query.is_empty() {
        let roots = if tab.chosen_search_scope == SearchScope::System {
            load_drives(false).into_iter().filter(|d| d.kind != DriveKind::Network).map(|d| d.mount_point).collect()
        } else {
            vec![tab.dir.clone()]
        };
        tab.search_job = Some(start_search(roots, tab.search_query.clone(), show_hidden, fmt, ctx.clone()));

    }
    tab.view_dirty = true;
}

pub(crate) fn poll_search(tab: &mut Tab, indexer: &Indexer) {
    use mpsc::TryRecvError;
    let Some(job) = &mut tab.search_job else { return };
    let mut changed = false;
    loop {
        match job.rx.try_recv() {
            Ok(mut batch) => {
                for e in batch.iter_mut().filter(|e| e.is_dir && !e.is_link) {
                    e.dir_size = indexer.lookup(&e.path);
                }
                let base = tab.search_results.len();
                for (k, e) in batch.iter().enumerate() {
                    tab.search_lookup.insert(path_key(&e.path), base + k);
                }
                tab.search_results.extend(batch);
                changed = true;
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                job.done = true;
                break;
            }
        }
    }
    if changed {
        tab.view_dirty = true;
    }
}

pub(crate) fn rebuild_view(tab: &mut Tab) {
    // While searching, the walker has already filtered by name, so no contains() check here.
    let entries = if tab.search.trim().is_empty() {
        &tab.entries
    } else {
        &tab.search_results
    };

    tab.view.clear();
    tab.view.extend(0..entries.len());

    let (col, asc) = (tab.sort_col, tab.sort_asc);
    tab.view.sort_by(|&a, &b| {
        let (ea, eb) = (&entries[a], &entries[b]);
        let d = eb.is_dir.cmp(&ea.is_dir); // folders always first
        if d != Ordering::Equal {
            return d;
        }
        let o = match col {
            SortColumn::Name => natural_cmp(&ea.name_lc, &eb.name_lc),
            SortColumn::Modified => ea.modified.cmp(&eb.modified),
            SortColumn::Type => ea
                .type_text
                .cmp(&eb.type_text)
                .then_with(|| natural_cmp(&ea.name_lc, &eb.name_lc)),
            SortColumn::Size => ea.sort_size().cmp(&eb.sort_size()),
        };
        if asc {
            o
        } else {
            o.reverse()
        }
    });
    tab.view_dirty = false;
}
