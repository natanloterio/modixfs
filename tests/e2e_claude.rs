use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};
use std::fs;

// ── Prerequisite check ─────────────────────────────────────────────────────────

/// Returns true if the test should be skipped (missing API key or claude CLI).
fn prerequisites_missing() -> bool {
    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        eprintln!("Skipping: ANTHROPIC_API_KEY not set");
        return true;
    }
    let has_claude = Command::new("claude")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !has_claude {
        eprintln!("Skipping: claude CLI not found on PATH");
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
        eprintln!("Skipping: neither fusermount nor fusermount3 found on PATH");
        return true;
    }
    false
}

// ── Binary build ───────────────────────────────────────────────────────────────

fn build_livefolders_binary() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let status = Command::new("cargo")
        .args(["build", "--bin", "livefolders"])
        .current_dir(&manifest)
        .status()
        .expect("cargo build");
    assert!(status.success(), "cargo build --bin livefolders failed");
    manifest.join("target/debug/livefolders")
}

// ── Fixture ────────────────────────────────────────────────────────────────────

struct E2eFixture {
    _tmp_dir: tempfile::TempDir,
    pub tools_dir: PathBuf,
    pub mount_dir: PathBuf,
    pub work_dir: PathBuf,
    pub config_path: PathBuf,
    pub livefolders_bin: PathBuf,
    mount_proc: Child,
}

impl E2eFixture {
    fn new() -> Self {
        let bin = build_livefolders_binary();
        let tmp = tempfile::TempDir::new().expect("tempdir");

        let tools_dir = tmp.path().join("tools");
        let work_dir = tmp.path().join("work");
        let mount_dir = work_dir.join(".livefolders");

        fs::create_dir_all(&tools_dir).unwrap();
        fs::create_dir_all(&work_dir).unwrap();
        fs::create_dir_all(&mount_dir).unwrap();

        // Copy shout fixture into tools_dir
        let shout_dir = tools_dir.join("shout");
        fs::create_dir_all(&shout_dir).unwrap();
        fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/e2e/fixtures/shout/folder.yaml"),
            shout_dir.join("folder.yaml"),
        ).expect("copy shout fixture");

        // Write livefolders.yaml with absolute paths
        let config_path = work_dir.join("livefolders.yaml");
        let config_yaml = format!(
            "mount: {}\ntools_dir: {}\ntools:\n  - name: shout\n",
            mount_dir.display(),
            tools_dir.display(),
        );
        fs::write(&config_path, &config_yaml).unwrap();

        // Spawn FUSE mount in foreground
        let mut mount_proc = Command::new(&bin)
            .args(["mount", "--foreground", "--config"])
            .arg(&config_path)
            .spawn()
            .expect("spawn livefolders mount");

        // Poll until mount is ready (index.md synthesized by FUSE)
        let deadline = Instant::now() + Duration::from_secs(5);
        let index_md = mount_dir.join("tools").join("index.md");
        loop {
            if index_md.exists() {
                break;
            }
            if let Ok(Some(status)) = mount_proc.try_wait() {
                panic!("livefolders mount exited early with status: {}", status);
            }
            if Instant::now() >= deadline {
                let _ = mount_proc.kill();
                panic!("FUSE mount did not come up within 5 seconds (process still running — check FUSE availability and livefolders.yaml)");
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        E2eFixture {
            _tmp_dir: tmp,
            tools_dir,
            mount_dir,
            work_dir,
            config_path,
            livefolders_bin: bin,
            mount_proc,
        }
    }

    /// Writes .claude/settings.json with MCP server entry for this fixture.
    fn write_mcp_settings(&self) {
        let dot_claude = self.work_dir.join(".claude");
        fs::create_dir_all(&dot_claude).unwrap();
        let settings = serde_json::json!({
            "mcpServers": {
                "livefolders": {
                    "type": "stdio",
                    "command": self.livefolders_bin.to_str().unwrap(),
                    "args": ["mcp", "--config", self.config_path.to_str().unwrap()]
                }
            }
        });
        fs::write(
            dot_claude.join("settings.json"),
            serde_json::to_string_pretty(&settings).unwrap(),
        ).unwrap();
    }

    /// Writes CLAUDE.md in work_dir.
    fn write_claude_md(&self, content: &str) {
        fs::write(self.work_dir.join("CLAUDE.md"), content).unwrap();
    }

    /// Spawns `claude --print --dangerously-skip-permissions -p <prompt>` with
    /// a 90-second timeout. Returns (stdout, exit_success).
    fn run_claude(&self, prompt: &str) -> (String, bool) {
        let output = Command::new("timeout")
            .args(["90", "claude", "--print", "--dangerously-skip-permissions", "-p", prompt])
            .current_dir(&self.work_dir)
            .output()
            .expect("failed to spawn claude via timeout");
        if output.status.code() == Some(124) {
            panic!("claude timed out after 90 seconds");
        }
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        (stdout, output.status.success())
    }
}

impl Drop for E2eFixture {
    fn drop(&mut self) {
        let _ = self.mount_proc.kill();
        let _ = self.mount_proc.wait();
        let _ = Command::new("fusermount")
            .args(["-u", &self.mount_dir.to_string_lossy()])
            .status();
        let _ = Command::new("umount")
            .arg(&self.mount_dir)
            .status();
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[test]
#[ignore]
fn test_mcp_discover_and_call() {
    if prerequisites_missing() {
        return;
    }

    let fixture = E2eFixture::new();
    fixture.write_mcp_settings();

    let (output, ok) = fixture.run_claude(
        "Use the livefolders MCP server. Call the shout tool with the input 'hello world' \
         and tell me what it returned.",
    );

    assert!(ok, "claude exited with failure. Output:\n{}", output);
    assert!(
        output.to_uppercase().contains("HELLO WORLD"),
        "expected 'HELLO WORLD' in claude output, got:\n{}",
        output,
    );
}

#[test]
#[ignore]
fn test_mcp_create_and_call() {
    if prerequisites_missing() {
        return;
    }

    let fixture = E2eFixture::new();
    fixture.write_mcp_settings();

    // Sub-case B: Claude writes a folder.yaml to create a ping tool, then calls it via MCP.
    let tools_dir = fixture.tools_dir.to_str().unwrap().to_string();
    let prompt = format!(
        "You have access to LiveFolders tools via MCP. \
         Create a new tool by writing the following YAML content to the file \
         `{tools_dir}/ping/folder.yaml` (create the directory first):\n\
         \nname: ping\ndescription: Returns pong.\n\nfiles:\n  - name: ping\n    type: read_invoke\n    handler: \"echo pong\"\n    input:\n      type: none\n\n\
         Wait 3 seconds for hot-reload, then call the ping tool via MCP and tell me what it returned.",
        tools_dir = tools_dir,
    );

    let (output, ok) = fixture.run_claude(&prompt);

    assert!(ok, "claude exited with failure. Output:\n{}", output);
    assert!(
        output.to_lowercase().contains("pong"),
        "expected 'pong' in claude output, got:\n{}",
        output,
    );

    // Sub-case A: install from GitHub URL (optional, skipped unless env var set)
    if let Ok(url) = std::env::var("LIVEFOLDERS_E2E_GITHUB_URL") {
        let prompt_install = format!(
            "Run the shell command: `{bin} install {url}` \
             then list the available MCP tools and call any one of them, reporting its output.",
            bin = fixture.livefolders_bin.to_str().unwrap(),
            url = url,
        );
        let (out2, ok2) = fixture.run_claude(&prompt_install);
        assert!(ok2, "claude install+call failed. Output:\n{}", out2);
    }
}

#[test]
#[ignore]
fn test_claude_md_discover_and_call() {
    if prerequisites_missing() {
        return;
    }

    let fixture = E2eFixture::new();

    fixture.write_claude_md(
        "# LiveFolders Tools\n\
         \n\
         Tools are exposed as files under `.livefolders/tools/` in your working directory.\n\
         \n\
         To discover available tools:\n\
         ```bash\n\
         cat .livefolders/tools/index.md\n\
         ```\n\
         \n\
         To use a `write_invoke` tool (e.g. `shout`):\n\
         ```bash\n\
         echo \"your text\" > .livefolders/tools/shout/shout\n\
         cat .livefolders/tools/shout/shout\n\
         ```\n\
         The file simultaneously holds the input (write) and output (read).\n\
         Always write first, then read in two separate commands.\n",
    );

    let (output, ok) = fixture.run_claude(
        "Use the shout tool to shout 'hello world' and tell me what it returned.",
    );

    assert!(ok, "claude exited with failure. Output:\n{}", output);
    assert!(
        output.to_uppercase().contains("HELLO WORLD"),
        "expected 'HELLO WORLD' in claude output, got:\n{}",
        output,
    );
}
