//! Drive enumeration: system, removable and network drives, plus a background watcher that keeps the
//! list up to date in real time.
//!
//! * Windows talks to the Win32 API directly (`GetDriveTypeW`, `WNetGetConnectionW`, a bus-type query for
//!   USB / SD storage that Windows reports as "fixed"), so mapped network drives are found too. WSL2
//!   distributions (`\\wsl$\<distro>`) are not drive letters; they are read from the registry and listed
//!   as network locations.
//! * Everywhere else it uses `sysinfo` and recognises network file systems (nfs, cifs, sshfs, ...).
//!
//! Local and removable drives are polled every second on their own thread, so a USB stick shows up and
//! gets its real volume name as soon as Windows has read it, and free space follows live. Network
//! drives are handled on a second thread because probing a disconnected one can block for tens of
//! seconds; it is probed with a timeout and the last known size is kept for slow shares.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DriveKind {
    System,
    Removable,
    Network,
}

#[derive(Clone, PartialEq)]
pub struct DriveInfo {
    pub mount_point: PathBuf,
    pub name: String,
    pub total: u64,
    pub available: u64,
    pub kind: DriveKind,
}

/// Lists all drives once, sorted by mount point. Network drives are only probed when `include_network` is set.
pub fn load_drives(include_network: bool) -> Vec<DriveInfo> {
    let mut drives = imp::load_local();
    if include_network {
        drives.extend(imp::load_network());
    }
    drives.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
    drives
}

/// True if `path` lives on a network share (UNC path, mapped drive, or a known network mount).
/// Used to avoid expensive recursive work (folder-size scans, recursive watching) over the network.
pub fn is_network_path(path: &Path) -> bool {
    imp::is_network_path(path)
}

/// True for the root of a WSL distribution share (`\\wsl$\Ubuntu`, `\\wsl.localhost\Ubuntu`).
pub fn is_wsl_path(path: &Path) -> bool {
    let s = path.to_string_lossy().to_lowercase();
    s.starts_with("\\\\wsl$\\") || s.starts_with("\\\\wsl.localhost\\")
}

// ------------------------------------------------------------------------------------------------
// Real-time watcher

static INCLUDE_NETWORK: AtomicBool = AtomicBool::new(true);
static RESCAN_LOCAL: AtomicBool = AtomicBool::new(false);
static RESCAN_NET: AtomicBool = AtomicBool::new(false);
static STARTED: AtomicBool = AtomicBool::new(false);

const LOCAL_TICK: Duration = Duration::from_millis(1000);
const NET_TICK: Duration = Duration::from_millis(2000);
/// Network shares are re-probed at least this often (free space), even when nothing was (un)mapped.
const NET_REFRESH: Duration = Duration::from_secs(10);

#[derive(Default)]
struct Shared {
    local: Vec<DriveInfo>,
    network: Vec<DriveInfo>,
}

/// Whether network drives (and WSL distributions) are part of the list the watcher reports.
pub fn set_include_network(on: bool) {
    if INCLUDE_NETWORK.swap(on, Ordering::SeqCst) != on {
        RESCAN_NET.store(true, Ordering::SeqCst);
    }
}

/// Asks the watcher to re-read everything and report it again (within about a second).
pub fn rescan() {
    RESCAN_LOCAL.store(true, Ordering::SeqCst);
    RESCAN_NET.store(true, Ordering::SeqCst);
}

/// Starts the background watcher (once). `notify` gets the complete, sorted drive list whenever
/// anything about a drive changes: a drive appears or disappears, its volume name arrives, its free
/// space changes. It is called from the watcher threads, so it should only hand the list over.
pub fn start_watcher(notify: impl Fn(Vec<DriveInfo>) + Send + Sync + 'static) {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let shared = Arc::new(Mutex::new(Shared::default()));
    let notify: Arc<dyn Fn(Vec<DriveInfo>) + Send + Sync> = Arc::new(notify);

    let publish = {
        let (shared, notify) = (shared.clone(), notify.clone());
        Arc::new(move || {
            let mut all = {
                let g = shared.lock().unwrap_or_else(|e| e.into_inner());
                let mut v = g.local.clone();
                if INCLUDE_NETWORK.load(Ordering::SeqCst) {
                    v.extend(g.network.iter().cloned());
                }
                v
            };
            all.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
            notify(all);
        })
    };

    // Local + removable drives: cheap calls only, never touches the network.
    {
        let (shared, publish) = (shared.clone(), publish.clone());
        std::thread::Builder::new()
            .name("drive-watch-local".into())
            .spawn(move || {
                let mut last: Option<Vec<DriveInfo>> = None;
                loop {
                    let force = RESCAN_LOCAL.swap(false, Ordering::SeqCst);
                    let now = imp::load_local();
                    if force || last.as_ref() != Some(&now) {
                        shared.lock().unwrap_or_else(|e| e.into_inner()).local = now.clone();
                        last = Some(now);
                        publish();
                    }
                    std::thread::sleep(LOCAL_TICK);
                }
            })
            .ok();
    }

    // Network drives and WSL distributions: may block, so they have a thread of their own.
    {
        let (shared, publish) = (shared, publish);
        std::thread::Builder::new()
            .name("drive-watch-net".into())
            .spawn(move || {
                let mut last: Option<Vec<DriveInfo>> = None;
                let mut last_sig = String::new();
                let mut last_probe = Instant::now();
                let mut probed_once = false;
                loop {
                    let force = RESCAN_NET.swap(false, Ordering::SeqCst);
                    if INCLUDE_NETWORK.load(Ordering::SeqCst) {
                        let sig = imp::network_signature();
                        if force || !probed_once || sig != last_sig || last_probe.elapsed() >= NET_REFRESH {
                            last_sig = sig;
                            last_probe = Instant::now();
                            probed_once = true;
                            let now = imp::load_network();
                            if force || last.as_ref() != Some(&now) {
                                shared.lock().unwrap_or_else(|e| e.into_inner()).network = now.clone();
                                last = Some(now);
                                publish();
                            }
                        }
                    } else {
                        // Switched off: drop the network entries from the reported list.
                        probed_once = false;
                        last = None;
                        if force {
                            publish();
                        }
                    }
                    std::thread::sleep(NET_TICK);
                }
            })
            .ok();
    }
}

// ------------------------------------------------------------------------------------------------
#[cfg(windows)]
mod imp {
    use super::*;
    use std::ffi::c_void;
    use std::sync::atomic::AtomicU32;
    use std::sync::mpsc;

    /// Drive letters (bit 0 = A) whose network probe thread has not returned yet.
    static NET_BUSY: AtomicU32 = AtomicU32::new(0);
    /// Last successful (total, available) of each network drive; shown while a probe is slow or hung.
    static NET_CACHE: Mutex<Vec<(char, (u64, u64))>> = Mutex::new(Vec::new());
    /// Whether each fixed drive letter sits on an external bus. Only answers that were obtained are
    /// stored: right after a USB disk appears the query can still fail, and that must not stick.
    static BUS_CACHE: Mutex<Vec<(char, bool)>> = Mutex::new(Vec::new());

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
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegOpenKeyExW(key: isize, sub: *const u16, options: u32, sam: u32, out: *mut isize) -> i32;
        fn RegEnumKeyExW(
            key: isize,
            index: u32,
            name: *mut u16,
            name_len: *mut u32,
            reserved: *mut u32,
            class: *mut u16,
            class_len: *mut u32,
            last_write: *mut u64,
        ) -> i32;
        fn RegQueryValueExW(
            key: isize,
            name: *const u16,
            reserved: *mut u32,
            ty: *mut u32,
            data: *mut u8,
            data_len: *mut u32,
        ) -> i32;
        fn RegCloseKey(key: isize) -> i32;
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
    /// A drive whose previous probe is still stuck is not probed again (no pile of blocked threads);
    /// the last known size is reported instead.
    fn space_with_timeout(letter: char, root: &str) -> Option<(u64, u64)> {
        let bit = 1u32 << (letter as u8 - b'A');
        let cached = || {
            NET_CACHE
                .lock()
                .ok()
                .and_then(|c| c.iter().find(|e| e.0 == letter).map(|e| e.1))
        };
        if NET_BUSY.fetch_or(bit, Ordering::SeqCst) & bit != 0 {
            return cached();
        }
        let (tx, rx) = mpsc::channel();
        let root = root.to_owned();
        std::thread::spawn(move || {
            let r = space(&root);
            if let Ok(mut c) = NET_CACHE.lock() {
                c.retain(|e| e.0 != letter);
                if let Some(v) = r {
                    c.push((letter, v));
                }
            }
            NET_BUSY.fetch_and(!bit, Ordering::SeqCst);
            let _ = tx.send(r);
        });
        rx.recv_timeout(Duration::from_millis(2500)).ok().flatten().or_else(cached)
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
    /// None: the query failed (device not ready yet), so the answer must not be remembered.
    fn query_external_bus(letter: char) -> Option<bool> {
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
                return None;
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
                return None;
            }
            // STORAGE_DEVICE_DESCRIPTOR: RemovableMedia at byte 10, BusType at byte 28.
            let removable_media = buf[10] != 0;
            let bus = u32::from_le_bytes([buf[28], buf[29], buf[30], buf[31]]);
            Some(removable_media || matches!(bus, 4 | 7 | 12 | 13)) // 1394, USB, SD, MMC
        }
    }

    fn is_external_bus(letter: char) -> bool {
        if let Some(v) = BUS_CACHE.lock().ok().and_then(|c| c.iter().find(|e| e.0 == letter).map(|e| e.1)) {
            return v;
        }
        match query_external_bus(letter) {
            Some(v) => {
                if let Ok(mut c) = BUS_CACHE.lock() {
                    c.push((letter, v));
                }
                v
            }
            None => false,
        }
    }

    /// Forgets the bus type of letters that are no longer mounted (the letter may come back as another device).
    fn forget_gone_letters(mask: u32) {
        if let Ok(mut c) = BUS_CACHE.lock() {
            c.retain(|e| mask & (1 << (e.0 as u8 - b'A')) != 0);
        }
    }

    /// Installed WSL distributions (Explorer lists them under "Linux" as `\\wsl$\<name>`). They are
    /// read from the registry: nothing is started and no process is spawned.
    fn wsl_distros() -> Vec<String> {
        const HKCU: isize = -2147483647; // HKEY_CURRENT_USER, sign-extended like the C macro
        const KEY_READ: u32 = 0x0002_0019;
        const REG_SZ: u32 = 1;
        let mut out = Vec::new();
        unsafe {
            let base = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Lxss");
            let mut root = 0isize;
            if RegOpenKeyExW(HKCU, base.as_ptr(), 0, KEY_READ, &mut root) != 0 {
                return out;
            }
            let mut i = 0u32;
            loop {
                let mut name = [0u16; 256];
                let mut len = name.len() as u32;
                let rc = RegEnumKeyExW(
                    root,
                    i,
                    name.as_mut_ptr(),
                    &mut len,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
                if rc != 0 {
                    break; // ERROR_NO_MORE_ITEMS (or a real error: stop either way)
                }
                i += 1;
                let sub = wide(&from_wide(&name[..(len as usize).min(name.len())]));
                let mut key = 0isize;
                if RegOpenKeyExW(root, sub.as_ptr(), 0, KEY_READ, &mut key) != 0 {
                    continue;
                }
                let value = wide("DistributionName");
                let mut buf = [0u16; 256];
                let mut bytes = (buf.len() * 2) as u32;
                let mut ty = 0u32;
                let rc = RegQueryValueExW(
                    key,
                    value.as_ptr(),
                    std::ptr::null_mut(),
                    &mut ty,
                    buf.as_mut_ptr() as *mut u8,
                    &mut bytes,
                );
                RegCloseKey(key);
                if rc == 0 && ty == REG_SZ {
                    let n = from_wide(&buf[..(bytes as usize / 2).min(buf.len())]);
                    // Docker Desktop's internal distributions are not meant to be browsed.
                    if !n.is_empty() && !n.to_lowercase().starts_with("docker-desktop") {
                        out.push(n);
                    }
                }
            }
            RegCloseKey(root);
        }
        out.sort();
        out.dedup();
        out
    }

    /// System and removable drives (everything that is not a network drive).
    pub fn load_local() -> Vec<DriveInfo> {
        unsafe {
            SetErrorMode(1); // SEM_FAILCRITICALERRORS: no "insert a disk" pop-ups for empty readers
        }
        let mask = unsafe { GetLogicalDrives() };
        forget_gone_letters(mask);
        let mut out = Vec::new();
        for i in 0..26u32 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let letter = (b'A' + i as u8) as char;
            let root = format!("{letter}:\\");
            let rootw = wide(&root);
            let ty = unsafe { GetDriveTypeW(rootw.as_ptr()) };
            let kind = match ty {
                DRIVE_REMOVABLE | DRIVE_CDROM => DriveKind::Removable,
                DRIVE_FIXED => {
                    if is_external_bus(letter) {
                        DriveKind::Removable
                    } else {
                        DriveKind::System
                    }
                }
                DRIVE_RAMDISK => DriveKind::System,
                _ => continue, // network drives are `load_network`'s business
            };
            let probed = space(&root);
            // An empty card reader or optical drive has no media: nothing to show. The same goes for a
            // USB disk that Windows has not finished mounting; it appears as soon as it is ready.
            if probed.is_none() && kind == DriveKind::Removable {
                continue;
            }
            let (total, available) = probed.unwrap_or((0, 0));
            let l = volume_label(&root);
            let name = if !l.is_empty() {
                l
            } else if kind == DriveKind::Removable {
                "Removable Disk".to_owned()
            } else {
                "Local Disk".to_owned()
            };
            out.push(DriveInfo { mount_point: PathBuf::from(root), name, total, available, kind });
        }
        out
    }

    /// Mapped network drives and WSL distributions.
    pub fn load_network() -> Vec<DriveInfo> {
        unsafe {
            SetErrorMode(1);
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
            if unsafe { GetDriveTypeW(rootw.as_ptr()) } != DRIVE_REMOTE {
                continue;
            }
            let remote = remote_name(letter).unwrap_or_default();
            let share = remote.rsplit('\\').next().unwrap_or("").to_owned();
            let server = remote.trim_start_matches('\\').split('\\').next().unwrap_or("").to_owned();
            let name = match (share.is_empty(), server.is_empty()) {
                (false, false) => format!("{share} (\\\\{server})"),
                (false, true) => share,
                _ => "Network Drive".to_owned(),
            };
            let (total, available) = space_with_timeout(letter, &root).unwrap_or((0, 0));
            out.push(DriveInfo { mount_point: PathBuf::from(root), name, total, available, kind: DriveKind::Network });
        }
        // WSL2 distributions are shares, not drive letters. Their size is not probed: touching the
        // share would start the distribution.
        for distro in wsl_distros() {
            out.push(DriveInfo {
                mount_point: PathBuf::from(format!("\\\\wsl$\\{distro}")),
                name: distro,
                total: 0,
                available: 0,
                kind: DriveKind::Network,
            });
        }
        out
    }

    /// A cheap fingerprint of "which network locations exist" (mapped letters with their targets, WSL
    /// distributions). When it changes the watcher probes right away instead of waiting.
    pub fn network_signature() -> String {
        let mask = unsafe { GetLogicalDrives() };
        let mut sig = String::new();
        for i in 0..26u32 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let letter = (b'A' + i as u8) as char;
            let rootw = wide(&format!("{letter}:\\"));
            if unsafe { GetDriveTypeW(rootw.as_ptr()) } == DRIVE_REMOTE {
                sig.push_str(&format!("{letter}={};", remote_name(letter).unwrap_or_default()));
            }
        }
        for d in wsl_distros() {
            sig.push_str(&format!("wsl={d};"));
        }
        sig
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

    pub fn load_local() -> Vec<DriveInfo> {
        load(false)
    }

    pub fn load_network() -> Vec<DriveInfo> {
        load(true).into_iter().filter(|d| d.kind == DriveKind::Network).collect()
    }

    /// No cheap fingerprint here: the watcher falls back to its periodic refresh.
    pub fn network_signature() -> String {
        String::new()
    }

    fn load(include_network: bool) -> Vec<DriveInfo> {
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
