# Tool Manifest & Install Design

## Goal

Give tool builders a standard way to declare metadata and required env vars (`modix.yaml`), and give users a one-command install experience (`modixfs install <url>`) that downloads a tool, prompts for secrets, and stores them for automatic use at mount time.

## Target user

Tool builders — developers who want to publish ModixFS-compatible tools that others can install without manual copying or guesswork about required configuration.

## Architecture

Three pieces, all additive — no changes to the FUSE layer or tool registry:

1. **`modix.yaml` manifest** — optional file inside each tool directory, declaring name, description, version, and env var requirements
2. **`modixfs install <url>`** — CLI command that downloads from GitHub, reads the manifest, prompts for missing secrets, stores them, and copies the tool into `tools_dir`
3. **secrets.env auto-load** — at `modixfs mount`, load `~/.config/modixfs/secrets.env` into the process environment before starting FUSE

---

## Component 1: `modix.yaml` manifest

Location: inside each tool directory alongside `how_to.md`.

```yaml
name: mytool
description: Fetches and summarizes articles from the web
version: 0.1.0
env:
  - name: MYTOOL_API_KEY
    description: API key from https://example.com/settings
    required: true
  - name: MYTOOL_TIMEOUT
    description: Request timeout in seconds
    required: false
    default: "30"
```

Fields:
- `name` — tool directory name; used as the install target dir
- `description` — shown during install and in `index.md`
- `version` — informational only, not enforced
- `env` — list of env var declarations; `required: true` triggers a prompt at install time if the var is not already set

The manifest is optional. Tools without one install without prompts and work exactly as today.

**Implementation:** `src/manifest.rs` — `Manifest` struct deriving `serde::Deserialize`, `EnvDecl` struct with `name`, `description`, `required`, `default`.

---

## Component 2: `modixfs install <url>`

### URL formats accepted

```
github.com/owner/repo
github.com/owner/repo/tree/BRANCH/subdir
```

### Download strategy

Use the GitHub tarball API (no `git` required):

```
GET https://api.github.com/repos/{owner}/{repo}/tarball/{ref}
```

For subdirectory installs, download the full tarball and extract only the subdirectory. Uses `reqwest` (already a dependency) + `flate2` + `tar` crates for decompression.

### Install flow

1. Parse URL → extract owner, repo, optional ref and subdir
2. Download tarball via `reqwest` with `Accept: application/vnd.github+json` and optional `GITHUB_TOKEN` for rate-limit headroom
3. Extract into a temp dir; navigate to subdir if specified
4. Read and parse `modix.yaml` if present (warn and continue if absent)
5. For each `required: true` env var: check `~/.config/modixfs/secrets.env` and `std::env::var`; if missing, prompt interactively: `MYTOOL_API_KEY (API key from https://example.com/settings): `
6. Append newly entered vars to `~/.config/modixfs/secrets.env` (create with `chmod 600` if absent)
7. Copy extracted tool dir into `tools_dir` (from `tools.yaml`); if a tool with the same name exists, prompt to overwrite
8. Print: `Installed mytool → ~/.config/modixfs/tools/mytool/`

### Error cases

| Condition | Behavior |
|---|---|
| Bad URL format | Error with usage hint |
| Network failure | Error with underlying message |
| `tools_dir` not configured in tools.yaml | Error: instruct user to add `tools_dir:` |
| Name collision, user declines overwrite | Abort cleanly |
| `GITHUB_TOKEN` not set | Warn about rate limits, continue |

**Implementation:** `src/installer.rs` — `install(url: &str, cfg: &Config) -> Result<()>`; uses `tempfile` crate (already in dev-dependencies, move to regular deps).

---

## Component 3: secrets.env auto-load at mount

File: `~/.config/modixfs/secrets.env` — standard dotenv format, one `KEY=VALUE` per line, `#` comments ignored.

At `modixfs mount`, before building the registry:

```rust
load_secrets_env()?;  // reads ~/.config/modixfs/secrets.env, calls std::env::set_var for each entry not already set
```

Rules:
- Shell environment always wins — existing env vars are never overwritten
- File is optional — absence is not an error
- Vars are set on the `modixfs` process, so all subprocess tools inherit them automatically
- Built-in tools that read env vars at startup (e.g. `github` reading `GITHUB_TOKEN`) also benefit

**Implementation:** `src/secrets.rs` — `load_secrets_env() -> Result<()>`; parse line-by-line, skip comments and blank lines, call `std::env::set_var` for missing vars only.

---

## New dependencies

| Crate | Use |
|---|---|
| `flate2` | gzip decompression of GitHub tarballs |
| `tar` | tar extraction |
| `tempfile` | move from dev-deps to regular deps (already present) |

`reqwest` and `serde_yaml` are already present.

---

## Files touched

| File | Change |
|---|---|
| `src/manifest.rs` | New — `Manifest` and `EnvDecl` structs, parse/load |
| `src/installer.rs` | New — download, extract, prompt, copy |
| `src/secrets.rs` | New — load secrets.env into process env |
| `src/main.rs` | Add `install` command, call `load_secrets_env()` in `cmd_mount` |
| `Cargo.toml` | Add `flate2`, `tar`; move `tempfile` to regular deps |
| `README.md` | Document `modix.yaml` format and `modixfs install` |

---

## Out of scope

- Secret encryption at rest
- A hosted tool registry / index
- Version pinning or lock files
- Windows support
