//! All drawing code: widgets, panels, dialogs and pages.

mod central;
mod chrome;
mod dialogs;
mod file_table;
mod licenses;
mod ops_dialog;
mod props;
mod settings;
mod sidebar;
mod status;
mod tabs;
mod tool_window;
mod toolbar;
mod trash_table;
mod widgets;
pub mod picker;

pub(crate) use chrome::*;
pub(crate) use file_table::*;
pub(crate) use licenses::*;
pub(crate) use props::*;
pub(crate) use settings::*;
pub(crate) use sidebar::*;
pub(crate) use tool_window::*;
pub(crate) use trash_table::*;
pub(crate) use widgets::*;
