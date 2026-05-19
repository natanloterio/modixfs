use tokio::process::Command;
use super::Sandbox;

pub struct NoopSandbox;

impl Sandbox for NoopSandbox {
    fn apply(&self, _cmd: &mut Command) {}
}
