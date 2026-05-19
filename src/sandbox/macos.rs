use tokio::process::Command;
use crate::manifest::SandboxSpec;
use super::{Sandbox, SandboxMode, DEFAULT_READ_PATHS};

pub struct MacOsSandbox {
    profile: String,
    mode: SandboxMode,
}

impl MacOsSandbox {
    pub fn new(spec: Option<&SandboxSpec>, mode: SandboxMode) -> Self {
        Self {
            profile: build_sbpl_profile(spec),
            mode,
        }
    }
}

impl Sandbox for MacOsSandbox {
    fn apply(&self, cmd: &mut Command) {
        if !sandbox_exec_available() {
            if self.mode == SandboxMode::Strict {
                *cmd = Command::new("sh");
                cmd.arg("-c").arg(
                    "echo '[ERROR:SANDBOX] sandbox-exec not found; strict mode refuses to run' >&2; exit 1"
                );
            }
            return;
        }

        // Rewrite argv: sh -c <handler>  →  sandbox-exec -p <profile> sh -c <handler>
        let existing_args: Vec<std::ffi::OsString> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_owned())
            .collect();
        let prog = cmd.as_std().get_program().to_owned();

        *cmd = Command::new("sandbox-exec");
        cmd.arg("-p").arg(&self.profile);
        cmd.arg(prog);
        for arg in existing_args {
            cmd.arg(arg);
        }
    }
}

fn sandbox_exec_available() -> bool {
    std::process::Command::new("which")
        .arg("sandbox-exec")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub(crate) fn build_sbpl_profile(spec: Option<&SandboxSpec>) -> String {
    let mut lines = vec![
        "(version 1)".to_string(),
        "(deny default)".to_string(),
        "(allow process-exec*)".to_string(),
        "(allow process-fork)".to_string(),
        "(allow signal)".to_string(),
    ];

    let mut read_parts: Vec<String> = DEFAULT_READ_PATHS.iter()
        .map(|p| format!("(subpath \"{}\")", p))
        .collect();
    if let Some(s) = spec {
        for p in &s.fs.read {
            read_parts.push(format!("(subpath \"{}\")", p));
        }
    }
    lines.push(format!("(allow file-read* {})", read_parts.join(" ")));

    if let Some(s) = spec {
        if !s.fs.write.is_empty() {
            let write_parts: Vec<String> = s.fs.write.iter()
                .map(|p| format!("(subpath \"{}\")", p))
                .collect();
            lines.push(format!("(allow file-write* {})", write_parts.join(" ")));
        }
    }

    let allow_network = spec.and_then(|s| s.network).unwrap_or(false);
    if allow_network {
        lines.push("(allow network*)".to_string());
    } else {
        lines.push("(deny network*)".to_string());
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_contains_deny_default() {
        let profile = build_sbpl_profile(None);
        assert!(profile.contains("(deny default)"), "got: {profile}");
    }

    #[test]
    fn profile_denies_network_by_default() {
        let profile = build_sbpl_profile(None);
        assert!(profile.contains("(deny network*)"), "got: {profile}");
    }

    #[test]
    fn profile_allows_network_when_spec_says_true() {
        use crate::manifest::{SandboxSpec, SandboxFsSpec, SandboxResourceSpec};
        let spec = SandboxSpec {
            fs: SandboxFsSpec::default(),
            network: Some(true),
            resources: SandboxResourceSpec::default(),
        };
        let profile = build_sbpl_profile(Some(&spec));
        assert!(profile.contains("(allow network*)"), "got: {profile}");
    }

    #[test]
    fn profile_includes_custom_read_paths() {
        use crate::manifest::{SandboxSpec, SandboxFsSpec, SandboxResourceSpec};
        let spec = SandboxSpec {
            fs: SandboxFsSpec { read: vec!["/opt/homebrew".into()], write: vec![] },
            network: None,
            resources: SandboxResourceSpec::default(),
        };
        let profile = build_sbpl_profile(Some(&spec));
        assert!(profile.contains("/opt/homebrew"), "got: {profile}");
    }

    #[test]
    fn profile_includes_default_system_paths() {
        let profile = build_sbpl_profile(None);
        assert!(profile.contains("/usr"), "got: {profile}");
        assert!(profile.contains("/lib"), "got: {profile}");
    }
}
