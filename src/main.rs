mod config;
mod fs;
mod installer;
mod manifest;
mod registry;
mod secrets;
mod tools;
mod watcher;

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::{bail, Context, Result};
use fuser::MountOption;
use tokio::runtime::Runtime;
use tracing::{info, warn};

use config::Config;
use fs::LiveFolders;
use registry::{Session, ToolRegistry};
use tools::{EchoTool, ExternalTool};

const USAGE: &str = "\
Usage: livefolders <command> [options]

Commands:
  init                   Create a starter livefolders.yaml in the current directory
  mount [path]           Mount the filesystem (path overrides livefolders.yaml)
  install <url>          Install a tool from GitHub
  help                   Show this help

Options:
  --config, -c <file>   Path to livefolders.yaml (default: ./livefolders.yaml)

Environment:
  GITHUB_TOKEN      GitHub personal access token (for livefolders install)
";

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("livefolders=info".parse()?),
        )
        .init();

    secrets::load_secrets_env()
        .unwrap_or_else(|e| tracing::warn!("could not load secrets.env: {}", e));

    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("help");

    match cmd {
        "init" => cmd_init(),
        "mount" => cmd_mount(&args),
        "install" => {
            let url = args.get(2).map(|s| s.as_str()).unwrap_or("");
            if url.is_empty() {
                eprintln!("Usage: livefolders install <github-url>");
                eprintln!("Example: livefolders install github.com/owner/repo");
                std::process::exit(1);
            }
            let cfg = load_config_for_install(&args);
            installer::install(url, &cfg)
        }
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
    use std::io::Write as _;

    let path = PathBuf::from("livefolders.yaml");
    if path.exists() {
        bail!("livefolders.yaml already exists in the current directory");
    }

    print!("Mount directory [.livefolders]: ");
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let mount = input.trim();
    let mount = if mount.is_empty() { ".livefolders" } else { mount };

    std::fs::write(
        &path,
        format!(
            "# LiveFolders configuration\n# Run `livefolders mount` to start the filesystem.\n\nmount: {mount}\ntools_dir: ~/.config/livefolders/tools\n\ntools:\n  - name: echo\n"
        ),
    )?;

    println!("Created livefolders.yaml");
    println!("Run `livefolders install <github-url>` to add tools, then `livefolders mount`.");
    Ok(())
}

fn load_config_for_install(args: &[String]) -> Config {
    let config_path = parse_mount_args(args).1;
    match config_path {
        Some(p) => Config::load(&p).unwrap_or_else(|_| Config::default_config()),
        None => {
            let default = PathBuf::from("livefolders.yaml");
            if default.exists() {
                Config::load(&default).unwrap_or_else(|_| Config::default_config())
            } else {
                Config::default_config()
            }
        }
    }
}

fn cmd_mount(args: &[String]) -> Result<()> {
    let (cli_mount, config_path) = parse_mount_args(args);

    let cfg = match config_path {
        Some(p) => Config::load(&p)?,
        None => {
            let default_path = PathBuf::from("livefolders.yaml");
            if default_path.exists() {
                Config::load(&default_path)?
            } else {
                Config::default_config()
            }
        }
    };

    let mountpoint = cli_mount
        .or_else(|| cfg.mount.clone())
        .unwrap_or_else(|| PathBuf::from("/tmp/livefolders"));

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
        MountOption::FSName("livefolders".to_string()),
        MountOption::AutoUnmount,
        MountOption::AllowOther,
    ];

    info!("mounting livefolders at {}", mountpoint.display());
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

    let fs = LiveFolders::new(registry, tools_dir, session, handle, cfg.timeout_secs);
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
            other => warn!("unknown built-in tool in config: {}", other),
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
            // skip "mount" and "install" subcommand tokens themselves
            "mount" | "install" => {}
            arg if !arg.starts_with('-') => {
                mount = Some(PathBuf::from(arg));
            }
            _ => {}
        }
        i += 1;
    }
    (mount, config)
}
