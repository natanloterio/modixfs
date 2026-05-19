use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::Result;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::registry::ToolRegistry;
use crate::tools::ExternalTool;

pub fn spawn_watcher(
    tools_dir: PathBuf,
    registry: Arc<RwLock<ToolRegistry>>,
    timeout_secs: u64,
    sandbox_mode: crate::sandbox::SandboxMode,
) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<notify::Result<Event>>();

    let mut watcher = RecommendedWatcher::new(
        move |res| {
            let _ = tx.send(res);
        },
        notify::Config::default(),
    )?;

    watcher.watch(&tools_dir, RecursiveMode::NonRecursive)?;

    tokio::spawn(async move {
        let _watcher = watcher;
        while let Some(res) = rx.recv().await {
            match res {
                Ok(event) => handle_event(event, &tools_dir, &registry, timeout_secs, sandbox_mode),
                Err(e) => warn!("watcher error: {}", e),
            }
        }
    });

    Ok(())
}

fn handle_event(
    event: Event,
    tools_dir: &Path,
    registry: &Arc<RwLock<ToolRegistry>>,
    timeout_secs: u64,
    sandbox_mode: crate::sandbox::SandboxMode,
) {
    for path in &event.paths {
        let parent = path.parent();
        if parent != Some(tools_dir) {
            continue;
        }

        match event.kind {
            EventKind::Create(_) if path.is_dir() => {
                let name = match path.file_name() {
                    Some(n) => n.to_string_lossy().to_string(),
                    None => continue,
                };
                let mut reg = registry.write().unwrap_or_else(|e| e.into_inner());
                if reg.get(&name).is_none() {
                    reg.register(Arc::new(
                        ExternalTool::with_sandbox_mode(&name, path.clone(), timeout_secs, sandbox_mode)
                    ));
                    info!("hot-reload: registered tool '{}'", name);
                }
            }
            EventKind::Remove(_) => {
                let name = match path.file_name() {
                    Some(n) => n.to_string_lossy().to_string(),
                    None => continue,
                };
                let mut reg = registry.write().unwrap_or_else(|e| e.into_inner());
                reg.unregister(&name);
                info!("hot-reload: unregistered tool '{}'", name);
            }
            _ => {}
        }
    }
}
