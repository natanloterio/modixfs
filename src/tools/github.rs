use async_trait::async_trait;
use serde::Deserialize;

use crate::registry::{Session, Tool, ToolResult};

pub struct GitHubTool {
    token: String,
}

impl GitHubTool {
    pub fn new(token: impl Into<String>) -> Self {
        Self { token: token.into() }
    }
}

#[derive(Deserialize)]
struct SearchResponse {
    total_count: u64,
    items: Vec<SearchItem>,
}

#[derive(Deserialize)]
struct SearchItem {
    full_name: Option<String>,   // repos
    html_url: String,
    description: Option<String>,
    // code search fields
    name: Option<String>,
    path: Option<String>,
    repository: Option<RepoRef>,
}

#[derive(Deserialize)]
struct RepoRef {
    full_name: String,
}

#[async_trait]
impl Tool for GitHubTool {
    fn name(&self) -> &str {
        "github"
    }

    fn description(&self) -> &str {
        "Search GitHub repositories and code."
    }

    fn how_to(&self) -> &str {
        r#"# github

## Endpoints

### search_repos
Search GitHub repositories. Write a query string (GitHub search syntax).

```bash
echo "language:rust fuse filesystem" > /tools/github/search_repos
cat /tools/github/search_repos
```

### search_code
Search code across GitHub. Write a query string.

```bash
echo "fuser mount in:file language:rust" > /tools/github/search_code
cat /tools/github/search_code
```

## Query syntax
See https://docs.github.com/en/search-github/searching-on-github
Examples:
  - `tokio async runtime stars:>1000`
  - `org:rust-lang fuse`
  - `path:src/fs filename:vfs.rs`
"#
    }

    fn endpoints(&self) -> Vec<&str> {
        vec!["search_repos", "search_code"]
    }

    async fn invoke(&self, endpoint: &str, input: &[u8], _session: &Session) -> ToolResult {
        let query = match std::str::from_utf8(input) {
            Ok(s) => s.trim().to_string(),
            Err(_) => return ToolResult::err("input must be valid UTF-8"),
        };

        if query.is_empty() {
            return ToolResult::err("query is empty");
        }

        let client = reqwest::Client::new();

        match endpoint {
            "search_repos" => search_repos(&client, &self.token, &query).await,
            "search_code" => search_code(&client, &self.token, &query).await,
            _ => ToolResult::err(format!("unknown endpoint: {}", endpoint)),
        }
    }
}

async fn search_repos(client: &reqwest::Client, token: &str, query: &str) -> ToolResult {
    let resp = client
        .get("https://api.github.com/search/repositories")
        .header("Authorization", format!("Bearer {}", token))
        .header("User-Agent", "modixfs/0.1")
        .header("Accept", "application/vnd.github+json")
        .query(&[("q", query), ("per_page", "10")])
        .send()
        .await;

    let resp = match resp {
        Ok(r) => r,
        Err(e) => return ToolResult::err(format!("request failed: {}", e)),
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return ToolResult::err(format!("GitHub API error {}: {}", status, body));
    }

    let data: SearchResponse = match resp.json().await {
        Ok(d) => d,
        Err(e) => return ToolResult::err(format!("failed to parse response: {}", e)),
    };

    let mut out = format!("# GitHub Repository Search\n\nFound {} results\n\n", data.total_count);
    for item in &data.items {
        let name = item.full_name.as_deref().unwrap_or("unknown");
        let desc = item.description.as_deref().unwrap_or("(no description)");
        out.push_str(&format!("## {}\n{}\n{}\n\n", name, item.html_url, desc));
    }

    ToolResult::ok(out)
}

async fn search_code(client: &reqwest::Client, token: &str, query: &str) -> ToolResult {
    let resp = client
        .get("https://api.github.com/search/code")
        .header("Authorization", format!("Bearer {}", token))
        .header("User-Agent", "modixfs/0.1")
        .header("Accept", "application/vnd.github+json")
        .query(&[("q", query), ("per_page", "10")])
        .send()
        .await;

    let resp = match resp {
        Ok(r) => r,
        Err(e) => return ToolResult::err(format!("request failed: {}", e)),
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return ToolResult::err(format!("GitHub API error {}: {}", status, body));
    }

    let data: SearchResponse = match resp.json().await {
        Ok(d) => d,
        Err(e) => return ToolResult::err(format!("failed to parse response: {}", e)),
    };

    let mut out = format!("# GitHub Code Search\n\nFound {} results\n\n", data.total_count);
    for item in &data.items {
        let repo = item.repository.as_ref().map(|r| r.full_name.as_str()).unwrap_or("unknown");
        let path = item.path.as_deref().unwrap_or("");
        let name = item.name.as_deref().unwrap_or("");
        out.push_str(&format!("## {}/{}\n{}\n{}\n\n", repo, name, path, item.html_url));
    }

    ToolResult::ok(out)
}
