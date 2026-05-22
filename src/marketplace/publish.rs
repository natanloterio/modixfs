use anyhow::Context;
use serde_json::json;
use std::time::{Duration, Instant};

// Set via env var LIVEFOLDERS_GITHUB_CLIENT_ID or register an OAuth App at
// https://github.com/settings/developers and hard-code the client_id here.
const DEFAULT_CLIENT_ID: &str = "Ov23litjBhDq65tLjGoP";

pub fn publish() -> anyhow::Result<()> {
    // 1. Detect repo slug from git remote
    let remote_out = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
        .context("failed to run git — are you in a git repository?")?;

    if !remote_out.status.success() {
        anyhow::bail!("[ERROR:CONFIG] no git remote 'origin' found. Add one with: git remote add origin https://github.com/owner/repo");
    }

    let remote_url = String::from_utf8(remote_out.stdout)
        .context("non-UTF8 remote URL")?
        .trim()
        .to_string();

    let repo_slug = extract_slug(&remote_url).ok_or_else(|| {
        anyhow::anyhow!(
            "[ERROR:CONFIG] could not parse GitHub remote URL: {}\nExpected: https://github.com/owner/repo or git@github.com:owner/repo",
            remote_url
        )
    })?;

    // 2. Warn if no git tags
    let no_tags = std::process::Command::new("git")
        .args(["tag", "--list"])
        .output()
        .map(|o| o.stdout.is_empty())
        .unwrap_or(true);
    if no_tags {
        eprintln!("Warning: no git tags found. Create a tag (e.g. git tag v0.1.0) for versioned installs.");
    }

    println!("Publishing {} to the LiveFolders registry.", repo_slug);

    // 3. Obtain a GitHub token via device flow
    let token = github_device_flow()?;

    // 4. POST to registry
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
        anyhow::bail!("[ERROR:REGISTRY] {}", msg);
    }

    if let Some(url) = body.get("url").and_then(|u| u.as_str()) {
        println!("Published! View at: {}", url);
    } else {
        println!("Published successfully.");
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
