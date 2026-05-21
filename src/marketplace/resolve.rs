use serde::Deserialize;

#[derive(Deserialize, Debug)]
pub struct ResolveResponse {
    pub owner: String,
    pub name: String,
    pub version: String,
    pub tarball_url: String,
}

pub fn resolve(owner: &str, name: &str, version: Option<&str>) -> anyhow::Result<ResolveResponse> {
    let base = format!("{}/api/resolve/{}/{}", super::REGISTRY_URL, owner, name);
    let url = match version {
        Some(v) => format!("{}?version={}", base, v),
        None => base,
    };
    let client = reqwest::blocking::Client::builder()
        .user_agent("livefolders")
        .build()?;
    let resp: ResolveResponse = client.get(&url).send()?.error_for_status()?.json()?;
    Ok(resp)
}

#[cfg(test)]
mod tests {
    #[test]
    fn resolve_url_with_version() {
        let base = format!("{}/api/resolve/alice/weather", super::super::REGISTRY_URL);
        let url = format!("{}?version=v1.0.0", base);
        assert!(url.contains("v1.0.0"));
        assert!(url.contains("alice/weather"));
    }

    #[test]
    fn resolve_url_without_version() {
        let base = format!("{}/api/resolve/alice/weather", super::super::REGISTRY_URL);
        assert!(!base.contains("version"));
    }
}
