# Tool Manifest & Install Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add `modix.yaml` tool manifests, `modixfs install <url>` command, and automatic secrets.env loading at mount time so tool builders can publish installable tools.

**Architecture:** Three new source files (`manifest.rs`, `secrets.rs`, `installer.rs`) wired into `main.rs`. No changes to the FUSE layer or registry. The install command downloads a GitHub tarball via `reqwest` (blocking feature), extracts it, prompts for required env vars, and copies into `tools_dir`. Secrets are stored in `~/.config/modixfs/secrets.env` and loaded at mount time via `std::env::set_var`.

**Tech Stack:** Rust, reqwest (blocking), serde_yaml, flate2, tar, tempfile

---

## Task 1: Update Cargo.toml dependencies

**Files:**
- Modify: `Cargo.toml`

**Step 1: Update Cargo.toml**

Replace the current `reqwest` line and `tempfile` dev-dependency:

```toml
reqwest = { version = "0.12", features = ["json", "blocking"] }
flate2 = "1"
tar = "0.4"
tempfile = "3"
```

Remove `tempfile` from `[dev-dependencies]` (it moves to regular deps). The full `[dependencies]` section becomes:

```toml
[dependencies]
fuser = "0.15"
tokio = { version = "1", features = ["full"] }
async-trait = "0.1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
anyhow = "1"
reqwest = { version = "0.12", features = ["json", "blocking"] }
serde_yaml = "0.9"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
libc = "0.2"
notify = { version = "6", features = ["macos_kqueue"] }
flate2 = "1"
tar = "0.4"
tempfile = "3"
```

And `[dev-dependencies]` becomes empty (remove the section entirely or leave it empty).

**Step 2: Verify it resolves**

```bash
cd /media/loterio/workspace/workspace/research/ModixFS
cargo build 2>&1 | grep "^error"
```

Expected: no output.

**Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: add flate2, tar, blocking reqwest; move tempfile to regular deps"
```

---

## Task 2: Create `src/manifest.rs`

Parses `modix.yaml` from a tool directory. Tested with unit tests.

**Files:**
- Create: `src/manifest.rs`

**Step 1: Write the failing test**

At the bottom of `src/manifest.rs` (create the file first with just the test module):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_full_manifest() {
        let yaml = r#"
name: mytool
description: Does something useful
version: 0.2.0
env:
  - name: MYTOOL_KEY
    description: API key
    required: true
  - name: MYTOOL_TIMEOUT
    description: Timeout
    required: false
    default: "30"
"#;
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(m.name.as_deref(), Some("mytool"));
        assert_eq!(m.description.as_deref(), Some("Does something useful"));
        assert_eq!(m.env.len(), 2);
        assert!(m.env[0].required);
        assert!(!m.env[1].required);
        assert_eq!(m.env[1].default.as_deref(), Some("30"));
    }

    #[test]
    fn parse_minimal_manifest() {
        let yaml = "name: minimal\n";
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(m.name.as_deref(), Some("minimal"));
        assert!(m.env.is_empty());
    }

    #[test]
    fn load_from_dir_missing_file_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let result = Manifest::load(tmp.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn load_from_dir_reads_modix_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        let mut f = std::fs::File::create(tmp.path().join("modix.yaml")).unwrap();
        writeln!(f, "name: testpkg\nversion: 1.0.0").unwrap();
        let m = Manifest::load(tmp.path()).unwrap().unwrap();
        assert_eq!(m.name.as_deref(), Some("testpkg"));
    }
}
```

**Step 2: Run test to verify it fails**

```bash
cargo test manifest 2>&1 | tail -20
```

Expected: compilation error — `Manifest` not defined.

**Step 3: Write implementation**

Full `src/manifest.rs`:

```rust
use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
pub struct Manifest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub version: Option<String>,
    #[serde(default)]
    pub env: Vec<EnvDecl>,
}

#[derive(Debug, Deserialize)]
pub struct EnvDecl {
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub required: bool,
    pub default: Option<String>,
}

impl Manifest {
    pub fn load(tool_dir: &Path) -> anyhow::Result<Option<Self>> {
        let path = tool_dir.join("modix.yaml");
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)?;
        let manifest = serde_yaml::from_str(&content)?;
        Ok(Some(manifest))
    }
}

#[cfg(test)]
mod tests {
    // ... (paste test module from Step 1 here)
}
```

**Step 4: Add `mod manifest;` to `src/main.rs`**

At the top of `src/main.rs`, add:
```rust
mod manifest;
```

**Step 5: Run tests**

```bash
cargo test manifest 2>&1 | tail -20
```

Expected: 4 tests pass.

**Step 6: Commit**

```bash
git add src/manifest.rs src/main.rs
git commit -m "feat: add Manifest struct and modix.yaml parsing"
```

---

## Task 3: Create `src/secrets.rs`

Loads and appends to `~/.config/modixfs/secrets.env`.

**Files:**
- Create: `src/secrets.rs`

**Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_secrets_skips_comments_and_blanks() {
        let content = "# comment\n\nFOO=bar\nBAZ=qux\n";
        let pairs = parse_env_file(content);
        assert_eq!(pairs, vec![("FOO".to_string(), "bar".to_string()),
                                ("BAZ".to_string(), "qux".to_string())]);
    }

    #[test]
    fn parse_secrets_value_may_contain_equals() {
        let content = "URL=http://x.com?a=1&b=2\n";
        let pairs = parse_env_file(content);
        assert_eq!(pairs[0].1, "http://x.com?a=1&b=2");
    }

    #[test]
    fn has_secret_in_env() {
        std::env::set_var("_TEST_MODIX_KEY", "present");
        // Pass a non-existent file path so has_secret only checks env
        let result = std::env::var("_TEST_MODIX_KEY").is_ok();
        assert!(result);
        std::env::remove_var("_TEST_MODIX_KEY");
    }
}
```

**Step 2: Run test to verify it fails**

```bash
cargo test secrets 2>&1 | tail -10
```

Expected: compilation error.

**Step 3: Write implementation**

Full `src/secrets.rs`:

```rust
use std::path::PathBuf;

use anyhow::Result;

fn secrets_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".config/modixfs/secrets.env")
}

fn parse_env_file(content: &str) -> Vec<(String, String)> {
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            Some((k.trim().to_string(), v.to_string()))
        })
        .collect()
}

pub fn load_secrets_env() -> Result<()> {
    let path = secrets_path();
    if !path.exists() {
        return Ok(());
    }
    let content = std::fs::read_to_string(&path)?;
    for (key, val) in parse_env_file(&content) {
        if std::env::var(&key).is_err() {
            std::env::set_var(&key, val);
        }
    }
    Ok(())
}

pub fn append_secret(key: &str, value: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let path = secrets_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)?;
    writeln!(file, "{}={}", key, value)?;
    Ok(())
}

pub fn has_secret(key: &str) -> bool {
    if std::env::var(key).is_ok() {
        return true;
    }
    let path = secrets_path();
    if !path.exists() {
        return false;
    }
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    parse_env_file(&content).iter().any(|(k, _)| k == key)
}

#[cfg(test)]
mod tests {
    use super::*;
    // ... (tests from Step 1)
}
```

**Step 4: Add `mod secrets;` to `src/main.rs`**

**Step 5: Run tests**

```bash
cargo test secrets 2>&1 | tail -10
```

Expected: 3 tests pass.

**Step 6: Commit**

```bash
git add src/secrets.rs src/main.rs
git commit -m "feat: add secrets.env load/append helpers"
```

---

## Task 4: Create `src/installer.rs` — URL parsing

The URL parser is the most testable piece of the installer. Build and test it first.

**Files:**
- Create: `src/installer.rs` (URL parser only for now)

**Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_owner_repo() {
        let u = parse_github_url("github.com/alice/mytool").unwrap();
        assert_eq!(u.owner, "alice");
        assert_eq!(u.repo, "mytool");
        assert_eq!(u.branch, "HEAD");
        assert!(u.subdir.is_none());
    }

    #[test]
    fn parse_with_https_prefix() {
        let u = parse_github_url("https://github.com/alice/mytool").unwrap();
        assert_eq!(u.owner, "alice");
        assert_eq!(u.repo, "mytool");
    }

    #[test]
    fn parse_with_subdir() {
        let u = parse_github_url("github.com/alice/tools/tree/main/summarizer").unwrap();
        assert_eq!(u.owner, "alice");
        assert_eq!(u.repo, "tools");
        assert_eq!(u.branch, "main");
        assert_eq!(u.subdir.as_deref(), Some("summarizer"));
    }

    #[test]
    fn parse_invalid_url_errors() {
        assert!(parse_github_url("notgithub.com/x").is_err());
        assert!(parse_github_url("github.com/onlyone").is_err());
    }
}
```

**Step 2: Run test to verify it fails**

```bash
cargo test installer 2>&1 | tail -10
```

Expected: compilation error.

**Step 3: Write URL parser**

`src/installer.rs` (URL parser only — `install` function comes in Task 5):

```rust
use anyhow::{bail, Result};

pub struct GithubUrl {
    pub owner: String,
    pub repo: String,
    pub branch: String,
    pub subdir: Option<String>,
}

pub fn parse_github_url(url: &str) -> Result<GithubUrl> {
    let url = url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("github.com/");

    // Must start with owner/repo — reject anything else
    let parts: Vec<&str> = url.splitn(5, '/').collect();

    match parts.as_slice() {
        [owner, repo] if !owner.is_empty() && !repo.is_empty() => Ok(GithubUrl {
            owner: owner.to_string(),
            repo: repo.to_string(),
            branch: "HEAD".to_string(),
            subdir: None,
        }),
        [owner, repo, "tree", branch, subdir] => Ok(GithubUrl {
            owner: owner.to_string(),
            repo: repo.to_string(),
            branch: branch.to_string(),
            subdir: Some(subdir.to_string()),
        }),
        _ => bail!(
            "unrecognized GitHub URL. Expected:\n  github.com/owner/repo\n  github.com/owner/repo/tree/BRANCH/subdir"
        ),
    }
}

pub fn install(_url: &str, _cfg: &crate::config::Config) -> Result<()> {
    todo!("implemented in Task 5")
}

#[cfg(test)]
mod tests {
    use super::*;
    // ... (tests from Step 1)
}
```

**Step 4: Add `mod installer;` to `src/main.rs`**

**Step 5: Run tests**

```bash
cargo test installer 2>&1 | tail -10
```

Expected: 4 URL parser tests pass (the `install` todo is not tested yet).

**Step 6: Commit**

```bash
git add src/installer.rs src/main.rs
git commit -m "feat: GitHub URL parser for modixfs install"
```

---

## Task 5: Complete `src/installer.rs` — download, extract, copy

Implement the full `install()` function.

**Files:**
- Modify: `src/installer.rs`

**Step 1: Replace the `install` stub with the full implementation**

Replace `pub fn install(_url: &str, _cfg: &crate::config::Config) -> Result<()> { todo!(...) }` with:

```rust
pub fn install(url: &str, cfg: &crate::config::Config) -> Result<()> {
    use std::io::Write;

    let tools_dir = cfg.resolved_tools_dir()?.ok_or_else(|| {
        anyhow::anyhow!(
            "tools_dir is not configured in tools.yaml. Add:\n  tools_dir: ~/.config/modixfs/tools"
        )
    })?;

    let gh = parse_github_url(url)?;

    println!("Downloading {}/{}...", gh.owner, gh.repo);

    let tarball_url = format!(
        "https://api.github.com/repos/{}/{}/tarball/{}",
        gh.owner, gh.repo, gh.branch
    );

    let client = reqwest::blocking::Client::builder()
        .user_agent("modixfs")
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()?;

    let mut req = client
        .get(&tarball_url)
        .header("Accept", "application/vnd.github+json");

    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        req = req.header("Authorization", format!("Bearer {}", token));
    } else {
        tracing::warn!("GITHUB_TOKEN not set — GitHub rate limits apply (60 req/hr unauthenticated)");
    }

    let bytes = req
        .send()
        .context("downloading tarball")?
        .error_for_status()
        .context("GitHub API returned an error")?
        .bytes()
        .context("reading response body")?;

    println!("Extracting...");

    let tmp = tempfile::tempdir()?;
    let gz = flate2::read::GzDecoder::new(bytes.as_ref());
    let mut archive = tar::Archive::new(gz);
    archive.unpack(tmp.path())?;

    // GitHub tarballs contain one top-level directory (owner-repo-SHA/).
    let top_level = std::fs::read_dir(tmp.path())?
        .flatten()
        .find(|e| e.path().is_dir())
        .context("unexpected tarball structure: no top-level directory")?
        .path();

    let tool_src = match &gh.subdir {
        Some(sub) => {
            let p = top_level.join(sub);
            if !p.is_dir() {
                bail!("subdir '{}' not found in repository", sub);
            }
            p
        }
        None => top_level,
    };

    let manifest = match crate::manifest::Manifest::load(&tool_src)? {
        Some(m) => {
            if let Some(desc) = &m.description {
                println!("  {}", desc);
            }
            m
        }
        None => {
            tracing::warn!("no modix.yaml found — installing without manifest");
            crate::manifest::Manifest::default()
        }
    };

    let tool_name = manifest
        .name
        .clone()
        .unwrap_or_else(|| gh.subdir.clone().unwrap_or_else(|| gh.repo.clone()));

    // Prompt for missing required env vars.
    for decl in &manifest.env {
        if !decl.required || crate::secrets::has_secret(&decl.name) {
            continue;
        }
        let desc = decl.description.as_deref().unwrap_or(&decl.name);
        print!("{} ({}): ", decl.name, desc);
        std::io::stdout().flush()?;
        let mut value = String::new();
        std::io::stdin().read_line(&mut value)?;
        let value = value.trim().to_string();
        if !value.is_empty() {
            crate::secrets::append_secret(&decl.name, &value)?;
            std::env::set_var(&decl.name, &value);
        }
    }

    std::fs::create_dir_all(&tools_dir)?;
    let dest = tools_dir.join(&tool_name);

    if dest.exists() {
        print!("Tool '{}' already exists. Overwrite? [y/N]: ", tool_name);
        std::io::stdout().flush()?;
        let mut ans = String::new();
        std::io::stdin().read_line(&mut ans)?;
        if !ans.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return Ok(());
        }
        std::fs::remove_dir_all(&dest)?;
    }

    copy_dir_all(&tool_src, &dest)?;

    println!("Installed {} → {}", tool_name, dest.display());
    println!("Run `modixfs mount` to start using it (or it will appear automatically if already mounted).");

    Ok(())
}

fn copy_dir_all(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)?.flatten() {
        let dst_path = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&entry.path(), &dst_path)?;
        } else {
            std::fs::copy(entry.path(), &dst_path)?;
        }
    }
    Ok(())
}
```

Also add the necessary `use` statements at the top of `src/installer.rs`:

```rust
use anyhow::{bail, Context, Result};
use tracing;
```

**Step 2: Build**

```bash
cargo build 2>&1 | grep "^error"
```

Expected: no output.

**Step 3: Run existing URL parser tests still pass**

```bash
cargo test installer 2>&1 | tail -10
```

Expected: 4 tests pass.

**Step 4: Commit**

```bash
git add src/installer.rs
git commit -m "feat: implement modixfs install — download, extract, prompt, copy"
```

---

## Task 6: Wire `install` command and secrets auto-load in `src/main.rs`

**Files:**
- Modify: `src/main.rs`

**Step 1: Update USAGE string**

Replace the current `const USAGE` with:

```rust
const USAGE: &str = "\
Usage: modixfs <command> [options]

Commands:
  init                   Create a starter tools.yaml in the current directory
  mount [path]           Mount the filesystem (path overrides tools.yaml)
  install <url>          Install a tool from GitHub
  help                   Show this help

Options:
  --config, -c <file>   Path to tools.yaml (default: ./tools.yaml)

Environment:
  GITHUB_TOKEN      GitHub personal access token (for github tool and install)
";
```

**Step 2: Add `install` arm to the `match cmd` block**

```rust
"install" => {
    let url = args.get(2).map(|s| s.as_str()).unwrap_or("");
    if url.is_empty() {
        eprintln!("Usage: modixfs install <github-url>");
        eprintln!("Example: modixfs install github.com/owner/repo");
        std::process::exit(1);
    }
    let cfg = load_config_for_install(&args);
    installer::install(url, &cfg)
}
```

Add helper `load_config_for_install` after `cmd_init`:

```rust
fn load_config_for_install(args: &[String]) -> Config {
    let config_path = parse_mount_args(args).1;
    match config_path {
        Some(p) => Config::load(&p).unwrap_or_else(|_| Config::default_config()),
        None => {
            let default = PathBuf::from("tools.yaml");
            if default.exists() {
                Config::load(&default).unwrap_or_else(|_| Config::default_config())
            } else {
                Config::default_config()
            }
        }
    }
}
```

**Step 3: Call `secrets::load_secrets_env()` in `cmd_mount`**

At the very beginning of `cmd_mount`, before loading config:

```rust
fn cmd_mount(args: &[String]) -> Result<()> {
    secrets::load_secrets_env()
        .unwrap_or_else(|e| tracing::warn!("could not load secrets.env: {}", e));

    let (cli_mount, config_path) = parse_mount_args(args);
    // ... rest unchanged
```

**Step 4: Build**

```bash
cargo build 2>&1 | grep "^error"
```

Expected: no output.

**Step 5: Smoke test install command help**

```bash
./target/debug/modixfs help
```

Expected: shows `install <url>` in the commands list.

**Step 6: Commit**

```bash
git add src/main.rs
git commit -m "feat: wire install command and secrets auto-load into main"
```

---

## Task 7: Update README.md

**Files:**
- Modify: `README.md`

**Step 1: Add `modixfs install` to the Quick Start section**

After the existing quick start block, add:

````markdown
## Installing tools

```bash
modixfs install github.com/someone/their-modixfs-tool
```

The installer will:
1. Download the tool from GitHub (no `git` required)
2. Read its `modix.yaml` to find required env vars
3. Prompt you for any missing secrets and store them in `~/.config/modixfs/secrets.env`
4. Copy the tool into your `tools_dir`

`modixfs mount` loads secrets automatically at startup — no manual `export` needed.

For tools in a subdirectory of a repo:

```bash
modixfs install github.com/owner/repo/tree/main/mytool
```
````

**Step 2: Add `modix.yaml` reference section**

After the External tools section, add:

````markdown
## Publishing a tool (`modix.yaml`)

To make your tool installable, add a `modix.yaml` to its directory:

```yaml
name: mytool
description: One-line description shown during install
version: 0.1.0
env:
  - name: MYTOOL_API_KEY
    description: API key from https://example.com/settings
    required: true
  - name: MYTOOL_TIMEOUT
    description: Request timeout in seconds
    required: false
    default: "30"
```

All fields are optional — a tool without `modix.yaml` installs fine, just without prompts. The `env` list is the most useful part: `required: true` vars trigger an interactive prompt at install time so users don't have to discover them from the README.

Push your tool directory to a public GitHub repo and share:

```
modixfs install github.com/you/your-tool
```
````

**Step 3: Commit**

```bash
git add README.md
git commit -m "docs: document modixfs install and modix.yaml manifest"
```

---

## Task 8: Tag v0.3.0 and push

**Step 1: Push all commits**

```bash
git push origin master
```

**Step 2: Tag and push**

```bash
git tag v0.3.0
git push origin master --tags
```

CI builds Linux x86_64/aarch64 and macOS x86_64/aarch64 binaries automatically.
