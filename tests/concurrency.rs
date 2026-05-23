//! Concurrency integration tests.
//!
//! These tests mount a real FUSE filesystem and verify that two
//! independent shell sessions invoking the same endpoint in parallel
//! each get back the right result. The routing key is `getsid(req.pid)`,
//! so we use `setsid()` in each child to give it its own session id.

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Returns true if the test environment cannot run a real FUSE mount
/// (missing fusermount, no /dev/fuse, etc).
fn fuse_unavailable() -> bool {
    if !std::path::Path::new("/dev/fuse").exists() {
        eprintln!("skipping: /dev/fuse not present");
        return true;
    }
    let has_fusermount = Command::new("fusermount")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
        || Command::new("fusermount3")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
    if !has_fusermount {
        eprintln!("skipping: no fusermount on PATH");
        return true;
    }
    false
}

fn build_binary() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let status = Command::new("cargo")
        .args(["build", "--bin", "livefolders"])
        .current_dir(&manifest)
        .status()
        .expect("cargo build");
    assert!(status.success(), "cargo build --bin livefolders failed");
    manifest.join("target/debug/livefolders")
}

struct MountFixture {
    _tmp: tempfile::TempDir,
    pub mount_dir: PathBuf,
    proc: Child,
}

impl MountFixture {
    fn new_with_shout() -> Self {
        let bin = build_binary();
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let tools_dir = tmp.path().join("tools");
        let work_dir = tmp.path().join("work");
        let mount_dir = work_dir.join(".livefolders");
        fs::create_dir_all(&tools_dir).unwrap();
        fs::create_dir_all(&work_dir).unwrap();
        fs::create_dir_all(&mount_dir).unwrap();

        // shout endpoint: stdin → uppercase via tr.
        let shout_dir = tools_dir.join("shout");
        fs::create_dir_all(&shout_dir).unwrap();
        fs::write(
            shout_dir.join("folder.yaml"),
            "name: shout\n\
             description: Uppercases stdin.\n\
             files:\n  - name: shout\n    type: write_invoke\n    handler: \"tr '[:lower:]' '[:upper:]'\"\n",
        )
        .unwrap();

        let config_path = work_dir.join("livefolders.yaml");
        let config_yaml = format!(
            "mount: {}\ntools_dir: {}\ntools:\n  - name: shout\n",
            mount_dir.display(),
            tools_dir.display(),
        );
        fs::write(&config_path, &config_yaml).unwrap();

        let mut proc = Command::new(&bin)
            .args(["mount", "--foreground", "--config"])
            .arg(&config_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn livefolders mount");

        let deadline = Instant::now() + Duration::from_secs(5);
        let index_md = mount_dir.join("index.md");
        loop {
            if index_md.exists() {
                break;
            }
            if let Ok(Some(status)) = proc.try_wait() {
                panic!("mount exited early: {status}");
            }
            if Instant::now() >= deadline {
                let _ = proc.kill();
                panic!("mount did not come up within 5s");
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        MountFixture {
            _tmp: tmp,
            mount_dir,
            proc,
        }
    }
}

impl Drop for MountFixture {
    fn drop(&mut self) {
        let _ = self.proc.kill();
        let _ = self.proc.wait();
        let _ = Command::new("fusermount")
            .args(["-u", &self.mount_dir.to_string_lossy()])
            .status();
        let _ = Command::new("fusermount3")
            .args(["-u", &self.mount_dir.to_string_lossy()])
            .status();
    }
}

/// Spawns a bash subprocess in a fresh session (so it has a unique sid).
/// The script is run via `bash -c` and its stdout is captured.
fn run_in_new_session(script: &str) -> String {
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(script);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    // SAFETY: setsid is async-signal-safe and only used in the child
    // process between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let out = cmd.output().expect("spawn bash");
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        panic!("bash failed: {} stderr: {}", out.status, err);
    }
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn two_parallel_shells_get_distinct_results() {
    if fuse_unavailable() {
        return;
    }
    let fix = MountFixture::new_with_shout();
    let ep = fix.mount_dir.join("tools/shout/shout");
    let ep_a = ep.clone();
    let ep_b = ep.clone();

    // Two concurrent shells, each runs `echo X > ep && cat ep` with a
    // different X. Each shell has its own session id thanks to setsid.
    let t_a = std::thread::spawn(move || {
        run_in_new_session(&format!(
            "echo alpha > {ep} && cat {ep}",
            ep = ep_a.display()
        ))
    });
    let t_b = std::thread::spawn(move || {
        run_in_new_session(&format!(
            "echo bravo > {ep} && cat {ep}",
            ep = ep_b.display()
        ))
    });

    let out_a = t_a.join().expect("thread a");
    let out_b = t_b.join().expect("thread b");

    assert!(
        out_a.contains("ALPHA"),
        "shell A expected ALPHA, got: {out_a:?}"
    );
    assert!(
        out_b.contains("BRAVO"),
        "shell B expected BRAVO, got: {out_b:?}"
    );
    assert!(
        !out_a.contains("BRAVO"),
        "shell A should not see shell B's data, got: {out_a:?}"
    );
    assert!(
        !out_b.contains("ALPHA"),
        "shell B should not see shell A's data, got: {out_b:?}"
    );
}

#[test]
fn many_parallel_shells_each_get_their_own_result() {
    if fuse_unavailable() {
        return;
    }
    let fix = MountFixture::new_with_shout();
    let ep = fix.mount_dir.join("tools/shout/shout");
    let n = 20;

    let mut handles = Vec::new();
    for i in 0..n {
        let ep = ep.clone();
        handles.push(std::thread::spawn(move || {
            let token = format!("token{i}");
            let out = run_in_new_session(&format!(
                "echo {token} > {ep} && cat {ep}",
                ep = ep.display()
            ));
            (i, out)
        }));
    }

    let mut wrong = Vec::new();
    for h in handles {
        let (i, out) = h.join().expect("thread");
        let expected = format!("TOKEN{i}");
        if !out.contains(&expected) {
            wrong.push((i, expected, out));
        }
    }
    assert!(wrong.is_empty(), "wrong results: {wrong:?}");
}

/// Mounts a fixture with a slow endpoint (sleeps 1s then echoes back)
/// to validate that two concurrent invocations run in parallel, not
/// serially.
fn new_with_slow_endpoint() -> MountFixture {
    let bin = build_binary();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let tools_dir = tmp.path().join("tools");
    let work_dir = tmp.path().join("work");
    let mount_dir = work_dir.join(".livefolders");
    fs::create_dir_all(&tools_dir).unwrap();
    fs::create_dir_all(&work_dir).unwrap();
    fs::create_dir_all(&mount_dir).unwrap();
    let slow_dir = tools_dir.join("slow");
    fs::create_dir_all(&slow_dir).unwrap();
    fs::write(
        slow_dir.join("folder.yaml"),
        "name: slow\n\
         description: Sleeps 1s then echoes input.\n\
         files:\n  - name: slow\n    type: write_invoke\n    handler: \"sleep 1; cat\"\n",
    )
    .unwrap();
    let config_path = work_dir.join("livefolders.yaml");
    let config_yaml = format!(
        "mount: {}\ntools_dir: {}\ntools:\n  - name: slow\n",
        mount_dir.display(),
        tools_dir.display(),
    );
    fs::write(&config_path, &config_yaml).unwrap();
    let mut proc = Command::new(&bin)
        .args(["mount", "--foreground", "--config"])
        .arg(&config_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    let deadline = Instant::now() + Duration::from_secs(5);
    let index_md = mount_dir.join("index.md");
    loop {
        if index_md.exists() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = proc.kill();
            panic!("mount did not come up");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    MountFixture {
        _tmp: tmp,
        mount_dir,
        proc,
    }
}

#[test]
fn two_slow_handlers_run_in_parallel() {
    if fuse_unavailable() {
        return;
    }
    let fix = new_with_slow_endpoint();
    let ep = fix.mount_dir.join("tools/slow/slow");
    let ep_a = ep.clone();
    let ep_b = ep.clone();

    let started = Instant::now();
    let t_a = std::thread::spawn(move || {
        run_in_new_session(&format!("echo a > {ep} && cat {ep}", ep = ep_a.display()))
    });
    let t_b = std::thread::spawn(move || {
        run_in_new_session(&format!("echo b > {ep} && cat {ep}", ep = ep_b.display()))
    });
    let _ = t_a.join().unwrap();
    let _ = t_b.join().unwrap();
    let elapsed = started.elapsed();

    // Each handler sleeps 1s. Serial = ~2s, parallel = ~1s.
    // Generous bound: 1.8s lets us detect serialisation without being flaky.
    assert!(
        elapsed < Duration::from_millis(1800),
        "expected parallel execution (~1s), got {:?} — handlers appear to be serialised",
        elapsed
    );
}

#[test]
fn same_shell_pipeline_works_sequentially() {
    if fuse_unavailable() {
        return;
    }
    let fix = MountFixture::new_with_shout();
    let ep = fix.mount_dir.join("tools/shout/shout");

    // Three sequential round trips in the same shell pipeline.
    let script = format!(
        "echo one   > {ep} && cat {ep}; \
         echo two   > {ep} && cat {ep}; \
         echo three > {ep} && cat {ep}",
        ep = ep.display()
    );
    let out = run_in_new_session(&script);
    assert!(out.contains("ONE"), "missing ONE: {out:?}");
    assert!(out.contains("TWO"), "missing TWO: {out:?}");
    assert!(out.contains("THREE"), "missing THREE: {out:?}");
}

