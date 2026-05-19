use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

pub fn data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".local/share/livefolders")
}

pub fn pid_file_for(mountpoint: &Path) -> PathBuf {
    let name = mountpoint.to_string_lossy().replace('/', "_");
    data_dir().join(format!("{}.pid", name))
}

pub fn log_file() -> PathBuf {
    data_dir().join("livefolders.log")
}

/// Fork into background. The parent prints a status line and exits 0.
/// The child redirects stderr to the log file and continues.
/// Returns only in the child.
pub fn daemonize(mountpoint: &Path) -> Result<()> {
    let pid_path = pid_file_for(mountpoint);
    let log_path = log_file();

    std::fs::create_dir_all(data_dir())
        .context("creating livefolders data directory")?;

    println!(
        "Mounting at {} in background.\nLogs: {}\nStop with: livefolders stop",
        mountpoint.display(),
        log_path.display()
    );

    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening log file {}", log_path.display()))?;

    unsafe {
        match libc::fork() {
            -1 => bail!("fork() failed"),
            0 => {
                // Child: create new session, redirect stderr to log
                libc::setsid();
                let log_fd = std::os::unix::io::IntoRawFd::into_raw_fd(log);
                libc::dup2(log_fd, libc::STDERR_FILENO);
                libc::close(log_fd);
                // Redirect stdout to /dev/null so daemon is silent
                let devnull = libc::open(
                    b"/dev/null\0".as_ptr() as *const libc::c_char,
                    libc::O_WRONLY,
                );
                libc::dup2(devnull, libc::STDOUT_FILENO);
                libc::close(devnull);
            }
            _ => {
                // Parent: exit cleanly
                std::process::exit(0);
            }
        }
    }

    // Write PID file (in child)
    let pid = unsafe { libc::getpid() };
    std::fs::write(&pid_path, format!("{}\n", pid))
        .with_context(|| format!("writing PID file {}", pid_path.display()))?;

    Ok(())
}

/// Kill the daemon for the given mountpoint and remove its PID file.
pub fn stop(mountpoint: &Path) -> Result<()> {
    let pid_path = pid_file_for(mountpoint);
    if !pid_path.exists() {
        bail!(
            "no PID file found for {}.\nIs livefolders running for that mountpoint?",
            mountpoint.display()
        );
    }
    let content = std::fs::read_to_string(&pid_path)
        .with_context(|| format!("reading PID file {}", pid_path.display()))?;
    let pid: i32 = content
        .trim()
        .parse()
        .with_context(|| format!("invalid PID in {}", pid_path.display()))?;

    let ret = unsafe { libc::kill(pid, libc::SIGTERM) };
    if ret != 0 {
        let err = std::io::Error::last_os_error();
        // ESRCH means process already gone — still clean up the PID file
        if err.raw_os_error() != Some(libc::ESRCH) {
            bail!("failed to send SIGTERM to PID {}: {}", pid, err);
        }
    }

    let _ = std::fs::remove_file(&pid_path);
    println!("Stopped livefolders (PID {}).", pid);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_file_for_encodes_mountpoint() {
        let p = pid_file_for(Path::new("/tmp/livefolders"));
        let name = p.file_name().unwrap().to_string_lossy();
        assert!(name.contains("tmp"));
        assert!(name.contains("livefolders"));
        assert!(name.ends_with(".pid"));
    }

    #[test]
    fn log_file_is_inside_data_dir() {
        assert!(log_file().starts_with(data_dir()));
    }

    #[test]
    fn stop_errors_when_no_pid_file() {
        let tmp = tempfile::tempdir().unwrap();
        let fake_mount = tmp.path().join("nonexistent_mount");
        let result = stop(&fake_mount);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("PID file"));
    }
}
