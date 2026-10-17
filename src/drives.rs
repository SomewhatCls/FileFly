//! Drive enumeration: system, removable and network drives.
//!
//! * Windows talks to the Win32 API directly (`GetDriveTypeW`, `WNetGetConnectionW`, a bus-type query for
//!   USB / SD storage that Windows reports as "fixed"), so mapped network drives are found too.
//! * Everywhere else it uses `sysinfo` and recognises network file systems (nfs, cifs, sshfs, ...).
//!
//! Probing a disconnected network drive can block for tens of seconds, so network volumes are probed
//! on helper threads with a timeout and are shown as "unavailable" when they do not answer.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DriveKind {
    System,
    Removable,
    Network,
}

#[derive(Clone)]
pub struct DriveInfo {
    pub mount_point: PathBuf,
    pub name: String,
    pub total: u64,
    pub available: u64,
    pub kind: DriveKind,
}

/// Lists all drives, sorted by mount point. Network drives are only probed when `include_network` is set.
pub fn load_drives(include_network: bool) -> Vec<DriveInfo> {
    let mut drives = imp::load(include_network);
    drives.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
    drives
}

/// True if `path` lives on a network share (UNC path, mapped drive, or a known network mount).
/// Used to avoid expensive recursive work (folder-size scans, recursive watching) over the network.
pub fn is_network_path(path: &Path) -> bool {
    imp::is_network_path(path)
}

// ------------------------------------------------------------------------------------------------
#[cfg(windows)]
mod imp {
    use super::*;
    use std::ffi::c_void;
    use std::sync::mpsc;
    use std::time::Duration;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLogicalDrives() -> u32;
        fn GetDriveTypeW(root: *const u16) -> u32;
        fn GetDiskFreeSpaceExW(dir: *const u16, avail: *mut u64, total: *mut u64, free: *mut u64) -> i32;
        fn GetVolumeInformationW(
            root: *const u16,
            name: *mut u16,
            name_len: u32,
            serial: *mut u32,
            max_component: *mut u32,
            flags: *mut u32,
            fs_name: *mut u16,
            fs_len: u32,
        ) -> i32;
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            security: *mut c_void,
            disposition: u32,
            flags: u32,
            template: isize,
        ) -> isize;
        fn DeviceIoControl(
            h: isize,
            code: u32,
            in_buf: *const c_void,
            in_len: u32,
            out_buf: *mut c_void,
            out_len: u32,
            returned: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
        fn CloseHandle(h: isize) -> i32;
        fn SetErrorMode(mode: u32) -> u32;
    }
    #[link(name = "mpr")]
    unsafe extern "system" {
        fn WNetGetConnectionW(local: *const u16, remote: *mut u16, len: *mut u32) -> u32;
    }

    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    const DRIVE_REMOTE: u32 = 4;
    const DRIVE_CDROM: u32 = 5;
    const DRIVE_RAMDISK: u32 = 6;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn from_wide(buf: &[u16]) -> String {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    }

    /// (total, available) bytes of the volume, or None if it can't be read (no media, offline).
    fn space(root: &str) -> Option<(u64, u64)> {
        let w = wide(root);
        let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
        let ok = unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut avail, &mut total, &mut free) };
        (ok != 0).then_some((total, avail))
    }

    /// Like `space`, but gives up after a couple of seconds (disconnected network drives can hang).
    fn space_with_timeout(root: &str) -> Option<(u64, u64)> {
        let (tx, rx) = mpsc::channel();
        let root = root.to_owned();
        std::thread::spawn(move || {
            let _ = tx.send(space(&root));
        });
        rx.recv_timeout(Duration::from_millis(2500)).ok().flatten()
    }

    fn volume_label(root: &str) -> String {
        let w = wide(root);
        let mut name = [0u16; 261];
        let ok = unsafe {
            GetVolumeInformationW(
                w.as_ptr(),
                name.as_mut_ptr(),
                name.len() as u32,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
            )
        };
        if ok != 0 {
            from_wide(&name)
        } else {
            String::new()
        }
    }

    /// "\\server\share" behind a mapped drive letter like "Z:".
    fn remote_name(letter: char) -> Option<String> {
        let local = wide(&format!("{letter}:"));
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let rc = unsafe { WNetGetConnectionW(local.as_ptr(), buf.as_mut_ptr(), &mut len) };
        (rc == 0).then(|| from_wide(&buf)).filter(|s| !s.is_empty())
    }

    /// USB / FireWire / SD / MMC storage is reported as "fixed" by Windows; ask the storage stack.
    fn is_external_bus(letter: char) -> bool {
        #[repr(C)]
        struct Query {
            property_id: u32, // StorageDeviceProperty
            query_type: u32,  // PropertyStandardQuery
            extra: [u8; 4],
        }
        let path = wide(&format!("\\\\.\\{letter}:"));
        unsafe {
            // Access 0: enough to ask for properties, needs no administrator rights.
            let h = CreateFileW(path.as_ptr(), 0, 3, std::ptr::null_mut(), 3, 0, 0);
            if h == -1 || h == 0 {
                return false;
            }
            let q = Query { property_id: 0, query_type: 0, extra: [0; 4] };
            let mut buf = [0u8; 512];
            let mut returned = 0u32;
            let ok = DeviceIoControl(
                h,
                0x002D_1400, // IOCTL_STORAGE_QUERY_PROPERTY
                &q as *const Query as *const c_void,
                std::mem::size_of::<Query>() as u32,
                buf.as_mut_ptr() as *mut c_void,
                buf.len() as u32,
                &mut returned,
                std::ptr::null_mut(),
            );
            CloseHandle(h);
            if ok == 0 || returned < 32 {
                return false;
            }
            // STORAGE_DEVICE_DESCRIPTOR: RemovableMedia at byte 10, BusType at byte 28.
            let removable_media = buf[10] != 0;
            let bus = u32::from_le_bytes([buf[28], buf[29], buf[30], buf[31]]);
            removable_media || matches!(bus, 4 | 7 | 12 | 13) // 1394, USB, SD, MMC
        }
    }

    pub fn load(include_network: bool) -> Vec<DriveInfo> {
        unsafe {
            SetErrorMode(1); // SEM_FAILCRITICALERRORS: no "insert a disk" pop-ups for empty readers
        }
        let mask = unsafe { GetLogicalDrives() };
        let mut out = Vec::new();
        for i in 0..26u32 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let letter = (b'A' + i as u8) as char;
            let root = format!("{letter}:\\");
            let rootw = wide(&root);
            let ty = unsafe { GetDriveTypeW(rootw.as_ptr()) };
            let (kind, label) = match ty {
                DRIVE_REMOTE => {
                    if !include_network {
                        continue;
                    }
                    let remote = remote_name(letter).unwrap_or_default();
                    let share = remote.rsplit('\\').next().unwrap_or("").to_owned();
                    let server = remote.trim_start_matches('\\').split('\\').next().unwrap_or("").to_owned();
                    let label = match (share.is_empty(), server.is_empty()) {
                        (false, false) => format!("{share} (\\\\{server})"),
                        (false, true) => share,
                        _ => "Network Drive".to_owned(),
                    };
                    (DriveKind::Network, label)
                }
                DRIVE_REMOVABLE | DRIVE_CDROM => (DriveKind::Removable, String::new()),
                DRIVE_FIXED => {
                    if is_external_bus(letter) {
                        (DriveKind::Removable, String::new())
                    } else {
                        (DriveKind::System, String::new())
                    }
                }
                DRIVE_RAMDISK => (DriveKind::System, String::new()),
                _ => continue,
            };
            let probed = if kind == DriveKind::Network { space_with_timeout(&root) } else { space(&root) };
            // An empty card reader or optical drive has no media: nothing to show.
            if probed.is_none() && kind == DriveKind::Removable {
                continue;
            }
            let (total, available) = probed.unwrap_or((0, 0));
            let name = if kind == DriveKind::Network {
                label
            } else {
                let l = volume_label(&root);
                if !l.is_empty() {
                    l
                } else if kind == DriveKind::Removable {
                    "Removable Disk".to_owned()
                } else {
                    "Local Disk".to_owned()
                }
            };
            out.push(DriveInfo { mount_point: PathBuf::from(root), name, total, available, kind });
        }
        out
    }

    pub fn is_network_path(path: &Path) -> bool {
        let s = path.to_string_lossy();
        if s.starts_with("\\\\?\\UNC\\") {
            return true;
        }
        if s.starts_with("\\\\?\\") || s.starts_with("\\\\.\\") {
            return false;
        }
        if s.starts_with("\\\\") {
            return true;
        }
        let b = s.as_bytes();
        if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
            let root = wide(&format!("{}:\\", b[0] as char));
            return unsafe { GetDriveTypeW(root.as_ptr()) } == DRIVE_REMOTE;
        }
        false
    }
}

// ------------------------------------------------------------------------------------------------
#[cfg(not(windows))]
mod imp {
    use super::*;
    use std::sync::RwLock;
    use sysinfo::Disks;

    /// Mount points of network file systems seen by the last `load`.
    static NETWORK_ROOTS: RwLock<Vec<PathBuf>> = RwLock::new(Vec::new());

    fn is_network_fs(fs: &str) -> bool {
        let fs = fs.to_lowercase();
        matches!(
            fs.as_str(),
            "nfs" | "nfs4" | "cifs" | "smb" | "smbfs" | "smb3" | "afpfs" | "afs" | "ncpfs" | "9p" | "davfs" | "webdav"
                | "ceph" | "glusterfs" | "lustre" | "beegfs"
        ) || fs.starts_with("fuse.sshfs")
            || fs.starts_with("fuse.rclone")
            || fs.starts_with("fuse.gvfs")
            || fs.starts_with("fuse.smb")
            || fs.starts_with("fuse.davfs")
            || fs.starts_with("fuse.nfs")
    }

    pub fn load(include_network: bool) -> Vec<DriveInfo> {
        let mut out = Vec::new();
        let mut net_roots = Vec::new();
        for d in Disks::new_with_refreshed_list().list() {
            let mount = d.mount_point().to_path_buf();
            let fs = d.file_system().to_string_lossy().into_owned();
            let kind = if is_network_fs(&fs) {
                DriveKind::Network
            } else if d.is_removable() || mount.starts_with("/media") || mount.starts_with("/run/media") {
                DriveKind::Removable
            } else {
                DriveKind::System
            };
            if kind == DriveKind::Network {
                net_roots.push(mount.clone());
                if !include_network {
                    continue;
                }
            }
            out.push(DriveInfo {
                name: d.name().to_string_lossy().into_owned(),
                mount_point: mount,
                total: d.total_space(),
                available: d.available_space(),
                kind,
            });
        }
        if let Ok(mut g) = NETWORK_ROOTS.write() {
            *g = net_roots;
        }
        out
    }

    pub fn is_network_path(path: &Path) -> bool {
        NETWORK_ROOTS.read().map(|r| r.iter().any(|root| path.starts_with(root))).unwrap_or(false)
    }
}
