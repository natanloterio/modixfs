use fuser::Request;

/// Returns the kernel session id of the FUSE caller, or `None` when
/// FUSE did not give us a usable pid.
///
/// `echo` and `cat` spawned from the same shell pipeline share a sid
/// even though their pids differ, so keying invocation state on the sid
/// routes results back to the right caller without any client ceremony.
///
/// Caveat: FUSE issues some operations (notably `release` and async
/// `read`) with `pid == 0`. `getsid(0)` would return the daemon's own
/// session id, which is wrong. When that happens we return `None` so
/// the caller can fall back to a per-inode cache.
pub fn caller_sid(req: &Request<'_>) -> Option<i32> {
    let pid = req.pid() as libc::pid_t;
    if pid == 0 {
        return None;
    }
    // SAFETY: getsid is a pure syscall; pid comes from the kernel.
    let sid = unsafe { libc::getsid(pid) };
    if sid < 0 {
        return None;
    }
    Some(sid as i32)
}

#[cfg(test)]
mod tests {
    /// `getsid(0)` returns the calling process's own session id, which
    /// must be non-negative. Smoke test that the libc binding works on
    /// this platform.
    #[test]
    fn getsid_zero_returns_nonneg() {
        let sid = unsafe { libc::getsid(0) };
        assert!(sid >= 0, "getsid(0) returned {}", sid);
    }

    /// A forked child shares its parent's session id (until it calls
    /// setsid). This mirrors the `echo + cat` pipeline case and is the
    /// invariant the refactor relies on.
    #[test]
    fn forked_child_shares_session_id() {
        let parent_sid = unsafe { libc::getsid(0) };
        // SAFETY: fork in a test is fine; child immediately exits.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            // Child: report own sid via exit code (mod 256).
            let child_sid = unsafe { libc::getsid(0) };
            let ok = child_sid == parent_sid;
            unsafe { libc::_exit(if ok { 0 } else { 1 }) };
        }
        // Parent: wait and assert child exited 0.
        let mut status: libc::c_int = 0;
        unsafe { libc::waitpid(pid, &mut status, 0) };
        assert!(libc::WIFEXITED(status), "child did not exit cleanly");
        assert_eq!(libc::WEXITSTATUS(status), 0, "child sid did not match parent sid");
    }
}
