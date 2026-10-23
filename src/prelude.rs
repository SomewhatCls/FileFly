//! Everything the other modules need in scope: external crates, std items and the crate's own
//! shared types. Each module starts with `use crate::prelude::*;`.
#![allow(unused_imports)]

pub(crate) use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, CornerRadius, FontId, Id, Layout, Margin, Rect,
    RichText, Sense, Stroke, StrokeKind, Ui,
};
pub(crate) use egui::containers::scroll_area::{DragScroll, ScrollSource};
pub(crate) use jwalk::{Parallelism, WalkDir};
pub(crate) use serde::{Deserialize, Serialize};
pub(crate) use std::cell::Cell;
pub(crate) use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
pub(crate) use std::{
    cmp::Ordering,
    collections::{hash_map::DefaultHasher, HashMap, HashSet, VecDeque},
    fs,
    fs::File,
    hash::{Hash, Hasher},
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering as AO},
        mpsc::{self, Receiver, Sender},
        Arc,
    },
    time::{Duration, Instant, SystemTime},
};
pub(crate) use num_format::{Locale, ToFormattedString};

// Sibling crate modules that are referenced by path (`trashbin::Item`, `defaultapp::is_default()`, ...).
pub(crate) use crate::{defaultapp, drives, picker, trashbin};
pub(crate) use crate::drives::{
    clean_path, is_network_path, load_drives, path_available, spawn_watcher, DriveInfo, DriveKind,
    DriveWatchCtl,
};
pub(crate) use crate::peers::{cursor_screen_pos, now_ms, Peers, WinInfo};

// Glob re-exports of the existing helper modules ...
pub(crate) use crate::fsops::*;
pub(crate) use crate::icons::*;
pub(crate) use crate::indexer::*;
pub(crate) use crate::theme::*;

// ... and of the modules split out of the old main.rs.
pub(crate) use crate::app::*;
pub(crate) use crate::dragdrop::*;
pub(crate) use crate::elevate::*;
pub(crate) use crate::messages::*;
pub(crate) use crate::data::*;
pub(crate) use crate::ops::*;
pub(crate) use crate::quick::*;
pub(crate) use crate::search::*;
pub(crate) use crate::settings::*;
pub(crate) use crate::startup::*;
pub(crate) use crate::ui::*;
