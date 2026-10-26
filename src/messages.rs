//! Messages coming from background threads and UI actions.

use crate::prelude::*;

pub(crate) enum AppMsg {
    Listing {
        tab: u64,
        seq: u64,
        result: Result<Vec<Entry>, String>,
    },
    Drives(Vec<DriveInfo>),
    /// A background file operation (see `run_op`) finished.
    Job(u64, Result<String, String>),
    Stats(u64, FolderStats),
    /// A failed job could be retried as administrator; `msg` is the error text it reports.
    AdminOffer { msg: String, retry: AdminRetry },
    Trash {
        tab: u64,
        seq: u64,
        result: Result<Vec<trash_bin::TrashRow>, String>,
    },
    /// A zip file was unpacked for browsing (see `fsops::mount_archive`); `target` is the path
    /// (through the zip) the tab should now open.
    ArchiveReady {
        tab: u64,
        zip: PathBuf,
        target: PathBuf,
        result: Result<(), String>,
    },
    /// Compress / Extract finished: the new items, what went wrong, whether the first new item
    /// should go straight into rename mode (a new zip file does, like in Explorer), and whether it
    /// should be opened in a new tab ("Extract to…").
    Made {
        tab: u64,
        created: Vec<PathBuf>,
        errors: Vec<String>,
        rename: bool,
        open: bool,
    },
}

pub(crate) enum Action {
    Navigate(PathBuf),
    NewTabAt(PathBuf),
    NewTab,
    CloseTab(usize),
    ClosePane(u64),
    OpenTerminal(PathBuf),
    SwitchTab(usize),
    StepTab(i32),
    Back,
    Forward,
    Up,
    Refresh,
    GoToAddress(String),
    FocusAddress,
    FocusSearch,
    Open(PathBuf),
    Copy(Vec<PathBuf>),
    Cut(Vec<PathBuf>),
    CopyPath(Vec<PathBuf>),
    Paste,
    /// Files dragged onto a folder / pane / sidebar entry: copy them there.
    DropCopy(Vec<PathBuf>, PathBuf),
    Rename(PathBuf),
    /// The "Try as administrator" button of the current error.
    RetryAsAdmin,
    /// "Change folder icon…": pick an .ico for the folder.
    ChangeFolderIcon(PathBuf),
    /// Back to the normal icon.
    ResetFolderIcon(PathBuf),
    /// Run a program elevated.
    RunAsAdmin(PathBuf),
    /// Open a folder in a new, elevated window.
    OpenFolderAsAdmin(PathBuf),
    CommitRename(PathBuf, String),
    CancelRename,
    Delete(Vec<PathBuf>),
    NewFolder,
    NewFile,
    Properties(PathBuf),
    OpenContaining(PathBuf),
    PeaOpen(PathBuf),
    PeaAdd(PathBuf),
    /// "Compress to ZIP file": zip the items next to themselves.
    Compress(Vec<PathBuf>),
    /// "Extract all": unpack each zip into a new folder named after it.
    Extract(Vec<PathBuf>),
    /// "Extract to…": ask for a destination folder (see picker.rs), then unpack there.
    ExtractTo(Vec<PathBuf>),
    ToggleHidden,
    OpenSettings,
    OpenRecycleBin,
    PinQuick(PathBuf),
    UnpinQuick(PathBuf),
    MoveSelection(i32),
    SelectEdge(bool),
    TrashRestoreSelected,
    TrashPurgeSelected,
    TrashEmpty,
    TrashSelectAll,
    SelectAll,
}
