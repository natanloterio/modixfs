use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};
use std::fs;

// ── Prerequisite check ─────────────────────────────────────────────────────────

/// Returns true if the test should be skipped (missing API key or claude CLI).
fn prerequisites_missing() -> bool {
    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        eprintln!("Skipping: ANTHROPIC_API_KEY not set");
        return true;
    }
    let has_claude = Command::new("claude")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !has_claude {
        eprintln!("Skipping: claude CLI not found on PATH");
        return true;
    }
    false
}

// ── Binary build ───────────────────────────────────────────────────────────────

fn build_livefolders_binary() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let status = Command::new("cargo")
        .args(["build", "--bin", "livefolders"])
        .current_dir(&manifest)
        .status()
        .expect("cargo build");
    assert!(status.success(), "cargo build --bin livefolders failed");
    manifest.join("target/debug/livefolders")
}
