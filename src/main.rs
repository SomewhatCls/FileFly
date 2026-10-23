#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! FileFly - a tabbed, tiling file explorer built on egui.
//!
//! Layout of the source tree:
//!   app/        the `Explorer` state, navigation, jobs, clipboard, actions, frame loop
//!   ui/         all drawing code (title bar, sidebar, toolbar, file table, dialogs, settings, ...)
//!   data/      plain data: listing entries, tabs, the tiling tree, Recycle Bin state
//!   ops/        file-operation progress and the paste workers
//!   search.rs, quick.rs, settings.rs, messages.rs, drag_drop, startup.rs
//!   drives.rs, fsops.rs, icons.rs, indexer.rs, peers.rs, picker.rs, theme.rs, trash_bin,
//!   default_app   (platform / file-system helpers)

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

fn main() -> eframe::Result<()> {
    startup::run()
}
