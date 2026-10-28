#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! FileFly - a tabbed, tiling file explorer built with the egui framework.
//!
//! Layout of the source tree:
//!   app/          the `Explorer` state, navigation, jobs, clipboard, actions, frame loop
//!   ui/           all drawing code (title bar, sidebar, toolbar, file table, dialogs, settings, ...)
//!   data/         plain data: listing entries, tabs, the tiling tree, Recycle Bin state
//!   ops/          file-operation progress and the paste workers
//!   indexer/      the Indexing logic behind the folder size index
//!   indexer/store the logic behind storing the indexed folders in a compressed binary format

mod app;
mod default_app;
mod drag_drop;
mod drives;
mod elevate;
mod fsops;
mod icons;
mod indexer;
mod messages;
mod data;
mod ops;
mod peers;
mod picker;
mod prelude;
mod quick;
mod search;
mod settings;
mod startup;
mod theme;
mod trash_bin;
mod ui;
mod git;
mod backdrop_blur;

fn main() -> eframe::Result<()> {
    startup::run()
}
