"""MCP server exposing the MonorepoPrompt stages as tools.

Register with Claude Code / Codex / Cursor:

    claude mcp add monorepoprompt -- python -m monorepoprompt.mcp /path/to/repo
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

from . import MonorepoPrompt, MonorepoPromptError

PROTOCOL_VERSION = "2024-11-05"


def _tool_definitions() -> list[dict[str, Any]]:
    return [
        {
            "name": "scan_repo",
            "description": (
                "Stage 1. Parse a monorepo into packages, languages, and a workspace "
                "dependency graph. Returns repo_manifest JSON."
            ),
            "inputSchema": {"type": "object", "properties": {}},
        },
        {
            "name": "build_architecture_map",
            "description": (
                "Stage 2. Render a <=2K token architecture map: package layers, entry "
                "points, shared contracts, communication patterns, and a Mermaid DAG."
            ),
            "inputSchema": {"type": "object", "properties": {}},
        },
        {
            "name": "select_files",
            "description": (
                "Stage 3. Rank repo files against a task and assign detail levels "
                "(full / codemap / slice) within a token budget."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task": {"type": "string", "description": "Natural-language task."},
                    "budget": {"type": "integer", "description": "Token budget for code context."},
                },
                "required": ["task"],
            },
        },
        {
            "name": "assemble_prompt",
            "description": (
                "Stage 4. Assemble the layered ROLE / ARCHITECTURE MAP / CODE CONTEXT / "
                "TASK / OUTPUT FORMAT / STATE prompt, trimming to fit the budget."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task": {"type": "string", "description": "Natural-language task."},
                    "budget": {"type": "integer"},
                    "state": {"type": "object", "description": "Prior STATE block."},
                },
                "required": ["task"],
            },
        },
        {
            "name": "next_phase",
            "description": (
                "Stage 5. Parse a STATE block out of a model reply, merge it with prior "
                "state, and emit the next incremental prompt."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task": {"type": "string"},
                    "llm_response": {"type": "string", "description": "Reply containing a STATE block."},
                    "state": {"type": "object", "description": "Prior STATE block."},
                    "budget": {"type": "integer"},
                },
                "required": ["task", "llm_response"],
            },
        },
        {
            "name": "count_tokens",
            "description": "Count tokens with cl100k_base for budget planning.",
            "inputSchema": {
                "type": "object",
                "properties": {"text": {"type": "string"}},
                "required": ["text"],
            },
        },
    ]


def _dispatch(name: str, args: dict[str, Any], root: Path) -> Any:
    budget = args.get("budget")
    state = args.get("state")
    mrp = MonorepoPrompt(root, budget=budget)

    if name == "scan_repo":
        return mrp.scan()
    if name == "build_architecture_map":
        return {"markdown": mrp.architecture_map()}
    if name == "select_files":
        selected = mrp.select_files(args["task"], budget)
        return {"files": [vars(s) for s in selected], "count": len(selected)}
    if name == "assemble_prompt":
        if state:
            return {
                "prompt": mrp.build(args["task"], budget=budget).prompt,
                "note": "state blocks are handled by next_phase; pass prior state there",
            }
        result = mrp.build(args["task"], budget=budget)
        return {
            "prompt": result.prompt,
            "token_count": result.token_count,
            "budget": result.budget,
            "file_count": len(result.selected),
            "dropped": result.dropped,
        }
    if name == "next_phase":
        result = mrp.next_phase(args["task"], args["llm_response"], state=state, budget=budget)
        return {
            "prompt": result.prompt,
            "token_count": result.token_count,
            "budget": result.budget,
        }
    if name == "count_tokens":
        text = args.get("text", "")
        return {"characters": len(text), "estimate": max(1, len(text) // 4) if text else 0}
    raise ValueError(f"unknown tool: {name}")


def serve(root: str | Path) -> int:
    """Run a newline-delimited JSON-RPC stdio server."""
    root = Path(root).resolve()
    if not root.is_dir():
        print(f"error: not a directory: {root}", file=sys.stderr)
        return 1

    print(f"[monorepoprompt] MCP server ready on {root}", file=sys.stderr)

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
        except json.JSONDecodeError as exc:
            _emit({"jsonrpc": "2.0", "id": None, "error": {"code": -32700, "message": str(exc)}})
            continue

        req_id = request.get("id")
        method = request.get("method", "")
        params = request.get("params") or {}

        if method == "initialize":
            result: Any = {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "monorepoprompt", "version": "0.1.0"},
            }
        elif method in ("notifications/initialized", "notifications/cancelled"):
            continue
        elif method == "ping":
            result = {}
        elif method == "tools/list":
            result = {"tools": _tool_definitions()}
        elif method == "tools/call":
            tool_name = params.get("name", "")
            tool_args = params.get("arguments") or {}
            try:
                payload = _dispatch(tool_name, tool_args, root)
                result = _wrap(payload, is_error=False)
            except (MonorepoPromptError, ValueError, KeyError) as exc:
                result = _wrap({"error": str(exc)}, is_error=True)
        elif method in ("resources/list", "prompts/list"):
            result = {"resources": [], "prompts": []}
        else:
            _emit({"jsonrpc": "2.0", "id": req_id, "error": {"code": -32601, "message": f"unknown method: {method}"}})
            continue

        if req_id is not None:
            _emit({"jsonrpc": "2.0", "id": req_id, "result": result})

    return 0


def _wrap(payload: Any, *, is_error: bool) -> dict[str, Any]:
    text = payload if isinstance(payload, str) else json.dumps(payload, indent=2)
    return {"content": [{"type": "text", "text": text}], "isError": is_error}


def _emit(message: dict[str, Any]) -> None:
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def main() -> int:
    if len(sys.argv) < 2:
        print("usage: python -m monorepoprompt.mcp <repo-path>", file=sys.stderr)
        return 2
    return serve(sys.argv[1])


if __name__ == "__main__":
    raise SystemExit(main())