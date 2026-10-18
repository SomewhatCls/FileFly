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
mod toolbar;
mod trash_table;
mod widgets;

pub(crate) use central::*;
pub(crate) use chrome::*;
pub(crate) use file_table::*;
pub(crate) use props::*;
pub(crate) use sidebar::*;
pub(crate) use settings::*;
pub(crate) use trash_table::*;
pub(crate) use widgets::*;
