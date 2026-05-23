use fuser::Request;

/// Returns the kernel session id of the FUSE caller.
///
/// `echo` and `cat` spawned from the same shell pipeline share a sid
/// even though their pids differ, so keying invocation state on the sid
/// routes results back to the right caller without any client ceremony.
///
/// Falls back to `0` when `getsid` fails (caller already exited, or some
/// macOS edge case). `sid == 0` is treated by the runtime as a shared
/// default session, which preserves the pre-refactor behaviour for any
/// caller we cannot identify.
pub fn caller_sid(req: &Request<'_>) -> i32 {
    let pid = req.pid() as libc::pid_t;
    // SAFETY: getsid is a pure syscall; pid comes from the kernel.
    let sid = unsafe { libc::getsid(pid) };
    if sid < 0 { 0 } else { sid as i32 }
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
