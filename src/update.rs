//! This file queries GitHub for the newest release and pushes the results through Actions.

use crate::prelude::*;
use semver::Version;
use serde::Deserialize;
use std::time::Duration;

const REPO: &str = "SomewhatCls/FileFly";

/// Shown in the "What's new" popup when the running version has no release (or an empty description).
const NO_NOTES: &str = "This release has no release notes.";

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct UpdateInfo {
    pub(crate) version: String,
    /// The installer asset if the release has one, otherwise the release page.
    pub(crate) url: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum UpdateState {
    Idle,
    Checking,
    UpToDate,
    Available(UpdateInfo),
    Failed(String),
}

/// The "What's new" popup: loading, or what GitHub says changed.
pub(crate) enum WhatsNew {
    Loading,
    Ready { version: String, notes: String },
    Failed(String),
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    assets: Vec<Asset>,
    /// The release description (the changelog).
    body: Option<String>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// Why a release lookup failed. Callers decide what to do from the variant, never from the message text.
#[derive(Debug)]
enum FetchError {
    /// HTTP 404: the requested release (or tag) doesn't exist.
    NotFound,
    /// HTTP 403/429.
    RateLimited,
    /// Transport errors, timeouts, unreadable JSON and any other status.
    Other(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("release not found"),
            Self::RateLimited => f.write_str("GitHub rate limit reached, try again later"),
            Self::Other(msg) => f.write_str(msg),
        }
    }
}

/// `v1.2.3` -> `1.2.3`. Strips exactly one leading `v`.
fn strip_v(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}

/// Runs on a background thread. `which` is `latest` or `tags/<tag>`.
fn fetch_release(which: &str) -> Result<Release, FetchError> {
    ureq::get(&format!("https://api.github.com/repos/{REPO}/releases/{which}"))
        .header("User-Agent", concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .config()
        .timeout_global(Some(Duration::from_secs(8)))
        .build()
        .call()
        .map_err(|e| match e {
            ureq::Error::StatusCode(404) => FetchError::NotFound,
            ureq::Error::StatusCode(403 | 429) => FetchError::RateLimited,
            other => FetchError::Other(other.to_string()),
        })?
        .body_mut()
        .read_json::<Release>()
        .map_err(|e| FetchError::Other(e.to_string()))
}

/// The release notes of the running version. Returns (version, notes).
/// A version without a published release (like a dev build) or with an empty description gets a
/// "no release notes" placeholder; any other failure (network, rate limit, ...) is reported as an error.
fn fetch_release_notes() -> Result<(String, String), String> {
    let current = env!("CARGO_PKG_VERSION");
    match fetch_release(&format!("tags/v{current}")) {
        Ok(rel) => {
            let notes = rel
                .body
                .filter(|b| !b.trim().is_empty())
                .unwrap_or_else(|| NO_NOTES.to_owned());
            Ok((current.to_owned(), notes))
        }
        Err(FetchError::NotFound) => Ok((current.to_owned(), NO_NOTES.to_owned())),
        Err(e) => Err(e.to_string()),
    }
}

/// Runs on a background thread.
/// Ok(None) = newest is installed
fn fetch_latest() -> Result<Option<UpdateInfo>, String> {
    let rel = fetch_release("latest").map_err(|e| match e {
        FetchError::NotFound => "no releases have been published yet".to_owned(),
        other => other.to_string(),
    })?;
    let latest = Version::parse(strip_v(&rel.tag_name))
        .map_err(|e| format!("unreadable version tag '{}': {e}", rel.tag_name))?;
    let current = Version::parse(env!("CARGO_PKG_VERSION")).map_err(|e| e.to_string())?;
    if latest <= current {
        return Ok(None);
    }

    #[cfg(target_os = "windows")]
    let file = rel.assets.iter().find(|a| {
        let n = a.name.to_ascii_lowercase();
        n.ends_with(".msi") || (n.ends_with(".exe") && (n.contains("setup") || n.contains("install")))
    });
    #[cfg(target_os = "linux")]
    let file = rel.assets.iter().find(|a| {
        let n = a.name.to_ascii_lowercase();
        n.ends_with(".appimage")
    });
    // Other targets have no installer to offer: fall back to the release page.
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    let file: Option<&Asset> = None;

    let url = file.map(|a| a.browser_download_url.clone()).unwrap_or(rel.html_url);
    Ok(Some(UpdateInfo { version: latest.to_string(), url }))
}

impl Explorer {
    pub(crate) fn check_for_update(&mut self, announce: bool) {
        if self.update == UpdateState::Checking {
            return;
        }
        self.update = UpdateState::Checking;
        let (tx, ui) = (self.msg_tx.clone(), self.ui.clone());
        std::thread::spawn(move || {
            let _ = tx.send(AppMsg::UpdateChecked { announce, result: fetch_latest() });
            ui.request_repaint();
        });
    }

    /// Opens the "What's new" popup and loads the release notes (only now: nothing contacts GitHub
    /// before the user asks).
    pub(crate) fn open_whats_new(&mut self) {
        // A fetch is already in flight; don't start a second one.
        if matches!(self.whats_new, Some(WhatsNew::Loading)) {
            return;
        }
        self.whats_new = Some(WhatsNew::Loading);
        let (tx, ui) = (self.msg_tx.clone(), self.ui.clone());
        std::thread::spawn(move || {
            let _ = tx.send(AppMsg::ReleaseNotes(fetch_release_notes()));
            ui.request_repaint();
        });
    }

    /// The popup was closed: the button has done its job.
    pub(crate) fn close_whats_new(&mut self) {
        self.whats_new = None;
        self.settings.whats_new_seen = env!("CARGO_PKG_VERSION").to_owned();
        save_settings(&self.settings);
    }

    pub(crate) fn update_checked(
        &mut self,
        announce: bool,
        result: Result<Option<UpdateInfo>, String>,
    ) {
        if let (true, Ok(Some(info))) = (announce, &result) {
            self.tabs[self.active].notify(
                Notice::info(format!("Version {} is available!", info.version))
                    .with_button("Download", Action::DownloadUpdate),
            );
        }
        self.update = match result {
            Ok(None) => UpdateState::UpToDate,
            Ok(Some(info)) => UpdateState::Available(info),
            Err(e) => UpdateState::Failed(e),
        };
    }

    pub(crate) fn open_update_download(&mut self) {
        let UpdateState::Available(info) = &self.update else {
            return;
        };
        // Only hand GitHub links to the browser
        if info.url.starts_with("https://github.com/") {
            self.ui.open_url(egui::OpenUrl::new_tab(&info.url));
        } else {
            self.tabs[self.active].error("Refusing to open a download link that isn't on github.com.");
        }
    }
}
