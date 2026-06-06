//! Freeze a target app so a hover-dismissing panel (WPS PDF etc.) stays put while
//! the user's cursor moves freely.
//!
//! The trick is to **suspend the app's process** with `SIGSTOP`: its run loop can't
//! execute, so it can't close the panel or redraw — the panel is frozen exactly as
//! it was. The window server keeps compositing the app's last frame, so
//! `screencapture -l <window_id>` still captures it. The cursor is an OS-level thing,
//! untouched — it moves freely. `SIGCONT` resumes the app.
//!
//! Caveat for callers: a stopped app cannot answer **AX** queries (they're synchronous
//! IPC to the app's run loop, which is suspended → the call hangs). So briefly
//! `resume()` around any live AX work, then `suspend()` again.

/// Suspend the app (SIGSTOP). No-op for an invalid pid; errors (e.g. dead pid) ignored.
pub fn suspend(pid: i32) {
    if pid > 0 {
        unsafe {
            libc::kill(pid, libc::SIGSTOP);
        }
    }
}

/// Resume a suspended app (SIGCONT).
pub fn resume(pid: i32) {
    if pid > 0 {
        unsafe {
            libc::kill(pid, libc::SIGCONT);
        }
    }
}
