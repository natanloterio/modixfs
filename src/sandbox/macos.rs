// src/sandbox/macos.rs — stub until Task 4 fills this in
use super::{Sandbox, SandboxMode};
use crate::manifest::SandboxSpec;
use tokio::process::Command;

pub struct MacOsSandbox;

impl MacOsSandbox {
    pub fn new(_spec: Option<&SandboxSpec>, _mode: SandboxMode) -> Self { Self }
}

impl Sandbox for MacOsSandbox {
    fn apply(&self, _cmd: &mut Command) {}
}
