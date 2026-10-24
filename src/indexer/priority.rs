//! Thread priority for background work.

#[cfg(windows)]
pub fn lower_priority() {
    // THREAD_PRIORITY_BELOW_NORMAL (-1): keeps the UI responsive without starving the scan.
    // (THREAD_MODE_BACKGROUND_BEGIN also drops I/O to "very low", which can make a scan crawl
    // whenever anything else touches the disk.)
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentThread() -> isize;
        fn SetThreadPriority(h: isize, p: i32) -> i32;
    }
    unsafe {
        SetThreadPriority(GetCurrentThread(), -1);
    }
}
#[cfg(target_os = "linux")]
pub fn lower_priority() {
    unsafe {
        // On Linux, nice is per-thread, so use the thread id.
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        libc::setpriority(libc::PRIO_PROCESS, tid, 10);
        // I/O priority: best-effort class (2), lowest level (7). Only honoured by some schedulers (BFQ).
        const IOPRIO_WHO_PROCESS: libc::c_int = 1;
        let ioprio: libc::c_int = (2 << 13) | 7;
        libc::syscall(libc::SYS_ioprio_set, IOPRIO_WHO_PROCESS, tid as libc::c_int, ioprio);
    }
}
#[cfg(all(unix, not(target_os = "linux")))]
pub fn lower_priority() {
    unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 10); }
}
