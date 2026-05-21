use serde::Deserialize;

#[derive(Deserialize, Debug)]
pub struct ToolDetail {
    pub owner: String,
    pub name: String,
    pub description: Option<String>,
    pub repo_url: String,
    pub downloads: u64,
    pub updated_at: String,
}

pub fn get_info(owner: &str, name: &str) -> anyhow::Result<ToolDetail> {
    let url = format!("{}/api/tools/{}/{}", super::REGISTRY_URL, owner, name);
    let client = reqwest::blocking::Client::builder()
        .user_agent("livefolders")
        .build()?;
    let detail: ToolDetail = client.get(&url).send()?.error_for_status()?.json()?;
    Ok(detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_url_is_correct() {
        let url = format!("{}/api/tools/alice/weather", super::super::REGISTRY_URL);
        assert!(url.ends_with("/api/tools/alice/weather"));
    }
}
