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
#[cfg(not(windows))]
pub fn lower_priority() {}
