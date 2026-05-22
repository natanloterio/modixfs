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
    /// Set by write_mcp_settings(); used by run_claude() as HOME override so
    /// Claude Code reads the test MCP config from fake_home/.claude.json.
    fake_home: Option<PathBuf>,
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

        // Poll until mount is ready (index.md is at the mount root, not under tools/)
        let deadline = Instant::now() + Duration::from_secs(5);
        let index_md = mount_dir.join("index.md");
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
            fake_home: None,
        }
    }

    /// Writes a fake HOME with .claude.json containing the MCP server entry.
    /// Claude Code reads mcpServers from ~/.claude.json, not from project settings.
    fn write_mcp_settings(&mut self) {
        let fake_home = self.work_dir.join("fake_home");
        fs::create_dir_all(&fake_home).unwrap();
        let settings = serde_json::json!({
            "mcpServers": {
                "livefolders": {
                    "type": "stdio",
                    "command": self.livefolders_bin.to_str().unwrap(),
                    "args": ["mcp", "--config", self.config_path.to_str().unwrap()],
                    "env": {}
                }
            }
        });
        fs::write(
            fake_home.join(".claude.json"),
            serde_json::to_string_pretty(&settings).unwrap(),
        ).unwrap();
        self.fake_home = Some(fake_home);
    }

    /// Writes CLAUDE.md in work_dir.
    fn write_claude_md(&self, content: &str) {
        fs::write(self.work_dir.join("CLAUDE.md"), content).unwrap();
    }

    /// Spawns `claude --print --dangerously-skip-permissions -p <prompt>` with
    /// a timeout. Returns (stdout, exit_success).
    /// If write_mcp_settings() was called, HOME is overridden to the fake home
    /// so Claude Code reads the test MCP config from fake_home/.claude.json.
    fn run_claude(&self, prompt: &str) -> (String, bool) {
        self.run_claude_timeout(prompt, 90)
    }

    fn run_claude_timeout(&self, prompt: &str, timeout_secs: u64) -> (String, bool) {
        let timeout_str = timeout_secs.to_string();
        let mut cmd = Command::new("timeout");
        cmd.args([timeout_str.as_str(), "claude", "--print", "--dangerously-skip-permissions", "-p", prompt])
            .current_dir(&self.work_dir);
        if let Some(ref home) = self.fake_home {
            cmd.env("HOME", home);
        }
        let output = cmd.output().expect("failed to spawn claude via timeout");
        if output.status.code() == Some(124) {
            panic!("claude timed out after {} seconds", timeout_secs);
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

    let mut fixture = E2eFixture::new();
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

    let mut fixture = E2eFixture::new();
    fixture.write_mcp_settings();

    // Sub-case A: Claude writes a folder.yaml to create a ping tool, then calls it via MCP.
    let tools_dir = fixture.tools_dir.to_str().unwrap().to_string();
    let prompt = format!(
        "You have access to LiveFolders tools via MCP. \
         Create a new tool by writing the following YAML content to the file \
         `{tools_dir}/ping/folder.yaml` (create the directory first):\n\
         \n```yaml\nname: ping\ndescription: Returns pong.\n\nfiles:\n  - name: ping\n    type: read_invoke\n    handler: \"echo pong\"\n    input:\n      type: none\n```\n\n\
         Wait 3 seconds for hot-reload, then call the ping tool via MCP and tell me what it returned.",
        tools_dir = tools_dir,
    );

    // Create-and-call needs more time: write file + 3s wait + MCP tool discovery + call
    let (output, ok) = fixture.run_claude_timeout(&prompt, 150);

    assert!(ok, "claude exited with failure. Output:\n{}", output);
    assert!(
        output.to_lowercase().contains("pong"),
        "expected 'pong' in claude output, got:\n{}",
        output,
    );

    // Sub-case B: install from GitHub URL (optional, skipped unless env var set)
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
         cat .livefolders/index.md\n\
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

#[test]
#[ignore]
fn test_claude_md_create_and_call() {
    if prerequisites_missing() {
        return;
    }

    let fixture = E2eFixture::new();
    let tools_dir = fixture.tools_dir.to_str().unwrap().to_string();

    let claude_md = format!(
        "# LiveFolders Tools\n\
         \n\
         Tools live under `.livefolders/tools/`. Use them with echo/cat:\n\
         ```bash\n\
         echo \"input\" > .livefolders/tools/<name>/<endpoint>\n\
         cat .livefolders/tools/<name>/<endpoint>\n\
         ```\n\
         \n\
         ## Creating a New Tool\n\
         \n\
         Write a `folder.yaml` to a new subdirectory under: `{tools_dir}`\n\
         \n\
         Example for a read-only tool (no input needed):\n\
         ```yaml\n\
         name: mytool\n\
         description: Does something.\n\
         \n\
         files:\n\
           - name: mytool\n\
             type: read_invoke\n\
             handler: \"echo result\"\n\
             input:\n\
               type: none\n\
         ```\n\
         \n\
         After writing `folder.yaml`, wait 3 seconds — the tool is hot-reloaded automatically.\n\
         Then use it at `.livefolders/tools/<name>/<endpoint>`.\n",
        tools_dir = tools_dir,
    );
    fixture.write_claude_md(&claude_md);

    let prompt = format!(
        "Create a new tool called 'ping' that returns 'pong' when read. \
         Write its folder.yaml to {tools_dir}/ping/folder.yaml, wait 3 seconds, \
         then call it and tell me what it returned.",
        tools_dir = tools_dir,
    );

    let (output, ok) = fixture.run_claude(&prompt);

    assert!(ok, "claude exited with failure. Output:\n{}", output);
    assert!(
        output.to_lowercase().contains("pong"),
        "expected 'pong' in claude output, got:\n{}",
        output,
    );
}
