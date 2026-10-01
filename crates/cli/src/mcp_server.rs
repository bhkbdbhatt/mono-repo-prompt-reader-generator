use anyhow::Result;
use monorepoprompt_core::assemble::render_selected;
use monorepoprompt_core::model::StateBlock;
use monorepoprompt_core::{assemble, Engine};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};

const PROTOCOL_VERSION: &str = "2024-11-05";

pub fn serve(engine: Engine) -> Result<()> {
    let stdin = BufReader::new(std::io::stdin());
    let mut stdout = std::io::stdout();
    let engine = std::cell::RefCell::new(engine);

    for line in stdin.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let err = json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": {"code": -32700, "message": format!("parse error: {e}")}
                });
                writeln!(stdout, "{err}")?;
                stdout.flush()?;
                continue;
            }
        };

        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or_default();
        let params = request.get("params").cloned().unwrap_or(json!({}));

        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "monorepoprompt", "version": env!("CARGO_PKG_VERSION")}
            })),
            "notifications/initialized" | "notifications/cancelled" => continue,
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tool_definitions() })),
            "tools/call" => {
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or_default();
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                match dispatch(name, &args, &engine) {
                    Ok(value) => Ok(tool_result(value)),
                    Err(e) => Ok(tool_result(json!({
                        "isError": true,
                        "content": [{"type": "text", "text": format!("{e:#}")}]
                    }))),
                }
            }
            "resources/list" => Ok(json!({"resources": []})),
            "prompts/list" => Ok(json!({"prompts": []})),
            other => Err(anyhow::anyhow!("unknown method: {other}")),
        };

        match result {
            Ok(value) => {
                if id.is_null() {
                    continue;
                }
                let response = json!({"jsonrpc": "2.0", "id": id, "result": value});
                writeln!(stdout, "{response}")?;
            }
            Err(e) => {
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32601, "message": format!("{e:#}")}
                });
                writeln!(stdout, "{response}")?;
            }
        }
        stdout.flush()?;
    }

    Ok(())
}

fn tool_result(value: Value) -> Value {
    let text = match &value {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    };
    json!({"content": [{"type": "text", "text": text}], "isError": false})
}

fn str_field(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
}

fn dispatch(name: &str, args: &Value, engine: &std::cell::RefCell<Engine>) -> Result<Value> {
    let engine_ref = engine.borrow();
    match name {
        "scan_repo" => {
            let manifest = serde_json::to_value(&engine_ref.manifest)?;
            Ok(json!({
                "root": engine_ref.manifest.root,
                "package_count": engine_ref.manifest.packages.len(),
                "file_count": engine_ref.files.len(),
                "manifest": manifest
            }))
        }
        "build_architecture_map" => {
            let map = engine_ref.architecture_map();
            Ok(json!({
                "markdown": map.markdown,
                "token_count": map.token_count,
                "dag_mermaid": map.dag_mermaid,
                "communication_patterns": map.communication_patterns
            }))
        }
        "select_files" => {
            let task = str_field(args, "task").unwrap_or_default();
            let budget = args
                .get("budget")
                .and_then(|b| b.as_u64())
                .map(|b| b as usize)
                .unwrap_or(engine_ref.config.budget_tokens);
            let selected = engine_ref.select_files(&task, budget)?;
            let mut value = render_selected(&engine_ref.manifest, &selected);
            value.push_str(&format!(
                "\n// {} files, estimated within {} tokens\n",
                selected.len(),
                budget
            ));
            Ok(Value::String(value))
        }
        "assemble_prompt" => {
            let task = str_field(args, "task").unwrap_or_default();
            let budget = args
                .get("budget")
                .and_then(|b| b.as_u64())
                .map(|b| b as usize)
                .unwrap_or(engine_ref.config.budget_tokens);
            let state: Option<StateBlock> = args
                .get("state")
                .filter(|s| !s.is_null())
                .and_then(|s| serde_json::from_value(s.clone()).ok());
            let selected = engine_ref.select_files(&task, budget)?;
            let assembled = assemble(&engine_ref, &task, &selected, state.as_ref());
            Ok(json!({
                "prompt": assembled.prompt,
                "token_count": assembled.token_count,
                "budget": budget,
                "sections": assembled.sections.iter().map(|(n, t)| json!({"section": n, "tokens": t})).collect::<Vec<_>>(),
                "dropped": assembled.dropped,
                "file_count": selected.len()
            }))
        }
        "next_phase" => {
            let task = str_field(args, "task").unwrap_or_default();
            let budget = args
                .get("budget")
                .and_then(|b| b.as_u64())
                .map(|b| b as usize)
                .unwrap_or(engine_ref.config.budget_tokens);
            let prior: StateBlock = args
                .get("state")
                .and_then(|s| serde_json::from_value(s.clone()).ok())
                .unwrap_or_default();
            let response = str_field(args, "llm_response").unwrap_or_default();
            let (prompt, selected, merged) =
                engine_ref.next_phase(&task, &prior, &response, budget)?;
            Ok(json!({
                "prompt": prompt,
                "state": merged,
                "new_files": selected.iter().map(|s| json!({
                    "path": s.path,
                    "detail_level": format!("{:?}", s.detail_level).to_lowercase(),
                    "reason": s.reason
                })).collect::<Vec<_>>(),
                "file_count": selected.len()
            }))
        }
        "count_tokens" => {
            let text = str_field(args, "text").unwrap_or_default();
            Ok(json!({
                "tokens": monorepoprompt_core::count_tokens(&text),
                "characters": text.chars().count()
            }))
        }
        other => anyhow::bail!("unknown tool: {other}"),
    }
}

fn prop(kind: &str, description: &str) -> Value {
    json!({"type": kind, "description": description})
}

fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "scan_repo",
            "description": "Stage 1. Parse the monorepo into packages, languages, and a workspace dependency graph.",
            "inputSchema": {"type": "object", "properties": {}}
        }),
        json!({
            "name": "build_architecture_map",
            "description": "Stage 2. Render a <=2K token architecture map: package layers, entry points, shared contracts, communication patterns, Mermaid DAG.",
            "inputSchema": {"type": "object", "properties": {}}
        }),
        json!({
            "name": "select_files",
            "description": "Stage 3. Rank repo files against a task and assign detail levels (full/codemap/slice) within a token budget.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task": prop("string", "Natural-language task description."),
                    "budget": prop("integer", "Token budget for the code context. Defaults to the configured budget.")
                },
                "required": ["task"]
            }
        }),
        json!({
            "name": "assemble_prompt",
            "description": "Stage 4. Assemble the layered ROLE/ARCHITECTURE MAP/CODE CONTEXT/TASK/OUTPUT FORMAT/STATE prompt, trimming to fit the budget.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task": prop("string", "Natural-language task description."),
                    "budget": prop("integer", "Token budget. Defaults to the configured budget."),
                    "state": prop("object", "Optional prior STATE block from an earlier phase.")
                },
                "required": ["task"]
            }
        }),
        json!({
            "name": "next_phase",
            "description": "Stage 5. Parse a STATE block out of a model reply, merge it with prior state, and emit the next incremental prompt.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task": prop("string", "The original task description."),
                    "llm_response": prop("string", "The model response containing a STATE block."),
                    "state": prop("object", "Prior STATE block."),
                    "budget": prop("integer", "Token budget for this phase.")
                },
                "required": ["task", "llm_response"]
            }
        }),
        json!({
            "name": "count_tokens",
            "description": "Count tokens with cl100k_base encoding for budget planning.",
            "inputSchema": {
                "type": "object",
                "properties": {"text": prop("string", "Text to measure.")},
                "required": ["text"]
            }
        }),
    ]
}