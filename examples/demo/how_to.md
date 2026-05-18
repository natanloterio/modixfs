# demo

Demonstrates all four LiveFolders file behavior types.

## Files

### shout (write_invoke)
Write text, read it back uppercased.

```bash
echo "hello world" > shout
cat shout
# → HELLO WORLD
```

### status (read_invoke)
Read to invoke the handler and get its output.

```bash
cat status
# → livefolders demo is running on <hostname> at <date>
```

### notes.txt (passthrough)
Regular file — reads and writes go directly to disk.

```bash
echo "my note" >> notes.txt
cat notes.txt
```

### how_to.md (readonly)
This file. Read-only.
