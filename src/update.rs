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

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// Runs on a background thread.
/// Ok(None) = newest is installed
fn fetch_latest() -> Result<Option<UpdateInfo>, String> {
    let rel: Release = ureq::get(&format!("https://api.github.com/repos/{REPO}/releases/latest"))
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
        .map_err(|e| e.to_string())?;

    let latest = Version::parse(rel.tag_name.trim_start_matches('v'))
        .map_err(|e| format!("unreadable version tag '{}': {e}", rel.tag_name))?;
    let current = Version::parse(env!("CARGO_PKG_VERSION")).map_err(|e| e.to_string())?;
    if latest <= current {
        return Ok(None);
    }

    let installer = rel.assets.iter().find(|a| {
        let n = a.name.to_ascii_lowercase();
        n.ends_with(".msi") || (n.ends_with(".exe") && (n.contains("setup") || n.contains("install")))
    });
    let url = installer.map(|a| a.browser_download_url.clone()).unwrap_or(rel.html_url);
    Ok(Some(UpdateInfo { version: latest.to_string(), url }))
}

impl Explorer {
    pub(crate) fn check_for_update(&mut self) {
        if self.update == UpdateState::Checking {
            return;
        }
        self.update = UpdateState::Checking;
        let (tx, ui) = (self.msg_tx.clone(), self.ui.clone());
        std::thread::spawn(move || {
            let _ = tx.send(AppMsg::UpdateChecked(fetch_latest()));
            ui.request_repaint();
        });
    }

    pub(crate) fn update_checked(&mut self, result: Result<Option<UpdateInfo>, String>) {
        self.update = match result {
            Ok(None) => UpdateState::UpToDate,
            Ok(Some(info)) => UpdateState::Available(info),
            Err(e) => UpdateState::Failed(e),
        };
    }

    pub(crate) fn open_update_download(&mut self) {
        if let UpdateState::Available(info) = &self.update {
            // Only hand GitHub links to the browser
            if info.url.starts_with("https://github.com/") {
                self.ui.open_url(egui::OpenUrl::new_tab(&info.url));
            }
        }
    }
}