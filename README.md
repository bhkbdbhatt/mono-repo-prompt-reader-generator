# MonorepoPrompt

Builds a surgical, layered prompt from a monorepo and a natural-language task.

Pipeline: `scan -> architecture map -> file selection -> prompt assembly -> state loop`.

## Requirements

- Rust toolchain. On Windows without MSVC tooling, use the GNU toolchain:
  `cargo +stable-x86_64-pc-windows-gnu build --release`
- Python 3.10+ only if you want the Python SDK or the Python MCP server.

## Layout

```
crates/core     monorepoprompt-core: scan, arch, select, assemble, state, tokens
crates/cli      monorepoprompt binary + MCP stdio server
python          Python SDK and MCP server (shells out to the CLI)
mcp             MCP server configuration
monorepoprompt.yaml  default configuration
```

## Build and test

```bash
cargo +stable-x86_64-pc-windows-gnu test
cargo +stable-x86_64-pc-windows-gnu build --release
```

The binary lands at `target/release/monorepoprompt.exe` on Windows.

## CLI

```bash
monorepoprompt scan <repo>                      # repo_manifest.json
monorepoprompt map <repo>                       # architecture_map.md
monorepoprompt build <repo> -t "task" --budget 60000
monorepoprompt build <repo> -t "task" --interactive   # phase loop on stdin
monorepoprompt next <repo> -t "task" --response reply.md --state state.json
monorepoprompt mcp                              # MCP stdio server
```

Use `-o <file>` to write output instead of stdout, `--selection <file>` to also
emit `selected_files.json`, and `--explain` for a per-section token breakdown on
stderr. `--interactive` accepts `--phases <n>` (default 3).

`--budget`, `--role`, `--config`, and `-q` work both before and after the
subcommand. Run `monorepoprompt <command> --help` for full per-command usage.

On Windows, generated prompts are copied to the clipboard.

## Python SDK

```python
from monorepoprompt import MonorepoPrompt

m = MonorepoPrompt("/path/to/repo")
result = m.build("trace the auth flow from login to refresh", budget=60_000)
print(result.token_count, result.within_budget)
print(result.prompt)

reply = """done

```json
{"files_read": ["packages/auth/src/login.ts"], "open_questions": ["where is state kept?"], "next_files_to_read": ["packages/auth/src/session-store.ts"]}
```
"""
phase2 = m.next_phase("trace the auth flow", reply, budget=60_000)
```

Methods: `scan`, `architecture_map`, `select_files`, `build`, `next_phase`,
`count_tokens`. `MonorepoPrompt` locates the binary via `MONOREPROMPT_BIN`,
then the local `target/release` directory, then `PATH`.

## MCP

Rust server (no extra dependencies):

```json
{
  "mcpServers": {
    "monorepoprompt": {
      "command": "/abs/path/to/target/release/monorepoprompt.exe",
      "args": ["mcp"]
    }
  }
}
```

Python server:

```bash
python -m monorepoprompt.mcp
```

Tools on both: `scan_repo`, `build_architecture_map`, `select_files`,
`assemble_prompt`, `next_phase`, `count_tokens`.

## Prompt contract

Every prompt contains these sections in order:

```
# ROLE
# ARCHITECTURE MAP
# CODE CONTEXT
# TASK
# OUTPUT FORMAT
# STATE
```

The model is instructed to end its reply with a STATE object:

```json
{
  "files_read": ["..."],
  "open_questions": ["..."],
  "next_files_to_read": ["..."],
  "phase": 1
}
```

`next_phase` merges that STATE, carries the prior STATE block forward verbatim,
honors `next_files_to_read` at full or codemap detail, and never resends a file
the model reported in `files_read` or the engine already emitted. Once nothing is
left, the prompt says so instead of repeating context.

## Configuration

`monorepoprompt.yaml` is discovered by walking upward from the target path. See
that file for the defaults, including token budget and detail overrides.

## Current limitations

- Relevance uses keyword and path matching, not embeddings (`fastembed`).
- Code maps use regex signature extraction, not `tree-sitter`.
- The Python SDK shells out to the Rust CLI; there are no PyO3 bindings.
- No TUI.
- Token counts use `tiktoken-rs` with `cl100k_base`.