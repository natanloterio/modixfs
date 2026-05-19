pub const ROOT_CREATE_TOOL: &str = "\
# Creating a LiveFolders tool

A tool is a directory under `tools/` that contains a `folder.yaml` manifest and one or more
handler scripts. The filesystem hot-reloads tools automatically — no restart needed.

## Quick start

```
mkdir tools/mytool
cat > tools/mytool/folder.yaml << 'EOF'
name: mytool
description: Echoes whatever you write.
files:
  - name: echo
    type: write_invoke
    handler: cat
EOF
# Test it:
echo \"hello\" > tools/mytool/echo && cat tools/mytool/echo
```

## folder.yaml schema

```yaml
name: <string>            # tool name shown in index.md
description: <string>     # one-line description shown in index.md
version: <string>         # optional semver, informational only
env:                      # secrets the tool needs (see below)
  - ...
files:                    # endpoint declarations (required)
  - ...
```

## File types

Each entry under `files` declares one endpoint (file) inside the tool directory.

### write_invoke

Write to the file → handler runs with your input on stdin → read the file for output.

```yaml
files:
  - name: search
    type: write_invoke
    handler: ./search.sh   # shell command; receives input on stdin
```

Usage:
```
echo \"rust fuse\" > tools/mytool/search
cat tools/mytool/search
```

### read_invoke

Reading the file triggers the handler (no input needed). Result is returned on read.

```yaml
files:
  - name: status
    type: read_invoke
    handler: date
```

Usage:
```
cat tools/mytool/status
```

### passthrough

Plain file on disk. Reads and writes go directly to the filesystem — no handler.
Useful for config files the handler reads.

```yaml
files:
  - name: config.json
    type: passthrough
```

### readonly

Read-only file on disk. Writes are rejected.

```yaml
files:
  - name: README.md
    type: readonly
```

## Input validation

Attach an `input` block to any `write_invoke` or `read_invoke` endpoint.
Invalid input is rejected before the handler runs.

### Plain text with constraints

```yaml
files:
  - name: greet
    type: write_invoke
    handler: cat
    input:
      type: string
      min_length: 1      # optional
      max_length: 200    # optional
      pattern: \"^[a-z ]+$\"  # optional regex; must match entire input
```

### JSON with schema

```yaml
files:
  - name: query
    type: write_invoke
    handler: ./search.sh
    input:
      type: json
      schema:
        required: [q]
        properties:
          q:
            type: string
          limit:
            type: number
```

### No payload

```yaml
files:
  - name: ping
    type: read_invoke
    handler: echo pong
    input:
      type: none
```

## Secrets (env declarations)

Declare secrets users must supply at `livefolders install` time.
They are injected as environment variables when handlers run.

```yaml
env:
  - name: GITHUB_TOKEN
    description: Personal access token with repo scope
    required: true
  - name: TIMEOUT
    description: Request timeout in seconds
    required: false
    default: \"30\"
```

## Stateful tools

Use `state_file` to serialize concurrent calls to the same endpoint.
The runtime holds an exclusive advisory lock on the file for the entire handler invocation
and passes its resolved path as `LIVEFOLDERS_STATE_FILE`.

```yaml
files:
  - name: counter
    type: write_invoke
    handler: ./increment.sh
    state_file: counter.json   # path relative to the tool directory
```

Inside `increment.sh`:
```bash
#!/bin/bash
state=\"$LIVEFOLDERS_STATE_FILE\"
count=$(jq '.count // 0' \"$state\" 2>/dev/null || echo 0)
echo \"{\\\"count\\\": $((count + 1))}\" | tee \"$state\"
```

## Pipelines

Chain endpoints: stdout of each stage becomes stdin of the next.
When `pipe` is set, `handler` must be absent.

```yaml
files:
  - name: fetch
    type: write_invoke
    handler: curl -s
  - name: extract
    type: write_invoke
    handler: jq '.title'
  - name: fetch_title
    type: write_invoke
    pipe: [fetch, extract]
```

## Environment variables available to handlers

| Variable | Value |
|----------|-------|
| `LIVEFOLDERS_STATE_FILE` | Absolute path to the locked state file (only when `state_file` is set) |
| Any declared `env` secret | Value supplied by the user at install time |
| Standard shell env | PATH, HOME, etc. |

## Companion .log file

After the first invocation, a `<endpoint>.log` file appears alongside every
`write_invoke` / `read_invoke` endpoint. It contains:

```
duration_ms: 42
exit: 0
stderr: (empty or error text)
```

Read it to diagnose failures: `cat tools/mytool/search.log`

## Error format

Handler errors are returned as: `[ERROR:CODE] message`

| Code | Meaning |
|------|---------|
| `INVALID_INPUT` | Input failed schema validation |
| `HANDLER` | Handler exited non-zero |
| `TIMEOUT` | Handler exceeded the configured timeout |
| `SPAWN` | Could not start the handler process |
| `PROCESS` | OS-level process error |

## Full example — GitHub search tool

```
mkdir tools/github
cat > tools/github/folder.yaml << 'EOF'
name: github
description: Search GitHub repositories and code.
env:
  - name: GITHUB_TOKEN
    description: Personal access token
    required: true
files:
  - name: search_repos
    type: write_invoke
    handler: ./search_repos.sh
    input:
      type: string
      min_length: 1
EOF

cat > tools/github/search_repos.sh << 'SCRIPT'
#!/bin/bash
query=$(cat -)
curl -s -H \"Authorization: Bearer $GITHUB_TOKEN\" \\
  \"https://api.github.com/search/repositories?q=$(python3 -c \"import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1]))\" \"$query\")\" \\
  | jq '[.items[] | {name: .full_name, stars: .stargazers_count, url: .html_url}]'
SCRIPT
chmod +x tools/github/search_repos.sh

# Test immediately:
echo \"language:rust fuse stars:>50\" > tools/github/search_repos && cat tools/github/search_repos
```

## Verifying your tool

```
cat tools/index.md                    # confirm tool appears
cat tools/mytool/how_to.md            # inspect generated usage doc
cat tools/mytool/schema.json          # inspect generated JSON schema
echo \"test\" > tools/mytool/<ep> && cat tools/mytool/<ep>   # smoke test
cat tools/mytool/<ep>.log             # check timing and stderr on failure
```
";

pub const ROOT_HOW_TO: &str = "\
# LiveFolders — how to use this filesystem

You are inside a virtual filesystem that exposes tools as files.
Every tool is a directory. To use a tool, write to one of its endpoint files and then read the result.

## Discover what is available

```
cat /tools/index.md          # list all tools and their descriptions
ls /tools/<name>/            # list endpoints for a specific tool
cat /tools/<name>/how_to.md  # read detailed usage for a specific tool
```

## Invoke a tool

Write your input to an endpoint file. The write blocks until the tool finishes.
Read the result immediately after — no sleep needed.

```
echo \"<your input>\" > /tools/<name>/<endpoint>
cat /tools/<name>/<endpoint>
```

Or chain them:

```
echo \"<your input>\" > /tools/<name>/<endpoint> && cat /tools/<name>/<endpoint>
```

The result is cleared after you read it — the file resets to empty, ready for the next invocation.

## Example — search GitHub

```
cat /tools/github/how_to.md
echo \"language:rust fuse stars:>100\" > /tools/github/search_repos
sleep 3
cat /tools/github/search_repos
```

## Rules

- **Write first, read after.** Reading before the tool finishes returns empty.
- **One result per write.** Reading consumes the result. Write again to invoke again.
- **Endpoint files always exist** — their size reflects result availability (0 = not ready).
- **how_to.md is read-only.** It describes what the tool does and what to write.
- **Regular files in tool dirs** (non-executable) are passthrough — read and write go directly to disk.
- **You can create new tools** by creating a directory under /tools/ and adding executable scripts.

## Creating a tool on the fly

```
mkdir /tools/mytool
echo \"# My tool\\nWrite a URL. Returns the page title.\" > /tools/mytool/how_to.md
printf '#!/bin/bash\\ncurl -s \"$(cat -)\" | grep -o \"<title>[^<]*\"\\n' > /tools/mytool/fetch
chmod +x /tools/mytool/fetch
```

The tool is live immediately — no restart needed.
";
