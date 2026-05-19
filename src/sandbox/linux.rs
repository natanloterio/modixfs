// src/sandbox/linux.rs — stub until Task 3 fills this in
use super::{Sandbox, SandboxMode};
use crate::manifest::SandboxSpec;
use tokio::process::Command;

pub struct LinuxSandbox;

impl LinuxSandbox {
    pub fn new(_spec: Option<&SandboxSpec>, _mode: SandboxMode) -> Self { Self }
}

impl Sandbox for LinuxSandbox {
    fn apply(&self, _cmd: &mut Command) {}
}
