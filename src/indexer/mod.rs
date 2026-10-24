//! Folder-size index.
//!
//! Two tiers:
//!  * EVERY folder size that was ever scanned lives in a compact on-disk hash table (`store`,
//!    no external database). New results are buffered in RAM and written in batches, so a cancelled
//!    scan keeps its progress, and there is no big "save everything" step on exit and no big
//!    "load everything" step at startup;
//!  * a bounded in-RAM cache sits in front of it (the limit is a user setting, `limit_mb`). A lookup
//!    that misses RAM reads one 4 KiB slot from the file and copies the entry into RAM. When RAM goes
//!    over the limit, entries are dropped from RAM only (the file still has them).
//!
//! Also:
//!  * the cache is shared (`Arc`) instead of being cloned on every navigation;
//!  * scanning is iterative, runs on a small worker pool at below-normal priority and yields regularly;
//!  * entries are only trusted while the folder's mtime matches *and* they are younger than `TTL_SECS`
//!    (a directory's mtime does not change when a file deeper down grows, so mtime alone goes stale);
//!  * a file watcher reports changes, and the live updater corrects the affected folder sizes within
//!    about a second, however deep the change is and even if the file is only being written to.
//!
//! # Files
//!
//! | file          | role                                                                              |
//! |---------------|-----------------------------------------------------------------------------------|
//! | `model.rs`    | plain data (`CachedFolder`, `IndexMsg`), constants, time helpers                  |
//! | `store/`      | the persistent index: on-disk hash table + RAM cache (no knowledge of the file system) |
//! | `scan.rs`     | the folder walk that produces sizes                                               |
//! | `hot.rs`      | files that just changed: exact sizes and polling (fixes stale sizes of open files)|
//! | `live.rs`     | change sink + updater thread: events -> invalidate -> rescan only what is affected |
//! | `watcher.rs`  | `DirWatcher`: which folders are watched, and how                                  |
//! | `engine.rs`   | `Indexer`: the API the app uses; owns store, workers and updater                   |
//! | `priority.rs` | below-normal thread priority                                                      |
//!
//! # Wiring (one change in the app)
//!
//! ```ignore
//! let watcher = DirWatcher::with_sink(ctx.clone(), indexer.change_sink());
//! ```
//! `DirWatcher::new(ctx)` still works, but then only the UI refreshes; sizes are not updated live.

mod engine;
mod hot;
mod live;
mod model;
mod priority;
mod scan;
mod store;
mod watcher;

pub use engine::Indexer;
pub use live::ChangeSink;
pub use model::{CachedFolder, IndexMsg};
pub use watcher::DirWatcher;
