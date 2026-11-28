//! The `Explorer` application: shared core, windows, navigation, jobs, actions and the frame loop.

mod actions;
mod clipboard;
mod jobs;
mod navigation;
mod pump;
mod runtime;
mod shortcuts;
mod state;
mod tabs;
mod trash;
mod typeahead;
mod window;
mod windows;

pub(crate) use state::*;
pub(crate) use windows::{is_handled_viewport_failure, show_viewport, win_id, window_builder, windows_mut};
pub(crate) use typeahead::TypeAhead;
