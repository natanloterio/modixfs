use anyhow::{bail, Context, Result};

pub struct GithubUrl {
    pub owner: String,
    pub repo: String,
    pub branch: String,
    pub subdir: Option<String>,
}

pub fn parse_github_url(url: &str) -> Result<GithubUrl> {
    let url = url.trim_end_matches('/');
    let url = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");

    let url = url
        .strip_prefix("github.com/")
        .with_context(|| format!("URL must start with github.com/: {}", url))?;

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

pub fn install(url: &str, cfg: &crate::config::Config) -> Result<()> {
    use std::io::Write;

    let tools_dir = cfg.resolved_tools_dir()?.ok_or_else(|| {
        anyhow::anyhow!(
            "tools_dir is not configured in livefolders.yaml. Add:\n  tools_dir: ~/.config/livefolders/tools"
        )
    })?;

    let gh = parse_github_url(url)?;

    println!("Downloading {}/{}...", gh.owner, gh.repo);

    let tarball_url = format!(
        "https://api.github.com/repos/{}/{}/tarball/{}",
        gh.owner, gh.repo, gh.branch
    );

    let client = reqwest::blocking::Client::builder()
        .user_agent("livefolders")
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
    // tar 0.4.26+ validates entries against path traversal during unpack.
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
            tracing::warn!("no folder.yaml found — installing without manifest");
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
            // SAFETY: install() is called from synchronous main() before any Tokio runtime is created.
            unsafe { std::env::set_var(&decl.name, &value); }
        }
    }

    std::fs::create_dir_all(&tools_dir)?;
    let dest = tools_dir.join(&tool_name);

    if dest.exists() {
        print!("Tool '{}' already exists. Overwrite? [y/N]: ", tool_name);
        std::io::stdout().flush()?;
        let mut ans = String::new();
        std::io::stdin().read_line(&mut ans)?;
        let a = ans.trim().to_ascii_lowercase();
        if a != "y" && a != "yes" {
            println!("Aborted.");
            return Ok(());
        }
        std::fs::remove_dir_all(&dest)?;
    }

    copy_dir_all(&tool_src, &dest)?;

    if let Some(ref mcp) = manifest.mcp {
        match register_mcp_server(&mcp.server, &mcp.command, &mcp.args) {
            Ok(()) => println!("Registered MCP server '{}'.", mcp.server),
            Err(e) => tracing::warn!("could not register MCP server: {}", e),
        }
    }

    println!("Installed {} → {}", tool_name, dest.display());
    println!("Run `livefolders mount` to start using it (or it will appear automatically if already mounted).");

    Ok(())
}

/// Install a tool directly from a tarball URL (used by registry installs).
pub fn install_from_tarball_url(
    tarball_url: &str,
    tool_name: &str,
    cfg: &crate::config::Config,
) -> anyhow::Result<()> {
    use std::io::Write;

    let tools_dir = cfg.resolved_tools_dir()?.ok_or_else(|| {
        anyhow::anyhow!(
            "tools_dir is not configured. Add:\n  tools_dir: ~/.config/livefolders/tools"
        )
    })?;

    println!("Downloading {}...", tool_name);

    let client = reqwest::blocking::Client::builder()
        .user_agent("livefolders")
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()?;

    let mut req = client.get(tarball_url);
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        req = req.header("Authorization", format!("Bearer {}", token));
    }

    let bytes = req
        .send()
        .context("downloading tarball")?
        .error_for_status()
        .context("tarball download failed")?
        .bytes()
        .context("reading response body")?;

    println!("Extracting...");
    let tmp = tempfile::tempdir()?;
    let gz = flate2::read::GzDecoder::new(bytes.as_ref());
    let mut archive = tar::Archive::new(gz);
    archive.unpack(tmp.path())?;

    let top_level = std::fs::read_dir(tmp.path())?
        .flatten()
        .find(|e| e.path().is_dir())
        .context("unexpected tarball structure: no top-level directory")?
        .path();

    std::fs::create_dir_all(&tools_dir)?;
    let dest = tools_dir.join(tool_name);

    if dest.exists() {
        print!("Tool '{}' already exists. Overwrite? [y/N]: ", tool_name);
        std::io::stdout().flush()?;
        let mut ans = String::new();
        std::io::stdin().read_line(&mut ans)?;
        let a = ans.trim().to_ascii_lowercase();
        if a != "y" && a != "yes" {
            println!("Aborted.");
            return Ok(());
        }
        std::fs::remove_dir_all(&dest)?;
    }

    copy_dir_all(&top_level, &dest)?;

    if let Ok(Some(manifest)) = crate::manifest::Manifest::load(&dest) {
        if let Some(ref mcp) = manifest.mcp {
            match register_mcp_server(&mcp.server, &mcp.command, &mcp.args) {
                Ok(()) => println!("Registered MCP server '{}'.", mcp.server),
                Err(e) => tracing::warn!("could not register MCP server: {}", e),
            }
        }
    }

    println!("Installed {} → {}", tool_name, dest.display());
    println!("Run `livefolders mount` to start using it.");
    Ok(())
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Default)]
struct McpServersConfig {
    #[serde(default)]
    servers: std::collections::HashMap<String, McpServerEntry>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct McpServerEntry {
    command: String,
    #[serde(default)]
    args: Vec<String>,
}

/// Write/merge an MCP server entry into `<config_dir>/mcp-servers.yaml`.
pub fn register_mcp_server_to_dir(
    config_dir: &std::path::Path,
    name: &str,
    command: &str,
    args: &[String],
) -> anyhow::Result<()> {
    std::fs::create_dir_all(config_dir)?;
    let path = config_dir.join("mcp-servers.yaml");
    let existing: McpServersConfig = if path.exists() {
        serde_yaml::from_str(&std::fs::read_to_string(&path)?)?
    } else {
        McpServersConfig::default()
    };
    let new_entry = McpServerEntry { command: command.to_string(), args: args.to_vec() };
    let servers = existing.servers.into_iter()
        .chain(std::iter::once((name.to_string(), new_entry)))
        .collect();
    let updated = McpServersConfig { servers };
    std::fs::write(&path, serde_yaml::to_string(&updated)?)?;
    Ok(())
}

pub fn register_mcp_server(name: &str, command: &str, args: &[String]) -> anyhow::Result<()> {
    let home = std::env::var("HOME").context("HOME environment variable is not set")?;
    let config_dir = std::path::PathBuf::from(&home)
        .join(".config")
        .join("livefolders");
    register_mcp_server_to_dir(&config_dir, name, command, args)
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

    #[test]
    fn parse_trailing_slash() {
        let u = parse_github_url("github.com/alice/mytool/").unwrap();
        assert_eq!(u.owner, "alice");
        assert_eq!(u.repo, "mytool");
    }

    #[test]
    fn register_mcp_server_writes_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        let config_dir = tmp.path().to_path_buf();
        super::register_mcp_server_to_dir(
            &config_dir,
            "brave-search",
            "npx",
            &["-y".to_string(), "@mcp/server-brave-search".to_string()],
        ).unwrap();
        let path = config_dir.join("mcp-servers.yaml");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("brave-search"));
        assert!(content.contains("npx"));
    }

    #[test]
    fn register_mcp_server_merges_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        super::register_mcp_server_to_dir(&dir, "tool-a", "node", &[]).unwrap();
        super::register_mcp_server_to_dir(&dir, "tool-b", "python", &["-m".to_string(), "mcp_b".to_string()]).unwrap();
        let content = std::fs::read_to_string(dir.join("mcp-servers.yaml")).unwrap();
        assert!(content.contains("tool-a"));
        assert!(content.contains("tool-b"));
    }
}
