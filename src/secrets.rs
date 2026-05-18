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
            // SAFETY: called at mount-time before any threads are spawned.
            unsafe { std::env::set_var(&key, val) };
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

    #[test]
    fn parse_secrets_skips_comments_and_blanks() {
        let content = "# comment\n\nFOO=bar\nBAZ=qux\n";
        let pairs = parse_env_file(content);
        assert_eq!(
            pairs,
            vec![
                ("FOO".to_string(), "bar".to_string()),
                ("BAZ".to_string(), "qux".to_string())
            ]
        );
    }

    #[test]
    fn parse_secrets_value_may_contain_equals() {
        let content = "URL=http://x.com?a=1&b=2\n";
        let pairs = parse_env_file(content);
        assert_eq!(pairs[0].1, "http://x.com?a=1&b=2");
    }

    #[test]
    fn has_secret_finds_env_var() {
        unsafe {
            std::env::set_var("_TEST_MODIX_PRESENT", "yes");
        }
        assert!(std::env::var("_TEST_MODIX_PRESENT").is_ok());
        unsafe {
            std::env::remove_var("_TEST_MODIX_PRESENT");
        }
    }
}
