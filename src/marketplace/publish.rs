use anyhow::Context;
use serde_json::json;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

// Set via env var LIVEFOLDERS_GITHUB_CLIENT_ID or register an OAuth App at
// https://github.com/settings/developers and hard-code the client_id here.
const DEFAULT_CLIENT_ID: &str = "Ov23litjBhDq65tLjGoP";

pub fn publish(repo_arg: Option<&str>) -> anyhow::Result<()> {
    let cwd = std::env::current_dir().context("could not determine current directory")?;
    let in_tool_dir = cwd.join("folder.yaml").exists() && !cwd.join(".git").exists();

    if in_tool_dir {
        let slug = match repo_arg {
            Some(s) => s.to_string(),
            None => prompt_repo_slug()?,
        };
        bootstrap_and_publish(&slug, &cwd)
    } else {
        publish_from_dir(&cwd, None)
    }
}

fn bootstrap_and_publish(slug: &str, src: &Path) -> anyhow::Result<()> {
    println!("Bootstrapping repository for {}...", slug);

    let tmp = tempfile::tempdir().context("failed to create temp directory")?;
    let clone_dir = tmp.path().join("repo");

    // Clone the GitHub repo
    let clone_url = format!("https://github.com/{}", slug);
    println!("Cloning {}...", clone_url);
    let status = std::process::Command::new("git")
        .args(["clone", &clone_url, clone_dir.to_str().context("non-UTF8 temp path")?])
        .status()
        .context("failed to run git clone")?;
    if !status.success() {
        anyhow::bail!(
            "[ERROR:GIT] git clone failed — check that {} exists and you have access",
            clone_url
        );
    }

    // Collect and copy publishable files
    let files = collect_publishable_files(src)?;
    println!("Copying {} file(s)...", files.len());
    for file in &files {
        let dest = clone_dir.join(file.file_name().context("file has no name")?);
        std::fs::copy(file, &dest)
            .with_context(|| format!("failed to copy {}", file.display()))?;
    }

    // Commit if there are changes
    let status_out = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&clone_dir)
        .output()
        .context("failed to run git status")?;

    if status_out.stdout.is_empty() {
        println!("No changes to commit — files already up to date.");
    } else {
        run_git(&clone_dir, &["add", "."])?;
        run_git(&clone_dir, &["commit", "-m", "feat: add tool definition"])?;

        let tag_out = std::process::Command::new("git")
            .args(["tag", "--list"])
            .current_dir(&clone_dir)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();
        let existing: Vec<&str> = tag_out.lines().collect();
        let new_tag = next_version_tag(&existing);
        run_git(&clone_dir, &["tag", &new_tag])?;
        println!("Created tag {}.", new_tag);

        if let Err(e) = run_git(&clone_dir, &["push"])
            .and_then(|_| run_git(&clone_dir, &["push", "--tags"]))
        {
            let saved = tmp.keep();
            anyhow::bail!(
                "{}\n\n\
                 The committed repo was saved to: {}\n\
                 Fix credentials (e.g. gh auth login) then run:\n  \
                 cd {} && git push && git push --tags\n  \
                 livefolders publish {}",
                e,
                saved.join("repo").display(),
                saved.join("repo").display(),
                slug
            );
        }
        println!("Pushed to GitHub.");
    }

    publish_from_dir(&clone_dir, None)
}

fn publish_from_dir(dir: &Path, token: Option<String>) -> anyhow::Result<()> {
    let repo_slug = get_remote_slug(dir)?;

    let no_tags = std::process::Command::new("git")
        .args(["tag", "--list"])
        .current_dir(dir)
        .output()
        .map(|o| o.stdout.is_empty())
        .unwrap_or(true);
    if no_tags {
        eprintln!("Warning: no git tags found. Create a tag (e.g. git tag v0.1.0) for versioned installs.");
    }

    println!("Publishing {} to the LiveFolders registry.", repo_slug);

    let token = match token {
        Some(t) => t,
        None => github_device_flow()?,
    };
    post_to_registry(&repo_slug, &token)
}

fn get_remote_slug(dir: &Path) -> anyhow::Result<String> {
    let out = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(dir)
        .output()
        .context("failed to run git — are you in a git repository?")?;

    if !out.status.success() {
        anyhow::bail!(
            "[ERROR:CONFIG] no git remote 'origin' found. Add one with: git remote add origin https://github.com/owner/repo"
        );
    }

    let url = String::from_utf8(out.stdout)
        .context("non-UTF8 remote URL")?
        .trim()
        .to_string();

    extract_slug(&url).ok_or_else(|| {
        anyhow::anyhow!(
            "[ERROR:CONFIG] could not parse GitHub remote URL: {}\nExpected: https://github.com/owner/repo or git@github.com:owner/repo",
            url
        )
    })
}

fn post_to_registry(repo_slug: &str, token: &str) -> anyhow::Result<()> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("livefolders")
        .build()?;

    let resp = client
        .post(format!("{}/api/publish", super::REGISTRY_URL))
        .json(&json!({ "token": token, "repo": repo_slug }))
        .send()
        .context("failed to reach registry")?;

    let status = resp.status();
    let body: serde_json::Value = resp.json().context("invalid JSON response from registry")?;

    if !status.is_success() {
        let msg = body.get("error").and_then(|e| e.as_str()).unwrap_or("unknown error");
        if msg.contains("folder.yaml") {
            anyhow::bail!(
                "[ERROR:REGISTRY] {}\n\n\
                 `livefolders publish` must be run from a GitHub repo that has a folder.yaml at its root.\n\n\
                 To create a publishable tool:\n\
                   1. Create a new repo for your tool (e.g. github.com/you/my-tool)\n\
                   2. Add a folder.yaml describing the tool's endpoints\n\
                   3. Push and run `livefolders publish` from that repo\n\n\
                 Docs: https://livefoldersfs.org/publish",
                msg
            );
        }
        anyhow::bail!("[ERROR:REGISTRY] {}", msg);
    }

    if let Some(url) = body.get("url").and_then(|u| u.as_str()) {
        println!("Published! View at: {}", url);
    } else {
        println!("Published successfully.");
    }

    Ok(())
}

fn collect_publishable_files(src: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let endpoint_names = endpoint_names_from_manifest(src);
    let always_exclude: HashSet<&str> = ["how_to.md", "schema.json"].iter().copied().collect();

    let mut result = Vec::new();
    for entry in std::fs::read_dir(src)
        .with_context(|| format!("reading directory {}", src.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if name_str.ends_with(".log") {
            continue;
        }
        if always_exclude.contains(name_str.as_ref()) {
            continue;
        }
        if endpoint_names.contains(name_str.as_ref()) {
            continue;
        }

        result.push(path);
    }
    Ok(result)
}

fn endpoint_names_from_manifest(src: &Path) -> HashSet<String> {
    let Ok(Some(manifest)) = crate::manifest::Manifest::load(src) else {
        return HashSet::new();
    };
    manifest.files.iter().map(|f| f.name.clone()).collect()
}

fn next_version_tag(tags: &[&str]) -> String {
    let latest = tags.iter().filter_map(|t| {
        let s = t.trim_start_matches('v');
        let parts: Vec<u64> = s.split('.').filter_map(|p| p.parse().ok()).collect();
        if parts.len() == 3 { Some((parts[0], parts[1], parts[2])) } else { None }
    }).max();
    match latest {
        Some((maj, min, patch)) => format!("v{}.{}.{}", maj, min, patch + 1),
        None => "v0.1.0".to_string(),
    }
}

fn prompt_repo_slug() -> anyhow::Result<String> {
    use std::io::Write;
    print!("GitHub repository (owner/name): ");
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let slug = input.trim().to_string();
    if slug.is_empty() || !slug.contains('/') {
        anyhow::bail!("[ERROR:CONFIG] expected owner/name format (e.g. alice/my-tool)");
    }
    Ok(slug)
}

fn run_git(dir: &Path, args: &[&str]) -> anyhow::Result<()> {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .with_context(|| format!("failed to run: git {}", args.join(" ")))?;
    if !status.success() {
        anyhow::bail!("[ERROR:GIT] git {} failed", args.join(" "));
    }
    Ok(())
}

fn github_device_flow() -> anyhow::Result<String> {
    let client_id = std::env::var("LIVEFOLDERS_GITHUB_CLIENT_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_CLIENT_ID.to_string());

    if client_id.is_empty() {
        anyhow::bail!(
            "[ERROR:AUTH] no GitHub OAuth client_id configured.\n\
             Set LIVEFOLDERS_GITHUB_CLIENT_ID or register an app at https://github.com/settings/developers"
        );
    }

    let client = reqwest::blocking::Client::builder()
        .user_agent("livefolders")
        .build()?;

    // Request device code
    let resp = client
        .post("https://github.com/login/device/code")
        .header("Accept", "application/json")
        .form(&[("client_id", client_id.as_str()), ("scope", "repo")])
        .send()
        .context("failed to contact GitHub")?;

    let device: serde_json::Value = resp.json().context("invalid response from GitHub device endpoint")?;

    if let Some(err) = device["error"].as_str() {
        let desc = device["error_description"].as_str().unwrap_or(err);
        anyhow::bail!("[ERROR:AUTH] GitHub: {} — make sure Device Flow is enabled on the OAuth App at https://github.com/settings/developers", desc);
    }

    let device_code = device["device_code"].as_str().context("missing device_code")?;
    let user_code = device["user_code"].as_str().context("missing user_code")?;
    let verification_uri = device["verification_uri"].as_str().unwrap_or("https://github.com/login/device");
    let interval_secs = device["interval"].as_u64().unwrap_or(5);
    let expires_in = device["expires_in"].as_u64().unwrap_or(900);

    println!("\nTo authorize, open this URL in your browser:");
    println!("  {}", verification_uri);
    println!("\nEnter code: {}\n", user_code);

    // Try to open the browser automatically
    let _ = std::process::Command::new("xdg-open").arg(verification_uri).spawn()
        .or_else(|_| std::process::Command::new("open").arg(verification_uri).spawn());

    // Poll for access token
    let deadline = Instant::now() + Duration::from_secs(expires_in);
    let poll_interval = Duration::from_secs(interval_secs);

    loop {
        std::thread::sleep(poll_interval);

        if Instant::now() > deadline {
            anyhow::bail!("[ERROR:AUTH] device flow expired — run `livefolders publish` again");
        }

        let poll = client
            .post("https://github.com/login/oauth/access_token")
            .header("Accept", "application/json")
            .form(&[
                ("client_id", client_id.as_str()),
                ("device_code", device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .context("failed to poll GitHub token endpoint")?;

        let body: serde_json::Value = poll.json().context("invalid token response")?;

        if let Some(token) = body["access_token"].as_str() {
            println!("Authorized.");
            return Ok(token.to_string());
        }

        match body["error"].as_str() {
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                std::thread::sleep(Duration::from_secs(interval_secs));
            }
            Some("expired_token") => anyhow::bail!("[ERROR:AUTH] device code expired — run `livefolders publish` again"),
            Some("access_denied") => anyhow::bail!("[ERROR:AUTH] access denied by user"),
            Some(other) => anyhow::bail!("[ERROR:AUTH] {}", other),
            None => continue,
        }
    }
}

fn extract_slug(remote_url: &str) -> Option<String> {
    let cleaned = remote_url.trim_end_matches(".git");
    if let Some(path) = cleaned.strip_prefix("https://github.com/") {
        return Some(path.to_string());
    }
    if let Some(path) = cleaned.strip_prefix("git@github.com:") {
        return Some(path.to_string());
    }
    None
}

#[cfg(test)]
mod version_tests {
    use super::*;

    #[test]
    fn next_tag_with_no_existing_tags_returns_v0_1_0() {
        assert_eq!(next_version_tag(&[]), "v0.1.0");
    }

    #[test]
    fn next_tag_bumps_patch() {
        assert_eq!(next_version_tag(&["v0.1.0"]), "v0.1.1");
        assert_eq!(next_version_tag(&["v1.2.3"]), "v1.2.4");
    }

    #[test]
    fn next_tag_picks_highest_when_multiple_tags() {
        assert_eq!(next_version_tag(&["v0.1.0", "v0.2.0", "v0.1.5"]), "v0.2.1");
    }

    #[test]
    fn next_tag_ignores_non_semver_tags() {
        assert_eq!(next_version_tag(&["latest", "v0.1.0", "nightly"]), "v0.1.1");
    }

    #[test]
    fn next_tag_all_non_semver_falls_back_to_v0_1_0() {
        assert_eq!(next_version_tag(&["latest", "nightly"]), "v0.1.0");
    }
}

#[cfg(test)]
mod bootstrap_tests {
    use super::*;

    fn make_tool_dir(manifest_yaml: &str, files: &[(&str, &[u8])]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("folder.yaml"), manifest_yaml).unwrap();
        for (name, content) in files {
            std::fs::write(tmp.path().join(name), content).unwrap();
        }
        tmp
    }

    #[test]
    fn collect_publishable_excludes_endpoint_names() {
        let yaml = "files:\n  - name: data\n    type: write_invoke\n    handler: cat\n  - name: report\n    type: write_invoke\n    handler: cat\n";
        let tmp = make_tool_dir(yaml, &[("data", b""), ("report", b""), ("fmt.py", b"print(1)")]);
        let files = collect_publishable_files(tmp.path()).unwrap();
        let names: Vec<_> = files.iter().map(|p| p.file_name().unwrap().to_str().unwrap().to_string()).collect();
        assert!(!names.contains(&"data".to_string()), "should exclude endpoint 'data'");
        assert!(!names.contains(&"report".to_string()), "should exclude endpoint 'report'");
        assert!(names.contains(&"fmt.py".to_string()), "should include companion script");
        assert!(names.contains(&"folder.yaml".to_string()), "should include folder.yaml");
    }

    #[test]
    fn collect_publishable_excludes_log_files() {
        let yaml = "files:\n  - name: data\n    type: write_invoke\n    handler: cat\n";
        let tmp = make_tool_dir(yaml, &[("data.log", b"exit=0"), ("helper.sh", b"#!/bin/sh")]);
        let files = collect_publishable_files(tmp.path()).unwrap();
        let names: Vec<_> = files.iter().map(|p| p.file_name().unwrap().to_str().unwrap().to_string()).collect();
        assert!(!names.contains(&"data.log".to_string()), "should exclude .log files");
        assert!(names.contains(&"helper.sh".to_string()));
    }

    #[test]
    fn collect_publishable_excludes_generated_files() {
        let yaml = "files:\n  - name: data\n    type: write_invoke\n    handler: cat\n";
        let tmp = make_tool_dir(yaml, &[("how_to.md", b"# How to"), ("schema.json", b"{}")]);
        let files = collect_publishable_files(tmp.path()).unwrap();
        let names: Vec<_> = files.iter().map(|p| p.file_name().unwrap().to_str().unwrap().to_string()).collect();
        assert!(!names.contains(&"how_to.md".to_string()), "should exclude generated how_to.md");
        assert!(!names.contains(&"schema.json".to_string()), "should exclude generated schema.json");
    }

    #[test]
    fn collect_publishable_includes_companion_files() {
        let yaml = "files:\n  - name: data\n    type: write_invoke\n    handler: ./fetch.sh\n";
        let tmp = make_tool_dir(yaml, &[
            ("fetch.sh", b"#!/bin/sh\ncurl https://example.com"),
            ("config.json", b"{\"timeout\": 30}"),
            ("requirements.txt", b"requests==2.31.0"),
        ]);
        let files = collect_publishable_files(tmp.path()).unwrap();
        let names: Vec<_> = files.iter().map(|p| p.file_name().unwrap().to_str().unwrap().to_string()).collect();
        assert!(names.contains(&"fetch.sh".to_string()));
        assert!(names.contains(&"config.json".to_string()));
        assert!(names.contains(&"requirements.txt".to_string()));
        assert!(names.contains(&"folder.yaml".to_string()));
    }

    #[test]
    fn collect_publishable_always_includes_folder_yaml() {
        let yaml = "files: []\n";
        let tmp = make_tool_dir(yaml, &[]);
        let files = collect_publishable_files(tmp.path()).unwrap();
        let names: Vec<_> = files.iter().map(|p| p.file_name().unwrap().to_str().unwrap().to_string()).collect();
        assert!(names.contains(&"folder.yaml".to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_https_slug() {
        assert_eq!(
            extract_slug("https://github.com/alice/weather.git"),
            Some("alice/weather".to_string())
        );
    }

    #[test]
    fn extracts_ssh_slug() {
        assert_eq!(
            extract_slug("git@github.com:alice/weather.git"),
            Some("alice/weather".to_string())
        );
    }

    #[test]
    fn returns_none_for_non_github() {
        assert_eq!(extract_slug("https://gitlab.com/alice/weather.git"), None);
    }

    #[test]
    fn extracts_without_git_suffix() {
        assert_eq!(
            extract_slug("https://github.com/alice/weather"),
            Some("alice/weather".to_string())
        );
    }
}
