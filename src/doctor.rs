use crate::config::Config;

pub struct CheckResult {
    pub name: &'static str,
    pub ok: bool,
    pub message: String,
}

pub fn check_fuse() -> CheckResult {
    let exists = std::path::Path::new("/dev/fuse").exists();
    CheckResult {
        name: "FUSE device",
        ok: exists,
        message: if exists {
            "/dev/fuse exists".into()
        } else {
            "/dev/fuse not found. Install FUSE: sudo apt-get install fuse3".into()
        },
    }
}

pub fn check_config_exists() -> CheckResult {
    check_config_exists_in(&std::env::current_dir().unwrap_or_default())
}

pub(crate) fn check_config_exists_in(base: &std::path::Path) -> CheckResult {
    let exists = base.join("livefolders.yaml").exists();
    CheckResult {
        name: "livefolders.yaml",
        ok: exists,
        message: if exists {
            "livefolders.yaml found".into()
        } else {
            "livefolders.yaml not found in current directory. Run `livefolders init` to create one.".into()
        },
    }
}

pub fn check_tools_dir(cfg: &Config) -> CheckResult {
    match cfg.resolved_tools_dir() {
        Err(e) => CheckResult {
            name: "tools_dir",
            ok: false,
            message: format!("tools_dir config error: {}", e),
        },
        Ok(None) => CheckResult {
            name: "tools_dir",
            ok: false,
            message: "tools_dir is not set in livefolders.yaml. Add: tools_dir: ~/.config/livefolders/tools".into(),
        },
        Ok(Some(p)) => {
            let exists = p.exists();
            CheckResult {
                name: "tools_dir",
                ok: exists,
                message: if exists {
                    format!("{} exists", p.display())
                } else {
                    format!("{} does not exist. Run `livefolders install <url>` to create it.", p.display())
                },
            }
        }
    }
}

pub fn check_tool_manifests(cfg: &Config) -> Vec<CheckResult> {
    let tools_dir = match cfg.resolved_tools_dir().ok().flatten() {
        Some(p) if p.exists() => p,
        _ => return vec![],
    };
    let Ok(entries) = std::fs::read_dir(&tools_dir) else { return vec![] };
    entries.flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| {
            let path = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if !path.join("folder.yaml").exists() {
                return CheckResult {
                    name: "tool manifest",
                    ok: false,
                    message: format!("tool '{}' has no folder.yaml", name),
                };
            }
            match crate::manifest::Manifest::load(&path) {
                Ok(Some(m)) => match m.validate() {
                    Ok(()) => CheckResult {
                        name: "tool manifest",
                        ok: true,
                        message: format!("tool '{}' folder.yaml is valid", name),
                    },
                    Err(e) => CheckResult {
                        name: "tool manifest",
                        ok: false,
                        message: format!("tool '{}' folder.yaml is invalid: {}", name, e),
                    },
                },
                _ => CheckResult {
                    name: "tool manifest",
                    ok: false,
                    message: format!("tool '{}' folder.yaml could not be read", name),
                },
            }
        })
        .collect()
}

pub fn check_sandbox() -> CheckResult {
    #[cfg(target_os = "linux")]
    {
        use landlock::{AccessFs, Ruleset, RulesetAttr};
        match Ruleset::default().handle_access(AccessFs::ReadFile) {
            Ok(_) => CheckResult {
                name: "Sandbox",
                ok: true,
                message: "Landlock available — full filesystem isolation enabled".into(),
            },
            Err(_) => CheckResult {
                name: "Sandbox",
                ok: false,
                message: "Landlock not available (kernel < 5.13). \
                          Tool isolation will degrade to NO_NEW_PRIVS + setrlimit only."
                    .into(),
            },
        }
    }

    #[cfg(target_os = "macos")]
    {
        let available = std::process::Command::new("which")
            .arg("sandbox-exec")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if available {
            CheckResult {
                name: "Sandbox",
                ok: true,
                message: "sandbox-exec found — macOS sandbox available".into(),
            }
        } else {
            CheckResult {
                name: "Sandbox",
                ok: false,
                message: "sandbox-exec not found. \
                          Tool isolation will be limited to setrlimit only."
                    .into(),
            }
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        CheckResult {
            name: "Sandbox",
            ok: true,
            message: "Sandbox checks not applicable on this platform".into(),
        }
    }
}

pub fn run_doctor(cfg: &Config) {
    let mut all_ok = true;
    let checks = [check_fuse(), check_config_exists(), check_tools_dir(cfg), check_sandbox()];
    let tool_checks = check_tool_manifests(cfg);
    for check in checks.iter().chain(tool_checks.iter()) {
        let icon = if check.ok { "OK" } else { "FAIL" };
        println!("[{}] {} — {}", icon, check.name, check.message);
        if !check.ok { all_ok = false; }
    }
    if all_ok {
        println!("\nAll checks passed.");
    } else {
        println!("\nSome checks failed. Fix the issues above and run `livefolders doctor` again.");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_fuse_returns_a_result_with_message() {
        let result = check_fuse();
        assert!(!result.name.is_empty());
        assert!(!result.message.is_empty());
    }

    #[test]
    fn check_config_exists_fails_in_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        // Write a known-absent livefolders.yaml by NOT creating it
        let result = check_config_exists_in(tmp.path());
        assert!(!result.ok);
        assert!(result.message.contains("livefolders.yaml"));
    }

    #[test]
    fn check_config_exists_passes_when_file_present() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("livefolders.yaml"), "mount: .livefolders\n").unwrap();
        let result = check_config_exists_in(tmp.path());
        assert!(result.ok);
    }

    #[test]
    fn check_tools_dir_fails_when_dir_missing() {
        let mut cfg = Config::default_config();
        cfg.tools_dir = Some(std::path::PathBuf::from("/nonexistent/path/tools"));
        let result = check_tools_dir(&cfg);
        assert!(!result.ok);
    }
}
