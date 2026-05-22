mod config;
mod daemon;
mod error;
mod doctor;
mod mcp;
mod mcp_proxy;
mod fs;
mod installer;
mod manifest;
mod registry;
mod sandbox;
mod secrets;
mod tools;
mod marketplace;
mod watcher;

use std::path::{Path, PathBuf};
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
  tools list             List all installed tools
  search <query>         Search the LiveFolders tool registry
  info <owner/name>      Show tool info from the registry
  publish                Publish this repo to the registry
  doctor                 Check setup and print actionable fixes
  mcp proxy [--background]          Start the MCP bridge proxy daemon
  mcp call <server> <tool> [--arg key=value ...]  Call a tool via proxy
  mcp status                         Show proxy status
  mcp stop                           Stop the proxy daemon
  mcp register [config]              Register as MCP server in ~/.claude.json
  mcp [mount]                        Expose tools as MCP server over stdio
  help                   Show this help
  --version, -v          Print version

Options:
  --config, -c <file>   Path to livefolders.yaml (default: ./livefolders.yaml)
  --foreground, -f      Keep mount in the foreground (don't daemonize)

Environment:
  GITHUB_TOKEN      GitHub personal access token (for livefolders install)
";

fn list_installed_tools(tools_dir: &std::path::Path) -> anyhow::Result<Vec<String>> {
    let mut lines = Vec::new();
    for entry in std::fs::read_dir(tools_dir)
        .with_context(|| format!("reading tools dir {}", tools_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() || !path.join("folder.yaml").exists() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let description = manifest::Manifest::load(&path)
            .ok()
            .flatten()
            .and_then(|m| m.description)
            .unwrap_or_else(|| "—".to_string());
        lines.push(format!("{:<20} — {}", name, description));
    }
    Ok(lines)
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("livefolders=info".parse()?),
        )
        .with_writer(std::io::stderr)
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
            let arg = args.get(2).map(|s| s.as_str()).unwrap_or("");
            if arg.is_empty() {
                eprintln!("Usage: livefolders install <owner/name[@version] | github-url>");
                std::process::exit(1);
            }
            let cfg = load_config_for_install(&args);
            // Detect registry format: owner/name or owner/name@version (no github.com prefix)
            if !arg.contains("github.com") && !arg.starts_with("http") {
                let (slug, version) = if let Some((s, v)) = arg.split_once('@') {
                    (s, Some(v))
                } else {
                    (arg, None)
                };
                let (owner, name) = slug.split_once('/').ok_or_else(|| {
                    anyhow::anyhow!("[ERROR:INVALID] expected owner/name or owner/name@version")
                })?;
                let resolved = marketplace::resolve::resolve(owner, name, version)
                    .context("registry lookup failed")?;
                println!("Resolved {}/{}@{}", owner, name, resolved.version);
                installer::install_from_tarball_url(&resolved.tarball_url, name, &cfg)?;
                // Increment download counter (fire and forget)
                let _ = reqwest::blocking::Client::builder()
                    .user_agent("livefolders")
                    .build()
                    .and_then(|c| {
                        c.post(format!(
                            "{}/api/tools/{}/{}/downloads",
                            marketplace::REGISTRY_URL, owner, name
                        ))
                        .send()
                    });
                Ok(())
            } else {
                installer::install(arg, &cfg)
            }
        }
        "search" => {
            let query = args.get(2).map(|s| s.as_str()).unwrap_or("");
            if query.is_empty() {
                eprintln!("Usage: livefolders search <query>");
                std::process::exit(1);
            }
            match marketplace::search::search(query) {
                Ok(results) if results.is_empty() => {
                    println!("No tools found for \"{}\".", query);
                }
                Ok(results) => {
                    for t in results {
                        println!(
                            "{}/{}\t— {}\t({} installs)",
                            t.owner,
                            t.name,
                            t.description.as_deref().unwrap_or("no description"),
                            t.downloads
                        );
                    }
                }
                Err(e) => {
                    eprintln!("[ERROR:REGISTRY] {}", e);
                    std::process::exit(1);
                }
            }
            Ok(())
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
        "info" => {
            let pkg = args.get(2).map(|s| s.as_str()).unwrap_or("");
            if pkg.is_empty() {
                eprintln!("Usage: livefolders info <owner/name>");
                std::process::exit(1);
            }
            let (owner, name) = pkg.split_once('/').ok_or_else(|| {
                anyhow::anyhow!("[ERROR:INVALID] expected owner/name")
            })?;
            match marketplace::info::get_info(owner, name) {
                Ok(info) => {
                    println!("{}/{}", info.owner, info.name);
                    println!("  Description: {}", info.description.as_deref().unwrap_or("—"));
                    println!("  Repository:  {}", info.repo_url);
                    println!("  Downloads:   {}", info.downloads);
                    println!("  Updated:     {}", info.updated_at);
                    println!("  Install:     livefolders install {}/{}", owner, name);
                }
                Err(e) => {
                    eprintln!("[ERROR:REGISTRY] {}", e);
                    std::process::exit(1);
                }
            }
            Ok(())
        }
        "publish" => {
            let repo_arg = args.get(2).map(|s| s.as_str());
            marketplace::publish::publish(repo_arg)?;
            Ok(())
        }
        "tools" => {
            let sub = args.get(2).map(|s| s.as_str()).unwrap_or("help");
            match sub {
                "list" => {
                    let config_path = parse_mount_args(&args).1;
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
                    let tools_dir = cfg.resolved_tools_dir()
                        .context("could not resolve tools_dir")?;
                    let Some(tools_dir) = tools_dir else {
                        println!("No tools installed.");
                        return Ok(());
                    };
                    if !tools_dir.exists() {
                        println!("No tools installed.");
                        return Ok(());
                    }
                    let mut lines = list_installed_tools(&tools_dir)?;
                    if lines.is_empty() {
                        println!("No tools installed.");
                    } else {
                        lines.sort();
                        for line in lines {
                            println!("{}", line);
                        }
                    }
                    Ok(())
                }
                _ => {
                    eprintln!("Usage: livefolders tools list");
                    std::process::exit(1);
                }
            }
        }
        "mcp" => cmd_mcp(&args),
        "--version" | "-v" => {
            println!("{}", env!("CARGO_PKG_VERSION"));
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
    let name = prompt_project_name()?;
    prompt_agent_integration(&base, &name)?;
    let mount = prompt_mount_location()?;
    cmd_init_silent(&base, &name, &mount)?;
    Ok(())
}

fn cmd_init_silent(base: &std::path::Path, name: &str, mount: &str) -> Result<()> {
    let path = base.join("livefolders.yaml");
    if path.exists() {
        bail!("livefolders.yaml already exists in the current directory");
    }
    std::fs::write(
        &path,
        format!("# LiveFolders configuration\n# Run `livefolders mount` to start the filesystem.\n\nname: {name}\nmount: {mount}\ntools_dir: ~/.config/livefolders/tools\n\ntools:\n  - name: echo\n"),
    )?;
    println!("Created livefolders.yaml");
    println!("Run `livefolders install <github-url>` to add tools, then `livefolders mount`.");
    Ok(())
}

fn prompt_project_name() -> Result<String> {
    use std::io::Write;
    let default = std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "myproject".to_string());
    print!("Project name [{}]: ", default);
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let name = input.trim().to_string();
    Ok(if name.is_empty() { default } else { name })
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

fn prompt_agent_integration(base: &std::path::Path, project_name: &str) -> Result<()> {
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
            let home = std::env::var("HOME").unwrap_or_default();
            let claude_json = PathBuf::from(&home).join(".claude.json");
            let server_name = format!("livefolders-{}", sanitize_mcp_name(project_name));
            let config_path = base.join("livefolders.yaml");
            let abs_config = config_path.canonicalize().unwrap_or(config_path);
            let bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("livefolders"));
            match register_claude_mcp(&claude_json, &server_name, &bin, Some(&abs_config)) {
                Ok(()) => println!("Registered MCP server '{}' in {}", server_name, claude_json.display()),
                Err(e) => println!("Could not register MCP server ({}). Add manually — see `livefolders mcp register livefolders.yaml`.", e),
            }
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

struct McpCallArgs {
    server: Option<String>,
    tool: Option<String>,
    arg_pairs: std::collections::HashMap<String, String>,
}

fn parse_mcp_call_args(args: &[String]) -> McpCallArgs {
    // args layout: ["livefolders", "mcp", "call", <server>, <tool>, "--arg", "k=v", ...]
    let server = args.get(3).cloned();
    let tool = args.get(4).cloned();
    let mut arg_pairs = std::collections::HashMap::new();
    let mut i = 5;
    while i < args.len() {
        if args[i] == "--arg" {
            i += 1;
            if i < args.len() {
                if let Some((k, v)) = args[i].split_once('=') {
                    arg_pairs.insert(k.to_string(), v.to_string());
                }
            }
        }
        i += 1;
    }
    McpCallArgs { server, tool, arg_pairs }
}

fn cmd_mcp(args: &[String]) -> Result<()> {
    let sub = args.get(2).map(|s| s.as_str()).unwrap_or("");
    match sub {
        "proxy" => {
            let socket_path = mcp_proxy::default_socket_path();
            let config_path = mcp_proxy::default_config_path();
            if args.iter().any(|a| a == "--background") {
                std::process::Command::new(std::env::current_exe()?)
                    .args(["mcp", "proxy"])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()?;
                println!("mcp-proxy started in background");
                return Ok(());
            }
            mcp_proxy::run_proxy(socket_path, config_path)
        }
        "call" => {
            let parsed = parse_mcp_call_args(args);
            let server = parsed.server.ok_or_else(|| {
                anyhow::anyhow!("Usage: livefolders mcp call <server> <tool> [--arg key=value ...]")
            })?;
            let tool = parsed.tool.ok_or_else(|| {
                anyhow::anyhow!("Usage: livefolders mcp call <server> <tool> [--arg key=value ...]")
            })?;
            let args_json: serde_json::Value = parsed.arg_pairs
                .into_iter()
                .map(|(k, v)| (k, serde_json::Value::String(v)))
                .collect::<serde_json::Map<_, _>>()
                .into();
            let socket_path = mcp_proxy::default_socket_path();
            let req = mcp_proxy::ProxyRequest::Call { server, tool, args: args_json };
            let resp = mcp_proxy::proxy_call(&socket_path, &req)?;
            if resp.ok {
                print!("{}", resp.result.unwrap_or_default());
            } else {
                eprintln!("[ERROR:HANDLER] {}", resp.error.unwrap_or_default());
                std::process::exit(1);
            }
            Ok(())
        }
        "status" => {
            let socket_path = mcp_proxy::default_socket_path();
            let req = mcp_proxy::ProxyRequest::Status;
            let resp = mcp_proxy::proxy_call(&socket_path, &req)?;
            println!("mcp-proxy socket: {}", socket_path.display());
            if resp.ok {
                let servers = resp.result.unwrap_or_default();
                if servers.is_empty() {
                    println!("No MCP servers running.");
                } else {
                    println!("Running servers:\n{}", servers);
                }
            }
            Ok(())
        }
        "stop" => {
            let socket_path = mcp_proxy::default_socket_path();
            let req = mcp_proxy::ProxyRequest::Stop;
            let resp = mcp_proxy::proxy_call(&socket_path, &req)?;
            if resp.ok {
                println!("mcp-proxy stopped.");
            }
            Ok(())
        }
        "register" => {
            cmd_mcp_register(args)
        }
        _ => {
            cmd_mcp_serve(args)
        }
    }
}

fn cmd_mcp_register(args: &[String]) -> Result<()> {
    let home = std::env::var("HOME").context("$HOME is not set")?;
    let claude_json = PathBuf::from(&home).join(".claude.json");

    let config_arg = args.get(3).map(PathBuf::from);
    let (server_name, abs_config) = if let Some(ref cp) = config_arg {
        let cfg = Config::load(cp)
            .with_context(|| format!("reading config {}", cp.display()))?;
        let abs = cp.canonicalize()
            .with_context(|| format!("resolving path {}", cp.display()))?;
        let raw_name = cfg.name.unwrap_or_else(|| {
            abs.parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "livefolders".to_string())
        });
        (format!("livefolders-{}", sanitize_mcp_name(&raw_name)), Some(abs))
    } else {
        ("livefolders".to_string(), None)
    };

    let bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("livefolders"));
    register_claude_mcp(&claude_json, &server_name, &bin, abs_config.as_deref())?;
    println!("Registered MCP server '{}' in {}", server_name, claude_json.display());
    Ok(())
}

fn cmd_mcp_serve(args: &[String]) -> Result<()> {
    let (cli_mount, config_path, _) = parse_mount_args(args);

    let cfg_base: Option<PathBuf>;
    let cfg = match config_path {
        Some(ref p) => {
            cfg_base = p.canonicalize().ok().and_then(|a| a.parent().map(|d| d.to_path_buf()));
            Config::load(p)?
        }
        None => {
            let default_path = PathBuf::from("livefolders.yaml");
            if default_path.exists() {
                cfg_base = std::env::current_dir().ok();
                Config::load(&default_path)?
            } else {
                cfg_base = None;
                Config::default_config()
            }
        }
    };

    let mount = cli_mount.or_else(|| cfg.mount.clone()).unwrap_or_else(|| PathBuf::from(".livefolders"));

    // Resolve relative mount paths against the config file's directory so that
    // `livefolders mcp --config /abs/path/livefolders.yaml` works regardless of CWD.
    let mount = if mount.is_relative() {
        cfg_base.as_deref().map(|base| base.join(&mount)).unwrap_or(mount)
    } else {
        mount
    };

    mcp::run(mount)
}

/// Replace characters not allowed in Claude Code MCP server names with hyphens.
fn sanitize_mcp_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect()
}

/// Merge an `mcpServers` entry into ~/.claude.json (the Claude Code MCP registry).
///
/// Uses the format `claude mcp add --scope user` would produce:
/// `{ "type": "stdio", "command": "<bin>", "args": [...], "env": {} }`
fn register_claude_mcp(
    claude_json: &Path,
    server_name: &str,
    bin: &Path,
    config_path: Option<&Path>,
) -> Result<()> {
    let mut root: serde_json::Value = if claude_json.exists() {
        let content = std::fs::read_to_string(claude_json)
            .with_context(|| format!("reading {}", claude_json.display()))?;
        serde_json::from_str(&content).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    let mcp_args = if let Some(cp) = config_path {
        serde_json::json!(["mcp", "--config", cp.to_string_lossy()])
    } else {
        serde_json::json!(["mcp"])
    };

    root["mcpServers"][server_name] = serde_json::json!({
        "type": "stdio",
        "command": bin.to_string_lossy(),
        "args": mcp_args,
        "env": {}
    });

    if let Some(parent) = claude_json.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let serialized = serde_json::to_string_pretty(&root)?;
    std::fs::write(claude_json, serialized)
        .with_context(|| format!("writing {}", claude_json.display()))?;
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
            _ => {}
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
            "mount" | "install" | "stop" | "doctor" | "init" | "help" | "mcp" | "tools" | "list" => {}
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
mod tools_list_tests {
    use super::*;
    use std::fs;

    fn make_tool(tools_dir: &std::path::Path, name: &str, description: Option<&str>) {
        let dir = tools_dir.join(name);
        fs::create_dir_all(&dir).unwrap();
        let desc_line = description
            .map(|d| format!("description: {}\n", d))
            .unwrap_or_default();
        fs::write(dir.join("folder.yaml"), format!("{}files: []\n", desc_line)).unwrap();
    }

    #[test]
    fn list_tools_returns_name_and_description() {
        let tmp = tempfile::tempdir().unwrap();
        make_tool(tmp.path(), "weather", Some("Get the weather"));
        make_tool(tmp.path(), "echo", Some("Echo input back"));
        let mut out = list_installed_tools(tmp.path()).unwrap();
        out.sort();
        assert_eq!(out.len(), 2);
        assert!(out[0].contains("echo") && out[0].contains("Echo input back"));
        assert!(out[1].contains("weather") && out[1].contains("Get the weather"));
    }

    #[test]
    fn list_tools_skips_dirs_without_folder_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        make_tool(tmp.path(), "real-tool", Some("A real tool"));
        fs::create_dir_all(tmp.path().join("not-a-tool")).unwrap();
        let out = list_installed_tools(tmp.path()).unwrap();
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("real-tool"));
    }

    #[test]
    fn list_tools_no_description_shows_dash() {
        let tmp = tempfile::tempdir().unwrap();
        make_tool(tmp.path(), "minimal", None);
        let out = list_installed_tools(tmp.path()).unwrap();
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("minimal") && out[0].contains("—"));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn init_creates_livefolders_yaml_with_local_mount() {
        let tmp = tempfile::tempdir().unwrap();
        super::cmd_init_silent(tmp.path(), "myproject", ".livefolders").unwrap();
        let content = std::fs::read_to_string(tmp.path().join("livefolders.yaml")).unwrap();
        assert!(content.contains("name: myproject"));
        assert!(content.contains("mount: .livefolders"));
        assert!(content.contains("tools_dir:"));
    }

    #[test]
    fn init_creates_livefolders_yaml_with_home_mount() {
        let tmp = tempfile::tempdir().unwrap();
        super::cmd_init_silent(tmp.path(), "myproject", "~/.livefolders").unwrap();
        let content = std::fs::read_to_string(tmp.path().join("livefolders.yaml")).unwrap();
        assert!(content.contains("name: myproject"));
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

    use std::path::PathBuf;

    fn bin() -> PathBuf { PathBuf::from("/usr/local/bin/livefolders") }

    #[test]
    fn register_claude_mcp_creates_entry_with_correct_format() {
        let tmp = tempfile::tempdir().unwrap();
        let claude_json = tmp.path().join(".claude.json");
        super::register_claude_mcp(&claude_json, "livefolders", &bin(), None).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
        let entry = &v["mcpServers"]["livefolders"];
        assert_eq!(entry["type"], "stdio");
        assert_eq!(entry["command"], "/usr/local/bin/livefolders");
        assert_eq!(entry["args"][0], "mcp");
        assert!(entry["env"].is_object());
    }

    #[test]
    fn register_claude_mcp_with_config_path_embeds_config_arg() {
        let tmp = tempfile::tempdir().unwrap();
        let claude_json = tmp.path().join(".claude.json");
        let config = tmp.path().join("livefolders.yaml");
        std::fs::write(&config, "").unwrap();
        super::register_claude_mcp(&claude_json, "livefolders-myproject", &bin(), Some(&config)).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
        let entry = &v["mcpServers"]["livefolders-myproject"];
        assert_eq!(entry["type"], "stdio");
        assert_eq!(entry["args"][0], "mcp");
        assert_eq!(entry["args"][1], "--config");
        assert!(entry["args"][2].as_str().unwrap().ends_with("livefolders.yaml"));
    }

    #[test]
    fn register_claude_mcp_preserves_existing_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let claude_json = tmp.path().join(".claude.json");
        std::fs::write(&claude_json, r#"{"oauthToken":"tok","someOtherKey":true}"#).unwrap();
        super::register_claude_mcp(&claude_json, "livefolders", &bin(), None).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
        assert_eq!(v["oauthToken"], "tok");
        assert_eq!(v["mcpServers"]["livefolders"]["type"], "stdio");
    }

    #[test]
    fn register_claude_mcp_multiple_projects_coexist() {
        let tmp = tempfile::tempdir().unwrap();
        let claude_json = tmp.path().join(".claude.json");
        super::register_claude_mcp(&claude_json, "livefolders-a", &bin(), None).unwrap();
        super::register_claude_mcp(&claude_json, "livefolders-b", &bin(), None).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["livefolders-a"]["type"], "stdio");
        assert_eq!(v["mcpServers"]["livefolders-b"]["type"], "stdio");
    }

    #[test]
    fn register_claude_mcp_overwrites_existing_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let claude_json = tmp.path().join(".claude.json");
        std::fs::write(&claude_json, r#"{"mcpServers":{"livefolders":{"type":"stdio","command":"old"}}}"#).unwrap();
        super::register_claude_mcp(&claude_json, "livefolders", &bin(), None).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["livefolders"]["command"], "/usr/local/bin/livefolders");
    }

    #[test]
    fn register_claude_mcp_tolerates_malformed_json() {
        let tmp = tempfile::tempdir().unwrap();
        let claude_json = tmp.path().join(".claude.json");
        std::fs::write(&claude_json, b"not json at all").unwrap();
        super::register_claude_mcp(&claude_json, "livefolders", &bin(), None).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["livefolders"]["type"], "stdio");
    }

    #[test]
    fn sanitize_mcp_name_replaces_dots_and_spaces() {
        assert_eq!(super::sanitize_mcp_name("pitaia.me"), "pitaia-me");
        assert_eq!(super::sanitize_mcp_name("my project"), "my-project");
        assert_eq!(super::sanitize_mcp_name("valid-name_123"), "valid-name_123");
    }

    #[test]
    fn parse_mcp_call_args_basic() {
        let args: Vec<String> = ["livefolders", "mcp", "call", "brave", "web_search",
            "--arg", "query=London", "--arg", "count=5"]
            .iter().map(|s| s.to_string()).collect();
        let parsed = super::parse_mcp_call_args(&args);
        assert_eq!(parsed.server.as_deref(), Some("brave"));
        assert_eq!(parsed.tool.as_deref(), Some("web_search"));
        assert_eq!(parsed.arg_pairs.get("query").map(|s| s.as_str()), Some("London"));
        assert_eq!(parsed.arg_pairs.get("count").map(|s| s.as_str()), Some("5"));
    }

    #[test]
    fn parse_mcp_call_args_no_args() {
        let args: Vec<String> = ["livefolders", "mcp", "call", "myserver", "mytool"]
            .iter().map(|s| s.to_string()).collect();
        let parsed = super::parse_mcp_call_args(&args);
        assert_eq!(parsed.server.as_deref(), Some("myserver"));
        assert_eq!(parsed.tool.as_deref(), Some("mytool"));
        assert!(parsed.arg_pairs.is_empty());
    }

    #[test]
    fn parse_mcp_call_args_value_with_equals() {
        // --arg filter=a=b should parse as key="filter", value="a=b"
        let args: Vec<String> = ["livefolders", "mcp", "call", "s", "t",
            "--arg", "filter=a=b"]
            .iter().map(|s| s.to_string()).collect();
        let parsed = super::parse_mcp_call_args(&args);
        assert_eq!(parsed.arg_pairs.get("filter").map(|s| s.as_str()), Some("a=b"));
    }
}
