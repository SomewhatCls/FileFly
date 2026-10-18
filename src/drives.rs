//! Drive enumeration: system, removable and network drives, polled live.
//!
//! * Windows talks to the Win32 API directly (`GetDriveTypeW`, `WNetGetConnectionW`, a bus-type query for
//!   USB / SD storage that Windows reports as "fixed"), so mapped network drives are found too.
//! * Everywhere else it uses `sysinfo` and recognises network file systems (nfs, cifs, sshfs, ...).
//!
//! `spawn_watcher` starts a background thread that re-checks the drives about once a second and calls back
//! whenever something changed (a drive appeared / vanished, a volume label arrived, free space moved).
//! Probing a disconnected network drive can block for tens of seconds, so network volumes are probed on
//! helper threads: a slow share never stalls the watcher, and shows as "unavailable" while it does not answer.
//!
//! WSL distributions are shown with their proper `\\wsl.localhost\<distro>\` path (legacy `\\wsl$\...`
//! paths and drive letters mapped to a WSL share are normalised to it).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, Thread};
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
    /// Windows: the drive letter root (`Z:\`) behind a `mount_point` that is a UNC path (a drive mapped to a
    /// WSL share), so entries pinned under the letter still match this drive.
    pub letter_root: Option<PathBuf>,
}

/// Handle to the watcher thread started by `spawn_watcher`.
pub struct DriveWatchCtl {
    network: AtomicBool,
    focused: AtomicBool,
    thread: Mutex<Option<Thread>>,
}

impl DriveWatchCtl {
    /// Whether network drives are listed (and therefore probed).
    pub fn set_network(&self, on: bool) {
        self.network.store(on, Ordering::Relaxed);
    }

    /// The watcher polls less often while the window is in the background.
    pub fn set_focused(&self, on: bool) {
        let was = self.focused.swap(on, Ordering::Relaxed);
        if on && !was {
            self.poke(); // back in the foreground: catch up immediately
        }
    }

    /// Polls again right now instead of waiting for the next tick.
    pub fn poke(&self) {
        let t = self.thread.lock().ok().and_then(|g| g.clone());
        if let Some(t) = t {
            t.unpark();
        }
    }
}

/// Starts the watcher. `on_change` receives the full, sorted drive list the first time and after every change;
/// returning `false` from it stops the thread.
pub fn spawn_watcher(
    include_network: bool,
    on_change: impl Fn(Vec<DriveInfo>) -> bool + Send + 'static,
) -> Arc<DriveWatchCtl> {
    let ctl = Arc::new(DriveWatchCtl {
        network: AtomicBool::new(include_network),
        focused: AtomicBool::new(true),
        thread: Mutex::new(None),
    });
    let c = ctl.clone();
    let spawned = thread::Builder::new().name("drive-watcher".into()).spawn(move || {
        let mut state = imp::State::default();
        let mut last: Option<Vec<DriveInfo>> = None;
        loop {
            let (mut list, pending) = imp::poll(&mut state, c.network.load(Ordering::Relaxed));
            list.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
            if last.as_ref() != Some(&list) {
                if std::env::var_os("SLOPEXPLORE_DRIVE_LOG").is_some() {
                    log_drives(&list);
                }
                last = Some(list.clone());
                if !on_change(list) {
                    return;
                }
            }
            let wait = if pending {
                Duration::from_millis(250)
            } else if c.focused.load(Ordering::Relaxed) {
                Duration::from_secs(1)
            } else {
                Duration::from_secs(3)
            };
            thread::park_timeout(wait);
        }
    });
    if let Ok(handle) = spawned {
        if let Ok(mut g) = ctl.thread.lock() {
            *g = Some(handle.thread().clone());
        }
    }
    ctl
}

/// Debug aid: with the environment variable `SLOPEXPLORE_DRIVE_LOG` set, every change the watcher sees is
/// appended to `slopexplore_drives.log` in the temp folder.
fn log_drives(list: &[DriveInfo]) {
    use std::io::Write;
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let items: Vec<String> = list
        .iter()
        .map(|d| format!("{} \"{}\" {:?} {}/{}", d.mount_point.display(), d.name, d.kind, d.available, d.total))
        .collect();
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(std::env::temp_dir().join("slopexplore_drives.log"))
    {
        let _ = writeln!(f, "[{secs}] {}", items.join(" | "));
    }
}

/// One-shot listing of all drives, sorted by mount point (blocks for up to ~3 s while network drives answer).
/// Network drives are only probed when `include_network` is set.
pub fn load_drives(include_network: bool) -> Vec<DriveInfo> {
    let mut state = imp::State::default();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let (mut drives, pending) = imp::poll(&mut state, include_network);
        if !pending || Instant::now() >= deadline {
            drives.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
            return drives;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// True if `path` lives on a network share (UNC path, mapped drive, or a known network mount).
/// Used to avoid expensive recursive work (folder-size scans, recursive watching) over the network.
pub fn is_network_path(path: &Path) -> bool {
    imp::is_network_path(path)
}

/// Cleans up a path for display and for handing to other programs. `std::fs::canonicalize` returns Windows paths
/// in verbatim form, which many tools (cargo build scripts, cmd, resource compilers) can't use:
/// `\\?\C:\x` becomes `C:\x`, `\\?\UNC\server\share` becomes `\\server\share`, and the legacy WSL form
/// `\\wsl$\distro` becomes `\\wsl.localhost\distro`. Other platforms: unchanged.
pub fn clean_path(path: &Path) -> PathBuf {
    imp::clean_path(path)
}

/// False if `path` sits on a removable / network drive that is currently not in `drives` (unplugged USB stick,
/// disconnected share, stopped WSL distro). Used to hide Quick access entries of absent drives.
/// Paths on ordinary drives are always available, and so is everything on network drives while they are
/// switched off (`include_network == false`), since those are not probed at all.
pub fn path_available(path: &Path, drives: &[DriveInfo], include_network: bool) -> bool {
    imp::path_available(path, drives, include_network)
}

// ------------------------------------------------------------------------------------------------
#[cfg(windows)]
mod imp {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::ffi::c_void;

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
            value: *const u16,
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

    /// How often the (slow) probes of network volumes are repeated.
    const NET_REPROBE: Duration = Duration::from_secs(15);
    /// How often the list of running WSL distributions is re-read.
    const WSL_RELIST: Duration = Duration::from_secs(5);
    /// A network volume that has not answered within this long is shown as unavailable.
    const NET_GIVE_UP: Duration = Duration::from_millis(2500);
    /// A freshly inserted volume may not report its label immediately: wait this long for it, then show the
    /// drive anyway (it is updated as soon as the label arrives).
    const LABEL_WAIT: Duration = Duration::from_secs(8);

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

    /// The volume label (empty if it has none), or None if the volume can't be queried (not ready yet).
    fn volume_label(root: &str) -> Option<String> {
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
        (ok != 0).then(|| from_wide(&name))
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
    /// None: the query itself failed (device not ready yet), so the answer must not be remembered.
    fn is_external_bus(letter: char) -> Option<bool> {
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

    // ---- background probes of network volumes ------------------------------------------------

    #[derive(Clone)]
    struct ProbeResult {
        /// Mapped drives: the remote name. WSL list: the distribution names, one per line.
        text: String,
        space: Option<(u64, u64)>,
        /// Local volumes: the volume label (None: the volume could not be queried yet).
        label: Option<String>,
        /// Local fixed volumes: Some(true) for USB / SD storage.
        bus: Option<bool>,
    }

    /// A slow query (network share, WSL) that runs on its own thread, is repeated now and then, and never
    /// blocks the watcher. A second run is not started while the previous one is still stuck.
    struct Probe {
        key: String,
        job: fn(&str) -> ProbeResult,
        started: Instant,
        last: Option<Instant>,
        slot: Arc<Mutex<Option<ProbeResult>>>,
        busy: Arc<AtomicBool>,
        data: Option<ProbeResult>,
    }

    impl Probe {
        fn new(key: String, job: fn(&str) -> ProbeResult) -> Self {
            Probe {
                key,
                job,
                started: Instant::now(),
                last: None,
                slot: Arc::new(Mutex::new(None)),
                busy: Arc::new(AtomicBool::new(false)),
                data: None,
            }
        }

        /// Takes in a finished run, starts the next one when due, and returns the latest result.
        fn tick(&mut self, every: Duration) -> Option<&ProbeResult> {
            if let Some(r) = self.slot.lock().ok().and_then(|mut g| g.take()) {
                self.data = Some(r);
            }
            let due = self.last.map_or(true, |t| t.elapsed() >= every);
            if due && !self.busy.load(Ordering::Relaxed) {
                self.busy.store(true, Ordering::Relaxed);
                self.last = Some(Instant::now());
                let (slot, busy, key, job) = (self.slot.clone(), self.busy.clone(), self.key.clone(), self.job);
                thread::spawn(move || {
                    let r = job(&key);
                    if let Ok(mut g) = slot.lock() {
                        *g = Some(r);
                    }
                    busy.store(false, Ordering::Relaxed);
                });
            }
            self.data.as_ref()
        }

        /// No answer yet, and not yet worth giving up on.
        fn waiting(&self) -> bool {
            self.data.is_none() && self.started.elapsed() < NET_GIVE_UP
        }

        /// The current run has been stuck for a while: the volume is not answering.
        fn stale(&self) -> bool {
            self.busy.load(Ordering::Relaxed) && self.last.is_some_and(|t| t.elapsed() > NET_GIVE_UP)
        }
    }

    fn probe_mapped(key: &str) -> ProbeResult {
        let letter = key.chars().next().unwrap_or('C');
        ProbeResult {
            text: remote_name(letter).unwrap_or_default(),
            space: space(&format!("{letter}:\\")),
            label: None,
            bus: None,
        }
    }

    fn probe_space(root: &str) -> ProbeResult {
        ProbeResult { text: String::new(), space: space(root), label: None, bus: None }
    }

    /// The slow per-volume queries of a local drive. Any of them can stall for seconds while Windows is still
    /// mounting a freshly plugged-in volume, so they run on their own thread and never hold up the watcher.
    fn probe_local(key: &str) -> ProbeResult {
        let letter = key.chars().next().unwrap_or('C');
        let root = format!("{letter}:\\");
        let rootw = wide(&root);
        let fixed = unsafe { GetDriveTypeW(rootw.as_ptr()) } == DRIVE_FIXED;
        ProbeResult {
            text: String::new(),
            space: space(&root),
            label: volume_label(&root),
            bus: if fixed { is_external_bus(letter) } else { None },
        }
    }

    /// Names of the installed WSL distributions (HKCU\\...\\Lxss). Reading the registry starts nothing.
    fn registered_wsl_distros() -> Vec<String> {
        const HKCU: isize = 0x8000_0001u32 as i32 as isize;
        const KEY_READ: u32 = 0x0002_0019;
        const REG_SZ: u32 = 1;
        let mut out = Vec::new();
        let sub = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Lxss");
        let value = wide("DistributionName");
        unsafe {
            let mut root = 0isize;
            if RegOpenKeyExW(HKCU, sub.as_ptr(), 0, KEY_READ, &mut root) != 0 {
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
                    break; // ERROR_NO_MORE_ITEMS
                }
                i += 1;
                let mut sk = 0isize;
                if RegOpenKeyExW(root, name.as_ptr(), 0, KEY_READ, &mut sk) != 0 {
                    continue;
                }
                let mut buf = [0u16; 256];
                let mut bytes = (buf.len() * 2) as u32;
                let mut ty = 0u32;
                let rc = RegQueryValueExW(
                    sk,
                    value.as_ptr(),
                    std::ptr::null_mut(),
                    &mut ty,
                    buf.as_mut_ptr() as *mut u8,
                    &mut bytes,
                );
                RegCloseKey(sk);
                if rc == 0 && ty == REG_SZ {
                    out.push(from_wide(&buf));
                }
            }
            RegCloseKey(root);
        }
        out
    }

    /// Installed WSL distributions as lines "<1 if running, else 0>\t<name>". Running ones are the entries of
    /// `\\\\wsl.localhost\\` (listing it starts nothing); the rest come from the registry.
    fn probe_wsl_list(_: &str) -> ProbeResult {
        let mut running: Vec<String> = Vec::new();
        for host in ["\\\\wsl.localhost\\", "\\\\wsl$\\"] {
            if let Ok(rd) = std::fs::read_dir(host) {
                running.extend(rd.flatten().filter_map(|e| e.file_name().to_str().map(str::to_owned)));
                break;
            }
        }
        let mut names = registered_wsl_distros();
        for r in &running {
            if !names.iter().any(|n| n.eq_ignore_ascii_case(r)) {
                names.push(r.clone());
            }
        }
        let lines: Vec<String> = names
            .iter()
            .filter(|n| !n.is_empty() && !n.starts_with("docker-desktop"))
            .map(|n| format!("{}\t{n}", u8::from(running.iter().any(|r| r.eq_ignore_ascii_case(n)))))
            .collect();
        ProbeResult { text: lines.join("\n"), space: None, label: None, bus: None }
    }

    // ---- WSL paths ----------------------------------------------------------------------------

    /// `\\wsl$\Ubuntu\home` / `\\wsl.localhost\Ubuntu\home` -> `\\wsl.localhost\Ubuntu\home`; None for other paths.
    fn wsl_normalize(path: &str) -> Option<String> {
        let rest = path.strip_prefix("\\\\")?;
        let (host, tail) = match rest.find('\\') {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, ""),
        };
        if host.eq_ignore_ascii_case("wsl$") || host.eq_ignore_ascii_case("wsl.localhost") {
            Some(format!("\\\\wsl.localhost\\{tail}"))
        } else {
            None
        }
    }

    /// Mount point and display name of a mapped network drive.
    fn describe_remote(remote: &str, root: &str) -> (PathBuf, String, Option<PathBuf>) {
        if let Some(unc) = wsl_normalize(remote) {
            let unc = unc.trim_end_matches('\\').to_owned();
            let distro = unc.trim_start_matches("\\\\wsl.localhost").trim_start_matches('\\');
            let distro = distro.split('\\').next().unwrap_or("");
            let name = if distro.is_empty() { "WSL".to_owned() } else { distro.to_owned() };
            return (PathBuf::from(format!("{unc}\\")), name, Some(PathBuf::from(root)));
        }
        let share = remote.rsplit('\\').next().unwrap_or("").to_owned();
        let server = remote.trim_start_matches('\\').split('\\').next().unwrap_or("").to_owned();
        let label = match (share.is_empty(), server.is_empty()) {
            (false, false) => format!("{share} (\\\\{server})"),
            (false, true) => share,
            _ => "Network Drive".to_owned(),
        };
        (PathBuf::from(root), label, None)
    }

    // ---- polling --------------------------------------------------------------------------------

    #[derive(Default)]
    pub struct State {
        /// Drive letter -> "is USB / SD storage" (last definite answer).
        bus: HashMap<char, bool>,
        /// Local drives: the slow volume queries, on their own threads.
        local: HashMap<char, Probe>,
        /// Removable drives: when media was first seen in the letter.
        media_since: HashMap<char, Instant>,
        /// Mapped network drives.
        net: HashMap<char, Probe>,
        /// Running WSL distributions: the list, and one space probe per distribution.
        wsl_list: Option<Probe>,
        wsl: HashMap<String, Probe>,
    }

    /// Reads all drives once. The flag says that some volume is still being waited for, so the caller should
    /// poll again soon.
    pub fn poll(st: &mut State, include_network: bool) -> (Vec<DriveInfo>, bool) {
        unsafe {
            SetErrorMode(1); // SEM_FAILCRITICALERRORS: no "insert a disk" pop-ups for empty readers
        }
        let mask = unsafe { GetLogicalDrives() };
        let mut out = Vec::new();
        let mut pending = false;
        let mut seen_net: HashSet<char> = HashSet::new();

        // Forget what was learned about letters that are gone, so a new device on the same letter starts fresh.
        let present = |l: &char| mask & (1u32 << (*l as u8 - b'A')) != 0;
        st.bus.retain(|l, _| present(l));
        st.local.retain(|l, _| present(l));
        st.media_since.retain(|l, _| present(l));

        for i in 0..26u32 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let letter = (b'A' + i as u8) as char;
            let root = format!("{letter}:\\");
            let rootw = wide(&root);
            let ty = unsafe { GetDriveTypeW(rootw.as_ptr()) };

            if ty == DRIVE_REMOTE {
                if !include_network {
                    continue;
                }
                seen_net.insert(letter);
                let probe = st.net.entry(letter).or_insert_with(|| Probe::new(letter.to_string(), probe_mapped));
                let data = probe.tick(NET_REPROBE).cloned();
                if data.is_none() && probe.waiting() {
                    pending = true; // first answer is on its way: don't flash an "unavailable" entry
                    continue;
                }
                let res = data.unwrap_or(ProbeResult { text: String::new(), space: None, label: None, bus: None });
                let (total, available) = if probe.stale() { (0, 0) } else { res.space.unwrap_or((0, 0)) };
                let (mount_point, name, letter_root) = describe_remote(&res.text, &root);
                out.push(DriveInfo { mount_point, name, total, available, kind: DriveKind::Network, letter_root });
                continue;
            }

            let hint = match ty {
                DRIVE_REMOVABLE | DRIVE_CDROM => DriveKind::Removable,
                DRIVE_FIXED | DRIVE_RAMDISK => DriveKind::System, // a fixed one may turn out to be USB / SD below
                _ => {
                    st.local.remove(&letter);
                    continue;
                }
            };
            let probe = st.local.entry(letter).or_insert_with(|| Probe::new(letter.to_string(), probe_local));
            // Re-check quickly while the volume is still settling, calmly afterwards.
            let settling = probe.data.as_ref().map_or(true, |r| r.space.is_none() || r.label.is_none());
            let res = probe.tick(if settling { Duration::from_millis(400) } else { Duration::from_secs(1) }).cloned();
            let Some(res) = res else {
                pending = true; // first answer of this volume is still on its way
                continue;
            };
            if let Some(b) = res.bus {
                st.bus.insert(letter, b);
            }
            let external = ty == DRIVE_FIXED && st.bus.get(&letter).copied().unwrap_or(false);
            let kind = if external { DriveKind::Removable } else { hint };
            let (total, available) = match res.space {
                Some(sp) => sp,
                // An empty card reader or optical drive has no media: nothing to show.
                None if kind == DriveKind::Removable => {
                    st.media_since.remove(&letter);
                    continue;
                }
                None => (0, 0),
            };
            let since = *st.media_since.entry(letter).or_insert_with(Instant::now);
            if kind == DriveKind::Removable && res.label.is_none() && since.elapsed() < LABEL_WAIT {
                // The volume is there but not ready to report its label (typical right after plugging in).
                pending = true;
                continue;
            }
            let name = match res.label.filter(|l| !l.is_empty()) {
                Some(l) => l,
                None if kind == DriveKind::Removable => "Removable Disk".to_owned(),
                None => "Local Disk".to_owned(),
            };
            out.push(DriveInfo {
                mount_point: PathBuf::from(root),
                name,
                total,
                available,
                kind,
                letter_root: None,
            });
        }
        st.net.retain(|l, _| seen_net.contains(l));

        // WSL distributions, with their \\wsl.localhost\<distro>\ path. All installed ones are listed; free space is
        // only probed for running ones, since touching the share of a stopped distribution would start it.
        if include_network {
            let list = st.wsl_list.get_or_insert_with(|| Probe::new(String::new(), probe_wsl_list));
            let entries: Vec<(String, bool)> = list
                .tick(WSL_RELIST)
                .map(|r| {
                    r.text
                        .lines()
                        .filter_map(|l| {
                            let (run, name) = l.split_once('\t')?;
                            (!name.is_empty()).then(|| (name.to_owned(), run == "1"))
                        })
                        .collect()
                })
                .unwrap_or_default();
            st.wsl.retain(|n, _| entries.iter().any(|(e, run)| *run && e == n));
            for (n, running) in entries {
                let unc = format!("\\\\wsl.localhost\\{n}\\");
                // Already listed through a drive letter mapped to the same share.
                if out.iter().any(|d| d.mount_point.to_string_lossy().eq_ignore_ascii_case(&unc)) {
                    continue;
                }
                let (total, available) = if running {
                    let probe = st.wsl.entry(n.clone()).or_insert_with(|| Probe::new(unc.clone(), probe_space));
                    let data = probe.tick(NET_REPROBE).cloned();
                    if data.is_none() && probe.waiting() {
                        pending = true;
                        continue;
                    }
                    if probe.stale() { (0, 0) } else { data.and_then(|r| r.space).unwrap_or((0, 0)) }
                } else {
                    (0, 0)
                };
                out.push(DriveInfo {
                    mount_point: PathBuf::from(unc),
                    name: n,
                    total,
                    available,
                    kind: DriveKind::Network,
                    letter_root: None,
                });
            }
        } else {
            st.wsl_list = None;
            st.wsl.clear();
        }
        (out, pending)
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

    pub fn clean_path(path: &Path) -> PathBuf {
        let s = path.to_string_lossy();
        let unc_prefix = "\\\\?\\UNC\\";
        let plain: String = if s.get(..unc_prefix.len()).is_some_and(|p| p.eq_ignore_ascii_case(unc_prefix)) {
            format!("\\\\{}", &s[unc_prefix.len()..])
        } else if let Some(rest) = s.strip_prefix("\\\\?\\") {
            let b = rest.as_bytes();
            if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
                rest.to_owned()
            } else {
                return path.to_path_buf(); // \\?\Volume{...}\ and friends: no plain equivalent
            }
        } else {
            s.into_owned()
        };
        PathBuf::from(wsl_normalize(&plain).unwrap_or(plain))
    }

    /// `path` equals `root` or lies below it (case-insensitive, whole path components).
    fn under(path: &str, root: &str) -> bool {
        let root = root.trim_end_matches('\\');
        path.get(..root.len()).is_some_and(|p| p.eq_ignore_ascii_case(root))
            && (path.len() == root.len() || path.as_bytes()[root.len()] == b'\\')
    }

    pub fn path_available(path: &Path, drives: &[DriveInfo], include_network: bool) -> bool {
        let s = path.to_string_lossy();
        let b = s.as_bytes();
        let letter_path = b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic();
        if letter_path {
            // Mapped drives are not probed while network drives are off: assume they are there.
            if !include_network && is_network_path(path) {
                return true;
            }
            let prefix = &s[..2];
            return drives.iter().any(|d| {
                let root = d.letter_root.as_ref().unwrap_or(&d.mount_point).to_string_lossy();
                root.get(..2).is_some_and(|r| r.eq_ignore_ascii_case(prefix))
            });
        }
        if let Some(unc) = wsl_normalize(&s) {
            if !include_network {
                return true;
            }
            return drives.iter().any(|d| under(&unc, &d.mount_point.to_string_lossy()));
        }
        true
    }
}

// ------------------------------------------------------------------------------------------------
#[cfg(not(windows))]
mod imp {
    use super::*;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
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
                letter_root: None,
            });
        }
        if let Ok(mut g) = NETWORK_ROOTS.write() {
            *g = net_roots;
        }
        out
    }

    #[derive(Default)]
    pub struct State {
        sig: Option<u64>,
        last_full: Option<Instant>,
        net_flag: bool,
        cache: Vec<DriveInfo>,
    }

    /// The mount table changes the moment a drive is mounted or unmounted, so a cheap hash of it tells when
    /// a full (statvfs-heavy) reload is needed; otherwise the free space is refreshed every 10 s.
    fn mounts_signature() -> Option<u64> {
        let text = std::fs::read_to_string("/proc/self/mounts").ok()?;
        let mut h = DefaultHasher::new();
        text.hash(&mut h);
        Some(h.finish())
    }

    pub fn poll(st: &mut State, include_network: bool) -> (Vec<DriveInfo>, bool) {
        let sig = mounts_signature();
        let window = Duration::from_secs(if sig.is_some() { 10 } else { 2 });
        let fresh = st.net_flag == include_network
            && st.last_full.is_some_and(|t| t.elapsed() < window)
            && sig == st.sig;
        if !fresh {
            st.cache = load(include_network);
            st.sig = sig;
            st.last_full = Some(Instant::now());
            st.net_flag = include_network;
        }
        (st.cache.clone(), false)
    }

    pub fn clean_path(path: &Path) -> PathBuf {
        path.to_path_buf()
    }

    pub fn is_network_path(path: &Path) -> bool {
        NETWORK_ROOTS.read().map(|r| r.iter().any(|root| path.starts_with(root))).unwrap_or(false)
    }

    pub fn path_available(path: &Path, drives: &[DriveInfo], include_network: bool) -> bool {
        let removable_prefix = ["/media", "/run/media", "/mnt", "/Volumes"].iter().any(|p| path.starts_with(p));
        if !removable_prefix {
            return true;
        }
        if !include_network && is_network_path(path) {
            return true;
        }
        // Any listed mount below "/" that contains the path (the root mount has no parent and is skipped).
        drives.iter().any(|d| d.mount_point.parent().is_some() && path.starts_with(&d.mount_point))
    }
}
