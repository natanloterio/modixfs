use anyhow::{bail, Context, Result};

pub struct GithubUrl {
    pub owner: String,
    pub repo: String,
    pub branch: String,
    pub subdir: Option<String>,
}

pub fn parse_github_url(url: &str) -> Result<GithubUrl> {
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

pub fn install(_url: &str, _cfg: &crate::config::Config) -> Result<()> {
    todo!("implemented in Task 5")
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
}
