pub const ROOT_HOW_TO: &str = "\
# ModixFS — how to use this filesystem

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
