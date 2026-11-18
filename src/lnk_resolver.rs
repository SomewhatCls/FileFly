//! Resolves a .lnk file to its target and shows that item in its folder.

use crate::prelude::*;

pub(crate) fn is_lnk(p: &Path) -> bool {
    p.extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk"))
}

/// The file or folder a .lnk points to. Runs on a background thread.
#[cfg(windows)]
fn resolve_shortcut(lnk: &Path) -> Result<PathBuf, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // The path travels in an environment variable and the output is
    // forced to UTF-8.
    let out = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Console]::OutputEncoding=[Text.Encoding]::UTF8; \
             (New-Object -ComObject WScript.Shell).CreateShortcut($env:FILEFLY_LNK).TargetPath",
        ])
        .env("FILEFLY_LNK", lnk)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("could not start PowerShell: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let target = text.trim();
    if target.is_empty() {
        return Err("this shortcut doesn't point to a file or folder".to_owned());
    }
    let target = PathBuf::from(target);
    if !target.exists() {
        return Err(format!("the target no longer exists ({})", target.display()));
    }
    Ok(target)
}

#[cfg(not(windows))]
fn resolve_shortcut(_: &Path) -> Result<PathBuf, String> {
    Err("shortcuts are only supported on Windows".to_owned())
}

impl Explorer {
    pub(crate) fn open_shortcut_location(&mut self, ti: usize, lnk: PathBuf) {
        let tab = self.tabs[ti].id;
        let (tx, ui) = (self.msg_tx.clone(), self.ui.clone());
        std::thread::spawn(move || {
            let _ = tx.send(AppMsg::ShortcutTarget { tab, result: resolve_shortcut(&lnk) });
            ui.request_repaint();
        });
    }

    pub(crate) fn shortcut_resolved(&mut self, tab: u64, result: Result<PathBuf, String>) {
        let Some(ti) = self.tabs.iter().position(|t| t.id == tab) else { return };
        match result {
            Ok(target) => {
                self.activate_tab(ti);
                self.apply(Action::OpenContaining(target));
            }
            Err(e) => {
                self.tabs[ti].error(format!("Could not open the shortcut's location: {e}."));
            }
        }
    }
}