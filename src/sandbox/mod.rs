pub mod noop;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;

use crate::manifest::SandboxSpec;

/// Applies a sandbox policy to a Command before it is spawned.
pub trait Sandbox: Send + Sync {
    fn apply(&self, cmd: &mut tokio::process::Command);
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SandboxMode {
    /// Refuse to run if the sandbox cannot be fully applied.
    Strict,
    /// Apply what is available; log a warning when degraded.
    Warn,
    /// Skip sandboxing entirely (debugging only).
    Disabled,
}

impl Default for SandboxMode {
    fn default() -> Self { SandboxMode::Warn }
}

/// Built-in default filesystem read paths allowed for every tool.
pub const DEFAULT_READ_PATHS: &[&str] = &[
    "/usr", "/lib", "/lib64", "/etc/ssl/certs", "/etc/localtime",
    "/etc/nsswitch.conf", "/etc/hosts", "/tmp",
];

/// Built-in default executable paths allowed for every tool.
pub const DEFAULT_EXEC_PATHS: &[&str] = &[
    "/usr/bin", "/bin", "/usr/local/bin",
];

/// Returns the active sandbox implementation for the current platform.
pub fn build(_spec: Option<&SandboxSpec>, mode: SandboxMode) -> Box<dyn Sandbox> {
    if mode == SandboxMode::Disabled {
        return Box::new(noop::NoopSandbox);
    }
    #[cfg(target_os = "linux")]
    return Box::new(linux::LinuxSandbox::new(_spec, mode));
    #[cfg(target_os = "macos")]
    return Box::new(macos::MacOsSandbox::new(_spec, mode));
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    Box::new(noop::NoopSandbox)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_mode_returns_noop_that_does_not_panic() {
        let sandbox = build(None, SandboxMode::Disabled);
        let mut cmd = tokio::process::Command::new("true");
        sandbox.apply(&mut cmd);
    }
}
