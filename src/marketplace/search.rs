use serde::Deserialize;

#[derive(Deserialize, Debug)]
pub struct ToolSummary {
    pub owner: String,
    pub name: String,
    pub description: Option<String>,
    pub downloads: u64,
}

#[derive(Deserialize)]
struct SearchResponse {
    results: Vec<ToolSummary>,
}

pub fn search(query: &str) -> anyhow::Result<Vec<ToolSummary>> {
    let url = format!(
        "{}/api/search?q={}",
        super::REGISTRY_URL,
        urlencoding::encode(query)
    );
    let client = reqwest::blocking::Client::builder()
        .user_agent("livefolders")
        .build()?;
    let resp: SearchResponse = client.get(&url).send()?.error_for_status()?.json()?;
    Ok(resp.results)
}

#[cfg(test)]
mod tests {
    #[test]
    fn search_url_contains_query() {
        let url = format!(
            "{}/api/search?q={}",
            super::super::REGISTRY_URL,
            urlencoding::encode("hello world")
        );
        assert!(url.contains("hello%20world"));
    }
}
