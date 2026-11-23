//! This file queries GitHub for the newest release and pushes the results through Actions.

use crate::prelude::*;
use semver::Version;
use serde::Deserialize;
use std::time::Duration;

const REPO: &str = "SomewhatCls/FileFly";

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

/// Runs on a background thread. `which` is `latest` or `tags/<tag>`.
fn fetch_release(which: &str) -> Result<Release, String> {
    ureq::get(&format!("https://api.github.com/repos/{REPO}/releases/{which}"))
        .header("User-Agent", concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .config()
        .timeout_global(Some(Duration::from_secs(8)))
        .build()
        .call()
        .map_err(|e| match e {
            ureq::Error::StatusCode(404) => "no releases have been published yet".to_owned(),
            ureq::Error::StatusCode(403 | 429) => "GitHub rate limit reached, try again later".to_owned(),
            other => other.to_string(),
        })?
        .body_mut()
        .read_json::<Release>()
        .map_err(|e| e.to_string())
}

/// The release notes of the running version (the newest release's when this build has none, like a dev build).
/// Returns (version, notes).
fn fetch_release_notes() -> Result<(String, String), String> {
    let current = env!("CARGO_PKG_VERSION");
    // Only "this version has no release" (a dev build) falls back to the newest one; a failing network
    // is reported at once instead of waiting for a second timeout.
    let rel = match fetch_release(&format!("tags/v{current}")) {
        Err(e) if e.contains("no releases") => fetch_release("latest")?,
        other => other?,
    };
    let version = rel.tag_name.trim_start_matches('v').to_owned();
    Ok((version, rel.body.unwrap_or_default()))
}

/// Runs on a background thread.
/// Ok(None) = newest is installed
fn fetch_latest() -> Result<Option<UpdateInfo>, String> {
    let rel = fetch_release("latest")?;
    let latest = Version::parse(rel.tag_name.trim_start_matches('v'))
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