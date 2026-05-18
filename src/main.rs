mod config;
mod fs;
mod registry;
mod tools;
mod watcher;

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::{bail, Context, Result};
use fuser::MountOption;
use tokio::runtime::Runtime;
use tracing::{info, warn};

use config::Config;
use fs::ModixFS;
use registry::{Session, ToolRegistry};
use tools::{EchoTool, ExternalTool, GitHubTool};

const USAGE: &str = "\
Usage: modixfs <command> [options]

Commands:
  init              Create a starter tools.yaml in the current directory
  mount [path]      Mount the filesystem (path overrides tools.yaml)
  help              Show this help

Options:
  --config, -c <file>   Path to tools.yaml (default: ./tools.yaml)

Environment:
  GITHUB_TOKEN      GitHub personal access token (for github tool)
";

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("modixfs=info".parse()?),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("help");

    match cmd {
        "init" => cmd_init(),
        "mount" => cmd_mount(&args),
        "help" | "--help" | "-h" => {
            print!("{}", USAGE);
            Ok(())
        }
        // Legacy: bare path argument mounts directly (backwards compat)
        path if !path.starts_with('-') => cmd_mount(&args),
        _ => {
            print!("{}", USAGE);
            bail!("unknown command: {}", cmd);
        }
    }
}

fn cmd_init() -> Result<()> {
    let path = PathBuf::from("tools.yaml");
    if path.exists() {
        bail!("tools.yaml already exists in the current directory");
    }
    std::fs::write(
        &path,
        r#"# ModixFS configuration
# Run `modixfs mount` to start the filesystem.

mount: /tmp/modixfs

tools:
  - name: echo

  # Uncomment and set GITHUB_TOKEN to enable GitHub search:
  # - name: github
  #   token_env: GITHUB_TOKEN
"#,
    )?;
    println!("Created tools.yaml");
    println!("Edit it to configure your tools, then run: modixfs mount");
    Ok(())
}

fn cmd_mount(args: &[String]) -> Result<()> {
    let (cli_mount, config_path) = parse_mount_args(args);

    let cfg = match config_path {
        Some(p) => Config::load(&p)?,
        None => {
            let default_path = PathBuf::from("tools.yaml");
            if default_path.exists() {
                Config::load(&default_path)?
            } else {
                Config::default_config()
            }
        }
    };

    let mountpoint = cli_mount
        .or_else(|| cfg.mount.clone())
        .unwrap_or_else(|| PathBuf::from("/tmp/modixfs"));

    std::fs::create_dir_all(&mountpoint)
        .with_context(|| format!("creating mountpoint {}", mountpoint.display()))?;

    let mut registry = build_registry(&cfg);
    load_external_tools(&cfg, &mut registry)?;
    let registry = Arc::new(RwLock::new(registry));

    let session = Session::new();
    let rt = Runtime::new()?;
    let handle = rt.handle().clone();

    let options = vec![
        MountOption::RW,
        MountOption::FSName("modixfs".to_string()),
        MountOption::AutoUnmount,
        MountOption::AllowOther,
    ];

    info!("mounting modixfs at {}", mountpoint.display());
    println!("Mounted at {}  (Ctrl-C or unmount to stop)", mountpoint.display());

    let tools_dir = cfg.resolved_tools_dir().unwrap_or(None);

    if let Some(ref td) = tools_dir {
        if td.exists() {
            let _guard = rt.enter();
            if let Err(e) = watcher::spawn_watcher(td.clone(), Arc::clone(&registry), cfg.timeout_secs) {
                warn!("hot-reload watcher failed to start: {}", e);
            } else {
                info!("hot-reload watcher started");
            }
        }
    }

    let fs = ModixFS::new(registry, tools_dir, session, handle);
    fuser::mount2(fs, &mountpoint, &options)?;

    Ok(())
}

fn load_external_tools(cfg: &Config, registry: &mut ToolRegistry) -> Result<()> {
    let tools_dir = match cfg.resolved_tools_dir()? {
        Some(p) => p,
        None => return Ok(()),
    };
    if !tools_dir.exists() {
        warn!("tools_dir does not exist: {}", tools_dir.display());
        return Ok(());
    }
    let entries = std::fs::read_dir(&tools_dir)
        .with_context(|| format!("reading tools_dir {}", tools_dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() { continue; }
        let name = entry.file_name().to_string_lossy().to_string();
        if registry.get(&name).is_some() {
            info!("skipping external tool '{}': shadowed by built-in", name);
            continue;
        }
        registry.register(Arc::new(ExternalTool::new(&name, path, cfg.timeout_secs)));
        info!("registered external tool: {}", name);
    }
    Ok(())
}

fn build_registry(cfg: &Config) -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    for tool_cfg in &cfg.tools {
        match tool_cfg.name.as_str() {
            "echo" => {
                registry.register(Arc::new(EchoTool));
                info!("registered tool: echo");
            }
            "github" => match tool_cfg.resolve_token() {
                Some(token) => {
                    registry.register(Arc::new(GitHubTool::new(token)));
                    info!("registered tool: github");
                }
                None => {
                    let env_var = tool_cfg
                        .token_env
                        .clone()
                        .unwrap_or_else(|| "GITHUB_TOKEN".to_string());
                    warn!("github tool skipped: ${} not set", env_var);
                }
            },
            other => warn!("unknown tool in config: {}", other),
        }
    }

    registry
}

fn parse_mount_args(args: &[String]) -> (Option<PathBuf>, Option<PathBuf>) {
    let mut mount = None;
    let mut config = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--config" | "-c" => {
                i += 1;
                if i < args.len() {
                    config = Some(PathBuf::from(&args[i]));
                }
            }
            // skip "mount" subcommand token itself
            "mount" => {}
            arg if !arg.starts_with('-') => {
                mount = Some(PathBuf::from(arg));
            }
            _ => {}
        }
        i += 1;
    }
    (mount, config)
}
