//! Talks to FileFly instances in OTHER PROCESSES. Windows of this process never come through here: they share
//! one `Explorer` and move tabs in memory (`app/windows.rs`). Another process exists because of elevation (an
//! elevated instance cannot share memory with a normal one, and one instance's token decides what all of its
//! windows may do), or because nothing of the same elevation was running when a folder was opened from outside
//! (`forward_open` hands it to a running instance instead of starting a second one). The processes find each
//! other through small files in the app folder:
//!   windows/<pid>.txt   - the window's screen rectangle, last focus, heartbeat and whether it is elevated
//!                         (refreshed every couple of seconds)
//!   inbox/<pid>/*.txt   - a folder to open as a tab (one path per file)
//!   inbox/<pid>/*.win   - a folder to open as a window

use crate::waker::Waker;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A path as bytes for the hand-over files. On Unix these are the exact bytes of the name;
/// elsewhere the path goes through (lossy) UTF-8.
fn path_to_bytes(p: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        p.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        p.to_string_lossy().into_owned().into_bytes()
    }
}

fn path_from_bytes(b: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(b))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(b).into_owned())
    }
}

/// A registration older than this belongs to a process that no longer runs (a live one rewrites it every ~2 s).
const STALE_MS: u64 = 60_000;

/// Removes what crashed or killed processes left behind, once at start: registrations whose heartbeat is
/// stale, and inbox folders of processes without a registration that are themselves stale (a process that
/// just started has a fresh folder and registers within moments, so it is never touched). Clean exits remove
/// their own traces in `Peers::shutdown`. Best effort: a failed removal is retried by the next start.
fn sweep_stale(base: &Path, now: u64) {
    let reg_dir = base.join("windows");
    if let Ok(rd) = fs::read_dir(&reg_dir) {
        for e in rd.flatten() {
            let beat = fs::read_to_string(e.path()).ok().and_then(|t| Registration::parse(&t)).map(|r| r.beat);
            // Unreadable or malformed counts as stale too: nothing live writes such a file.
            if beat.is_none_or(|b| now.saturating_sub(b) > STALE_MS) {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    if let Ok(rd) = fs::read_dir(base.join("inbox")) {
        for e in rd.flatten() {
            let registered = reg_dir.join(format!("{}.txt", e.file_name().to_string_lossy())).exists();
            let age = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(u64::MAX, |d| now.saturating_sub(d.as_millis() as u64));
            if !registered && age > STALE_MS {
                let _ = fs::remove_dir_all(e.path());
            }
        }
    }
}

/// A registration this old (milliseconds since its heartbeat) belongs to an instance that is still running.
const LIVE_MS: u64 = 6_000;

/// What an instance publishes in `windows/<pid>.txt`.
struct Registration {
    rect: [f64; 4],
    focus_ms: u64,
    /// Time of the last heartbeat (`now_ms`).
    beat: u64,
    elevated: bool,
}

impl Registration {
    /// None for a malformed file. A file without the elevation field (written by an older version) counts as
    /// elevated, so such an instance is never offered a folder from a normal process.
    fn parse(text: &str) -> Option<Self> {
        let v: Vec<f64> = text.split_whitespace().filter_map(|s| s.parse().ok()).collect();
        if v.len() < 6 {
            return None;
        }
        Some(Self { rect: [v[0], v[1], v[2], v[3]], focus_ms: v[4] as u64, beat: v[5] as u64, elevated: v.get(6).is_none_or(|e| *e != 0.0) })
    }
}

/// What another process hands over through `inbox/<pid>/`.
pub enum Handover {
    /// A tab dragged over from a window of another process.
    Tab(PathBuf),
    /// A folder opened from outside (the shell, "Show in folder") while this instance runs: a window of its own.
    Window(PathBuf),
}

/// Where a window is on screen, in physical pixels, and when it last had the focus.
#[derive(Clone, Copy)]
pub struct WinInfo {
    pub rect: [f32; 4], // left, top, right, bottom
    pub focus_ms: u64,
}

pub struct Peers {
    base: PathBuf,
    pid: u32,
    info: Arc<Mutex<Option<WinInfo>>>,
    inbox: Arc<Mutex<Vec<Handover>>>,
    stop: Arc<AtomicBool>,
}

impl Peers {
    pub fn new(waker: Waker, base: &Path) -> Self {
        let me = Self {
            base: base.to_path_buf(),
            pid: std::process::id(),
            info: Arc::new(Mutex::new(None)),
            inbox: Arc::new(Mutex::new(Vec::new())),
            stop: Arc::new(AtomicBool::new(false)),
        };
        let (info, inbox, stop) = (
            Arc::clone(&me.info),
            Arc::clone(&me.inbox),
            Arc::clone(&me.stop),
        );
        let (base, pid) = (me.base.clone(), me.pid);
        std::thread::spawn(move || {
            let reg_dir = base.join("windows");
            let mine = base.join("inbox").join(pid.to_string());
            sweep_stale(&base, now_ms());
            let _ = fs::create_dir_all(&reg_dir);
            let _ = fs::create_dir_all(&mine);
            let reg_file = reg_dir.join(format!("{pid}.txt"));
            let mut last: Option<WinInfo> = None;
            let mut last_write = 0u64;
            while !stop.load(Ordering::Relaxed) {
                // Publish our rectangle when it changed, and as a heartbeat every two seconds.
                let cur = info.lock().ok().and_then(|g| *g);
                if let Some(c) = cur {
                    let now = now_ms();
                    let changed = last.map_or(true, |l| l.rect != c.rect || l.focus_ms != c.focus_ms);
                    if changed || now.saturating_sub(last_write) > 2000 {
                        let text = format!(
                            "{} {} {} {} {} {} {}",
                            c.rect[0], c.rect[1], c.rect[2], c.rect[3], c.focus_ms, now, crate::elevate::is_elevated() as u8
                        );
                        let _ = fs::write(&reg_file, text);
                        last = Some(c);
                        last_write = now;
                    }
                }
                // Pick up what other processes have handed to us.
                let mut got = false;
                if let Ok(rd) = fs::read_dir(&mine) {
                    // File names start with a timestamp and a counter: sorting keeps the sending order.
                    let mut files: Vec<PathBuf> = rd
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| matches!(p.extension().and_then(|x| x.to_str()), Some("txt" | "win")))
                        .collect();
                    files.sort();
                    for p in files {
                        // Read as bytes: a folder name need not be valid UTF-8.
                        if let Ok(bytes) = fs::read(&p) {
                            if !bytes.is_empty() {
                                if let Ok(mut v) = inbox.lock() {
                                    let dir = path_from_bytes(&bytes);
                                    v.push(if p.extension().is_some_and(|x| x == "win") { Handover::Window(dir) } else { Handover::Tab(dir) });
                                    got = true;
                                }
                            }
                        }
                        let _ = fs::remove_file(&p);
                    }
                }
                if got {
                    waker.wake();
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        });
        me
    }

    pub fn set_info(&self, info: WinInfo) {
        if let Ok(mut g) = self.info.lock() {
            *g = Some(info);
        }
    }

    pub fn take_inbox(&self) -> Vec<Handover> {
        self.inbox
            .lock()
            .map(|mut v| std::mem::take(&mut *v))
            .unwrap_or_default()
    }

    /// The other FileFly window (if any) under the given screen point; the most recently
    /// focused one wins when windows overlap.
    pub fn window_at(&self, x: f32, y: f32) -> Option<u32> {
        let (x, y) = (x as f64, y as f64);
        let mut best: Option<(u64, u32)> = None;
        for e in fs::read_dir(self.base.join("windows")).ok()?.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(pid) = name
                .strip_suffix(".txt")
                .and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            if pid == self.pid {
                continue;
            }
            let Some(r) = fs::read_to_string(e.path()).ok().and_then(|t| Registration::parse(&t)) else {
                continue;
            };
            let age = now_ms().saturating_sub(r.beat);
            if age > STALE_MS {
                let _ = fs::remove_file(e.path()); // left behind by a crashed window
                continue;
            }
            if age > LIVE_MS {
                continue;
            }
            if x >= r.rect[0] && x < r.rect[2] && y >= r.rect[1] && y < r.rect[3] {
                if best.map_or(true, |(f, _)| r.focus_ms >= f) {
                    best = Some((r.focus_ms, pid));
                }
            }
        }
        best.map(|b| b.1)
    }

    /// Hands a folder to another window; it opens it as a new tab.
    pub fn send_tab(&self, pid: u32, dir: &Path) -> bool {
        write_handover(&self.base, pid, self.pid, dir, "txt").is_some()
    }

    /// Removes our traces (the process is about to be terminated without running destructors).
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = fs::remove_file(self.base.join("windows").join(format!("{}.txt", self.pid)));
        let _ = fs::remove_dir_all(self.base.join("inbox").join(self.pid.to_string()));
    }
}

/// Writes one hand-over file into `pid`'s inbox, atomically: a half-written file is never picked up. Returns the
/// file's path, which disappears once the receiver has taken it.
fn write_handover(base: &Path, pid: u32, from: u32, dir: &Path, ext: &str) -> Option<PathBuf> {
    let d = base.join("inbox").join(pid.to_string());
    fs::create_dir_all(&d).ok()?;
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let stem = format!("{:013}_{:06}_{}", now_ms(), SEQ.fetch_add(1, Ordering::Relaxed) % 1_000_000, from);
    let tmp = d.join(format!("{stem}.tmp"));
    fs::write(&tmp, path_to_bytes(dir)).ok()?;
    let to = d.join(format!("{stem}.{ext}"));
    fs::rename(&tmp, &to).ok()?;
    Some(to)
}

/// Hands a folder that was opened from outside (the shell, "Show in folder", an elevated relaunch) to an
/// already running instance of the same elevation, which opens it as a window, so that all non-elevated windows
/// (and all elevated ones) live in one process with one set of operations and locks. True once that instance has
/// taken it. False when there is no such instance, or it did not answer within `wait`: the caller then starts as
/// an instance of its own. Never forwards across elevations: an instance's token decides what its windows may do,
/// so a normal request must not end up with administrator rights (nor the reverse).
pub fn forward_open(base: &Path, dir: &Path, elevated: bool, wait: Duration) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let now = now_ms();
    let Ok(rd) = fs::read_dir(base.join("windows")) else { return false };
    // The most recently focused live instance of the same elevation.
    let target = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let pid = name.strip_suffix(".txt")?.parse::<u32>().ok()?;
            let r = Registration::parse(&fs::read_to_string(e.path()).ok()?)?;
            (pid != std::process::id() && now.saturating_sub(r.beat) <= LIVE_MS && r.elevated == elevated)
                .then_some((r.focus_ms, pid))
        })
        .max();
    let Some((_, pid)) = target else { return false };
    allow_foreground(pid); // so its new window may come to the front instead of flashing in the taskbar
    let Some(file) = write_handover(base, pid, std::process::id(), dir, "win") else { return false };
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if !file.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // Nobody took it. Withdraw it; if it is gone already, it was taken at the last moment after all.
    match fs::remove_file(&file) {
        Ok(()) => false,
        Err(_) => !file.exists(),
    }
}

/// Lets process `pid` bring a window to the front (a process that was just started by the shell may).
#[cfg(windows)]
fn allow_foreground(pid: u32) {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn AllowSetForegroundWindow(pid: u32) -> i32;
    }
    unsafe { AllowSetForegroundWindow(pid) };
}
#[cfg(not(windows))]
fn allow_foreground(_pid: u32) {}

/// The mouse pointer in physical screen pixels (works while it is outside our window).
#[cfg(windows)]
pub fn cursor_screen_pos() -> Option<(f32, f32)> {
    #[repr(C)]
    struct Pt {
        x: i32,
        y: i32,
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetCursorPos(p: *mut Pt) -> i32;
    }
    let mut p = Pt { x: 0, y: 0 };
    if unsafe { GetCursorPos(&mut p) } != 0 {
        Some((p.x as f32, p.y as f32))
    } else {
        None
    }
}
#[cfg(not(windows))]
pub fn cursor_screen_pos() -> Option<(f32, f32)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_removes_only_stale_traces() {
        let base = std::env::temp_dir().join(format!("fileflytest-peers-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let (win, inbox) = (base.join("windows"), base.join("inbox"));
        fs::create_dir_all(&win).unwrap();
        let now = now_ms();
        fs::write(win.join("1.txt"), format!("0 0 10 10 0 {}", now - 120_000)).unwrap(); // dead
        fs::write(win.join("2.txt"), format!("0 0 10 10 0 {}", now - 1_000)).unwrap(); // alive
        fs::write(win.join("3.txt"), "garbage").unwrap();
        for pid in ["1", "2", "9"] {
            fs::create_dir_all(inbox.join(pid)).unwrap();
        }
        // Now: only the dead and the malformed registrations go; fresh inbox folders stay, registered or not.
        sweep_stale(&base, now);
        assert!(!win.join("1.txt").exists() && !win.join("3.txt").exists());
        assert!(win.join("2.txt").exists());
        assert!(["1", "2", "9"].iter().all(|p| inbox.join(p).exists()));
        // Far in the future everything counts as old.
        sweep_stale(&base, now + 10 * STALE_MS);
        assert!(!win.join("2.txt").exists());
        assert!(!inbox.join("1").exists() && !inbox.join("9").exists());
        let _ = fs::remove_dir_all(&base);
    }

    /// Registers a fake instance (nothing runs behind it) with a heartbeat `age` ms old; `flag` is the optional
    /// elevation field.
    fn register(base: &Path, pid: u32, age: u64, flag: Option<u8>) {
        let dir = base.join("windows");
        fs::create_dir_all(&dir).unwrap();
        let mut text = format!("0 0 100 100 5 {}", now_ms() - age);
        if let Some(f) = flag {
            text += &format!(" {f}");
        }
        fs::write(dir.join(format!("{pid}.txt")), text).unwrap();
    }

    fn scratch(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("fileflytest-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn a_folder_opened_from_outside_goes_to_a_live_instance_of_the_same_elevation() {
        let base = scratch("fwd");
        register(&base, 4242, 100, Some(0));
        register(&base, 4343, 100, Some(1)); // elevated: never a target for a normal request
        let (b, d) = (base.clone(), base.clone());
        // The receiving instance: takes the file the way `Peers` does and reports what it held.
        let receiver = std::thread::spawn(move || {
            let inbox = b.join("inbox").join("4242");
            for _ in 0..80 {
                if let Some(f) = fs::read_dir(&inbox).ok().and_then(|rd| rd.flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|x| x == "win"))) {
                    let got = path_from_bytes(&fs::read(&f).unwrap());
                    fs::remove_file(&f).unwrap();
                    return Some(got);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            None
        });
        assert!(forward_open(&base, &d, false, Duration::from_secs(2)));
        assert_eq!(receiver.join().unwrap(), Some(base.clone()));
        assert!(!base.join("inbox").join("4343").exists(), "nothing went to the elevated instance");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn nothing_is_forwarded_across_elevations_to_stale_or_unmarked_instances_or_for_a_missing_folder() {
        let base = scratch("nofwd");
        register(&base, 5001, 100, Some(1)); // elevated, we are not
        register(&base, 5002, 20_000, Some(0)); // heartbeat too old: gone
        register(&base, 5003, 100, None); // unmarked (older version): counts as elevated
        let wait = Duration::from_millis(100);
        assert!(!forward_open(&base, &base, false, wait));
        assert!(!base.join("inbox").exists(), "no hand-over file was even written");
        register(&base, 5004, 100, Some(0));
        assert!(!forward_open(&base, &base.join("does-not-exist"), false, wait));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn an_instance_that_does_not_answer_gets_the_request_withdrawn() {
        let base = scratch("deaf");
        register(&base, 6001, 100, Some(0));
        assert!(!forward_open(&base, &base, false, Duration::from_millis(200)));
        let left = fs::read_dir(base.join("inbox").join("6001")).map(|rd| rd.count()).unwrap_or(0);
        assert_eq!(left, 0, "the caller starts on its own, so nobody may open the folder later");
        let _ = fs::remove_dir_all(&base);
    }
}
