//! The `Explorer` application: state, navigation, jobs, actions and the frame loop.

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
mod window;

pub(crate) use state::*;
