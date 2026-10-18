#![cfg_attr(windows, windows_subsystem = "windows")]

//! FileFly - a tabbed, tiling file explorer built on egui.
//!
//! Layout of the source tree:
//!   app/        the `Explorer` state, navigation, jobs, clipboard, actions, frame loop
//!   ui/         all drawing code (title bar, sidebar, toolbar, file table, dialogs, settings, ...)
//!   model/      plain data: listing entries, tabs, the tiling tree, Recycle Bin state
//!   ops/        file-operation progress and the paste workers
//!   search.rs, quick.rs, settings.rs, messages.rs, dragdrop.rs, startup.rs
//!   drives.rs, fsops.rs, icons.rs, indexer.rs, peers.rs, picker.rs, theme.rs, trashbin.rs,
//!   defaultapp.rs   (platform / file-system helpers)

mod app;
mod defaultapp;
mod dragdrop;
mod drives;
mod fsops;
mod icons;
mod indexer;
mod messages;
mod model;
mod ops;
mod peers;
mod picker;
mod prelude;
mod quick;
mod search;
mod settings;
mod startup;
mod theme;
mod trashbin;
mod ui;

fn main() -> eframe::Result<()> {
    startup::run()
}
