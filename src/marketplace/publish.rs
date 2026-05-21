use anyhow::Context;
use serde_json::json;

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

    // 3. Prompt for token
    println!("Publishing {} to the LiveFolders registry.", repo_slug);
    println!("Enter a GitHub personal access token with 'repo' scope (input will be shown):");
    let mut token = String::new();
    std::io::stdin().read_line(&mut token).context("failed to read token")?;
    let token = token.trim().to_string();
    if token.is_empty() {
        anyhow::bail!("[ERROR:AUTH] token cannot be empty");
    }

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
