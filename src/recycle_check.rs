//! Up-front check whether items can go to the Recycle Bin at all (Windows only).
//!
//! The shell's own decision is internal and not exposed by the `trash` crate, so this reads the
//! same per-volume settings it uses. It is an approximation: whatever it lets through is still
//! caught afterwards by the "bin refused it" fallback in `Action::Delete`.
//!
//! Rules (Windows purges the oldest bin items to make room, so only an item that is bigger than
//! the whole bin is a hard limit):
//!   - drive isn't a fixed drive (network, removable, UNC)  -> no bin, all its items refused
//!   - `NukeOnDelete` = 1 for the volume                    -> bin turned off, all its items refused
//!   - a single item bigger than the volume's capacity      -> only that item is refused
use std::path::{Path, PathBuf};

/// Splits `paths` into (can go to the bin, cannot, reason for the first refusal).
///
/// Slow only when sizes must be summed. `on_item` is called before each item is measured (for
/// the "Name" line of the progress panel); `cancelled` aborts a long scan.
#[cfg(not(windows))]
pub(crate) fn split(
    paths: &[PathBuf],
    _cancelled: &dyn Fn() -> bool,
    _on_item: &dyn Fn(&Path),
) -> (Vec<PathBuf>, Vec<PathBuf>, String) {
    (paths.to_vec(), Vec::new(), String::new())
}

#[cfg(windows)]
pub(crate) use imp::split;

#[cfg(windows)]
mod imp {
    use super::{Path, PathBuf};
    use crate::fsops::{format_size, size_up_to};
    use std::path::{Component, Prefix};
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};

    // Edition 2024: extern blocks are `unsafe extern`. These are plain kernel32 exports.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDriveTypeW(root_path_name: *const u16) -> u32;
        fn GetVolumeNameForVolumeMountPointW(volume_mount_point: *const u16, volume_name: *mut u16, buffer_length: u32) -> i32;
        fn GetDiskFreeSpaceExW(
            directory_name: *const u16,
            free_bytes_available: *mut u64,
            total_bytes: *mut u64,
            total_free_bytes: *mut u64,
        ) -> i32;
    }

    const DRIVE_FIXED: u32 = 3; // removable, network and optical drives have no Recycle Bin

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// "C:\" for a path on a drive letter; None for UNC paths and the like.
    fn drive_root(p: &Path) -> Option<String> {
        match p.components().next()? {
            Component::Prefix(pre) => match pre.kind() {
                Prefix::Disk(l) | Prefix::VerbatimDisk(l) => Some(format!("{}:\\", l as char)),
                _ => None,
            },
            _ => None,
        }
    }

    fn volume_guid(root_w: &[u16]) -> Option<String> {
        let mut buf = [0u16; 64];
        if unsafe { GetVolumeNameForVolumeMountPointW(root_w.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } == 0 {
            return None;
        }
        let end = buf.iter().position(|&c| c == 0)?;
        let s = String::from_utf16_lossy(&buf[..end]); // \\?\Volume{GUID}\
        let (a, b) = (s.find('{')?, s.find('}')?);
        Some(s[a..=b].to_owned())
    }

    fn reg_dword(subkey: &str, name: &str) -> Option<u32> {
        RegKey::predef(HKEY_CURRENT_USER).open_subkey(subkey).ok()?.get_value::<u32, _>(name).ok()
    }

    /// What applies to all items on one volume.
    enum Limit {
        /// The bin can't be used here at all (the reason is shown to the user).
        Refused(String),
        /// No size limit.
        None,
        /// A single item bigger than this many bytes can't be recycled.
        Capacity(u64),
    }

    fn volume_limit(root: Option<&str>) -> Limit {
        let Some(root) = root else {
            return Limit::Refused("This location has no Recycle Bin.".to_owned());
        };
        let root_w = wide(root);
        if unsafe { GetDriveTypeW(root_w.as_ptr()) } != DRIVE_FIXED {
            return Limit::Refused("This drive has no Recycle Bin.".to_owned());
        }
        let Some(guid) = volume_guid(&root_w) else { return Limit::None }; // can't tell: let the bin try
        let key = format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\BitBucket\Volume\{guid}");
        if reg_dword(&key, "NukeOnDelete") == Some(1) {
            return Limit::Refused("The Recycle Bin is turned off for this drive.".to_owned());
        }
        // MaxCapacity is in MB (all bits set = unlimited). Unset means the default of roughly 5 %
        // of the volume, which is an estimate: it can be a little off.
        match reg_dword(&key, "MaxCapacity") {
            Some(u32::MAX) => Limit::None,
            Some(mb) => Limit::Capacity(u64::from(mb) * 1024 * 1024),
            None => {
                let mut total = 0u64;
                let ok = unsafe {
                    GetDiskFreeSpaceExW(root_w.as_ptr(), std::ptr::null_mut(), &mut total, std::ptr::null_mut())
                } != 0;
                if ok { Limit::Capacity(total / 20) } else { Limit::None }
            }
        }
    }

    pub(crate) fn split(
        paths: &[PathBuf],
        cancelled: &dyn Fn() -> bool,
        on_item: &dyn Fn(&Path),
    ) -> (Vec<PathBuf>, Vec<PathBuf>, String) {
        let (mut ok, mut refused, mut reason) = (Vec::new(), Vec::new(), String::new());
        // Selections can span drives (search results, ...): decide per volume.
        let mut groups: Vec<(Option<String>, Vec<PathBuf>)> = Vec::new();
        for p in paths {
            let root = drive_root(p);
            match groups.iter_mut().find(|(r, _)| *r == root) {
                Some((_, v)) => v.push(p.clone()),
                None => groups.push((root, vec![p.clone()])),
            }
        }
        for (root, group) in groups {
            match volume_limit(root.as_deref()) {
                Limit::Refused(r) => {
                    if reason.is_empty() {
                        reason = r;
                    }
                    refused.extend(group);
                }
                Limit::None => ok.extend(group),
                Limit::Capacity(cap) => {
                    for p in group {
                        if cancelled() {
                            return (ok, refused, reason);
                        }
                        on_item(&p);
                        if size_up_to(&p, cap, cancelled) > cap {
                            if reason.is_empty() {
                                reason = format!("Larger than this drive's Recycle Bin allows ({}).", format_size(cap));
                            }
                            refused.push(p);
                        } else {
                            ok.push(p);
                        }
                    }
                }
            }
        }
        (ok, refused, reason)
    }
}
