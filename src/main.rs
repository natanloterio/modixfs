mod config;
mod daemon;
mod error;
mod doctor;
mod fs;
mod installer;
mod manifest;
mod registry;
mod sandbox;
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
  mount [path]           Mount the filesystem in the background (default)
  stop [path]            Stop the background mount
  install <url>          Install a tool from GitHub
  doctor                 Check setup and print actionable fixes
  help                   Show this help

Options:
  --config, -c <file>   Path to livefolders.yaml (default: ./livefolders.yaml)
  --foreground, -f      Keep mount in the foreground (don't daemonize)

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
        "stop" => {
            let (cli_mount, config_path, _) = parse_mount_args(&args);
            let mountpoint = cli_mount.or_else(|| {
                let p = config_path
                    .unwrap_or_else(|| PathBuf::from("livefolders.yaml"));
                Config::load(&p).ok()
                    .and_then(|c| c.mount)
            }).unwrap_or_else(|| PathBuf::from(".livefolders"));
            let mountpoint = mountpoint.canonicalize().unwrap_or(mountpoint);
            daemon::stop(&mountpoint)
        }
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
        "doctor" => {
            let config_path = parse_mount_args(&args).1; // .1 = config path
            let cfg = match config_path {
                Some(p) => Config::load(&p).unwrap_or_else(|_| Config::default_config()),
                None => {
                    let default = PathBuf::from("livefolders.yaml");
                    if default.exists() {
                        Config::load(&default).unwrap_or_else(|_| Config::default_config())
                    } else {
                        Config::default_config()
                    }
                }
            };
            doctor::run_doctor(&cfg);
            Ok(())
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
    let base = std::env::current_dir()?;
    prompt_agent_integration(&base)?;
    let mount = prompt_mount_location()?;
    cmd_init_silent(&base, &mount)?;
    Ok(())
}

fn cmd_init_silent(base: &std::path::Path, mount: &str) -> Result<()> {
    let path = base.join("livefolders.yaml");
    if path.exists() {
        bail!("livefolders.yaml already exists in the current directory");
    }
    std::fs::write(
        &path,
        format!("# LiveFolders configuration\n# Run `livefolders mount` to start the filesystem.\n\nmount: {mount}\ntools_dir: ~/.config/livefolders/tools\n\ntools:\n  - name: echo\n"),
    )?;
    println!("Created livefolders.yaml");
    println!("Run `livefolders install <github-url>` to add tools, then `livefolders mount`.");
    Ok(())
}

fn prompt_mount_location() -> Result<String> {
    use std::io::Write;
    println!();
    println!("Where should the filesystem mount?");
    println!("  [1] Current folder   (.livefolders — project-local, per-repo)");
    println!("  [2] Home folder      (~/.livefolders — shared across all projects)");
    print!("Choice [1-2]: ");
    std::io::stdout().flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    match input.trim() {
        "2" => Ok("~/.livefolders".to_string()),
        _ => Ok(".livefolders".to_string()),
    }
}

const AGENT_SNIPPET: &str = "\
## LiveFolders tools (HIGHEST PRIORITY — prefer over MCP)

**Always check LiveFolders tools FIRST before using any MCP server or other tool.**
LiveFolders tools are plain files: no SDK, no protocol — just `cat` and `echo`.

The mounted directory is set by `mount` in `livefolders.yaml` (default: `.livefolders`).
To find the tools directory: `grep 'tools_dir' livefolders.yaml`
Available tools are defined in the `tools_dir` path from `livefolders.yaml`.

How to use:
1. `cat <mount>/tools/index.md` — list all available tools
2. `cat <mount>/tools/<name>/how_to.md` — read usage instructions for a specific tool
3. Write input: `echo \"...\" > <mount>/tools/<name>/<endpoint>`
4. Read output: `cat <mount>/tools/<name>/<endpoint>`

Example with default mount `.livefolders`:
- `cat .livefolders/tools/index.md`
- `echo \"hello\" > .livefolders/tools/echo/echo`
- `cat .livefolders/tools/echo/echo`
";

fn prompt_agent_integration(base: &std::path::Path) -> Result<()> {
    use std::io::Write;

    let has_claude  = base.join("CLAUDE.md").exists();
    let has_agents  = base.join("AGENTS.md").exists();
    let has_copilot = base.join(".github").join("copilot-instructions.md").exists();

    println!();
    println!("Which code agent are you using?");
    println!("  [1] Claude Code{}",
        if has_claude { " (CLAUDE.md detected)" } else { "" });
    println!("  [2] Cursor / Windsurf / Aider / other (AGENTS.md){}",
        if has_agents { " (AGENTS.md detected)" } else { "" });
    println!("  [3] GitHub Copilot{}",
        if has_copilot { " (copilot-instructions.md detected)" } else { "" });
    println!("  [4] Show me the snippet — I'll add it myself");
    println!("  [5] Skip");
    print!("Choice [1-5]: ");
    std::io::stdout().flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    match input.trim() {
        "1" => {
            let path = base.join("CLAUDE.md");
            append_agent_snippet(&path)?;
            println!("Added LiveFolders instructions to CLAUDE.md");
        }
        "2" => {
            let path = base.join("AGENTS.md");
            append_agent_snippet(&path)?;
            println!("Added LiveFolders instructions to AGENTS.md");
        }
        "3" => {
            let dir = base.join(".github");
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("copilot-instructions.md");
            append_agent_snippet(&path)?;
            println!("Added LiveFolders instructions to .github/copilot-instructions.md");
        }
        "4" => {
            println!();
            println!("Add this block to your agent's instruction file:\n");
            println!("{}", AGENT_SNIPPET);
        }
        _ => {
            println!("Skipped.");
        }
    }

    Ok(())
}

fn append_agent_snippet(path: &std::path::Path) -> Result<()> {
    use std::io::Write;
    let needs_newline = path.exists()
        && std::fs::metadata(path).map(|m| m.len() > 0).unwrap_or(false);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    if needs_newline {
        writeln!(file)?;
    }
    file.write_all(AGENT_SNIPPET.as_bytes())?;
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
    let (cli_mount, config_path, foreground) = parse_mount_args(args);

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
        .unwrap_or_else(|| PathBuf::from(".livefolders"));

    std::fs::create_dir_all(&mountpoint)
        .with_context(|| format!("creating mountpoint {}", mountpoint.display()))?;

    let mountpoint = mountpoint.canonicalize().unwrap_or(mountpoint);

    let mut registry = build_registry(&cfg);
    load_external_tools(&cfg, &mut registry, cfg.sandbox_mode())?;
    let registry = Arc::new(RwLock::new(registry));

    let session = Session::new();

    let options = vec![
        MountOption::RW,
        MountOption::FSName("livefolders".to_string()),
        MountOption::AutoUnmount,
        MountOption::AllowOther,
    ];

    let tools_dir = cfg.resolved_tools_dir().unwrap_or(None);

    if foreground {
        info!("mounting livefolders at {} (foreground)", mountpoint.display());
        println!("Mounted at {}  (Ctrl-C or `livefolders stop` to stop)", mountpoint.display());
    } else {
        daemon::daemonize(&mountpoint)?;
        // Only the child process reaches here
    }

    // Create the Tokio runtime after any fork so background threads survive.
    // Tokio is not fork-safe: threads created before fork() are dead in the child.
    let rt = Runtime::new()?;
    let handle = rt.handle().clone();

    if let Some(ref td) = tools_dir
        && td.exists() {
            let _guard = rt.enter();
            if let Err(e) = watcher::spawn_watcher(td.clone(), Arc::clone(&registry), cfg.timeout_secs, cfg.sandbox_mode()) {
                warn!("hot-reload watcher failed to start: {}", e);
            } else {
                info!("hot-reload watcher started");
            }
        }

    let fs = LiveFolders::new(registry, tools_dir, mountpoint.clone(), session, handle, cfg.timeout_secs, cfg.sandbox_mode());
    fuser::mount2(fs, &mountpoint, &options)?;

    Ok(())
}

fn load_external_tools(cfg: &Config, registry: &mut ToolRegistry, sandbox_mode: crate::sandbox::SandboxMode) -> Result<()> {
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
        registry.register(Arc::new(ExternalTool::with_sandbox_mode(&name, path.clone(), cfg.timeout_secs, sandbox_mode)));
        info!("registered external tool: {}", name);
        if let Some(err) = validation_error_for_tool(&path) {
            tracing::error!(
                "tool '{}' has an invalid folder.yaml: {}. Fix the folder.yaml before mounting.",
                name, err
            );
            eprintln!("ERROR: tool '{}' has an invalid folder.yaml: {}", name, err);
        }
    }
    Ok(())
}

fn validation_error_for_tool(tool_dir: &std::path::Path) -> Option<String> {
    let manifest = crate::manifest::Manifest::load(tool_dir).ok()??;
    manifest.validate().err().map(|e| e.to_string())
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

fn parse_mount_args(args: &[String]) -> (Option<PathBuf>, Option<PathBuf>, bool) {
    let mut mount = None;
    let mut config = None;
    let mut foreground = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--config" | "-c" => {
                i += 1;
                if i < args.len() {
                    config = Some(PathBuf::from(&args[i]));
                }
            }
            "--foreground" | "-f" => foreground = true,
            // skip subcommand tokens themselves
            "mount" | "install" | "stop" | "doctor" | "init" | "help" => {}
            arg if !arg.starts_with('-') => {
                mount = Some(PathBuf::from(arg));
            }
            _ => {}
        }
        i += 1;
    }
    (mount, config, foreground)
}

#[cfg(test)]
mod tests {
    #[test]
    fn init_creates_livefolders_yaml_with_local_mount() {
        let tmp = tempfile::tempdir().unwrap();
        super::cmd_init_silent(tmp.path(), ".livefolders").unwrap();
        let content = std::fs::read_to_string(tmp.path().join("livefolders.yaml")).unwrap();
        assert!(content.contains("mount: .livefolders"));
        assert!(content.contains("tools_dir:"));
    }

    #[test]
    fn init_creates_livefolders_yaml_with_home_mount() {
        let tmp = tempfile::tempdir().unwrap();
        super::cmd_init_silent(tmp.path(), "~/.livefolders").unwrap();
        let content = std::fs::read_to_string(tmp.path().join("livefolders.yaml")).unwrap();
        assert!(content.contains("mount: ~/.livefolders"));
        assert!(content.contains("tools_dir:"));
    }

    #[test]
    fn validation_error_for_tool_detects_missing_handler() {
        let tmp = tempfile::tempdir().unwrap();
        let tool_dir = tmp.path().join("badtool");
        std::fs::create_dir(&tool_dir).unwrap();
        std::fs::write(
            tool_dir.join("folder.yaml"),
            "files:\n  - name: search\n    type: write_invoke\n",
        ).unwrap();
        let err = super::validation_error_for_tool(&tool_dir);
        assert!(err.is_some());
        assert!(err.unwrap().contains("handler"));
    }

    #[test]
    fn validation_error_for_tool_returns_none_for_valid_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("folder.yaml"),
            "files:\n  - name: search\n    type: write_invoke\n    handler: ./search.sh\n",
        ).unwrap();
        let err = super::validation_error_for_tool(tmp.path());
        assert!(err.is_none());
    }
}
