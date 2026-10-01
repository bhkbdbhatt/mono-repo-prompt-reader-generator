use crate::arch::build_architecture_map;
use crate::codemap::{codemap, first_window, slice_of};
use crate::engine::{default_detail_for, Engine};
use crate::model::{DetailLevel, RepoManifest, SelectedFile, StateBlock};
use crate::tokens::count_tokens;
use anyhow::Result;
use std::collections::HashSet;

const FILE_HEADER_OVERHEAD: usize = 24;

pub struct Assembled {
    pub prompt: String,
    pub token_count: usize,
    pub sections: Vec<(String, usize)>,
    pub dropped: Vec<String>,
    pub downgraded: Vec<(String, DetailLevel, DetailLevel)>,
}

pub fn assemble_prompt(
    engine: &Engine,
    task: &str,
    selected: &[SelectedFile],
    state: Option<&StateBlock>,
) -> Result<String> {
    Ok(assemble(engine, task, selected, state).prompt)
}

pub fn assemble(
    engine: &Engine,
    task: &str,
    selected: &[SelectedFile],
    state: Option<&StateBlock>,
) -> Assembled {
    let map = build_architecture_map(&engine.manifest);
    let arch_budget = engine.config.map_token_budget;
    let architecture = crate::tokens::truncate_to_tokens(&map.markdown, arch_budget);

    let mut sections: Vec<(String, usize)> = Vec::new();
    let mut role_block = String::new();
    role_block.push_str("# ROLE\n\n");
    role_block.push_str(&engine.config.role);
    role_block.push_str("\n\n");
    sections.push(("ROLE".to_string(), count_tokens(&role_block)));

    let arch_section = format!("# ARCHITECTURE MAP\n\n{architecture}");
    sections.push(("ARCHITECTURE MAP".to_string(), count_tokens(&arch_section)));

    let files = selected.to_vec();
    let prior_read: HashSet<String> = state
        .map(|s| s.files_read.iter().cloned().collect())
        .unwrap_or_default();

    let state_rendered = state.map(render_state_block).unwrap_or_default();
    if !state_rendered.is_empty() {
        sections.push(("STATE".to_string(), count_tokens(&state_rendered)));
    }

    let task_section = format!("# TASK\n\n{task}\n");
    let output_section = format!(
        "# OUTPUT FORMAT\n\n{}\n\nRespond in Markdown. End your reply with exactly one STATE block.\n",
        engine.config.output_format
    );
    let state_template = if engine.config.state_block {
        render_state_template()
    } else {
        String::new()
    };

    let fixed = sections.iter().map(|(_, t)| *t).sum::<usize>()
        + count_tokens(&task_section)
        + count_tokens(&output_section)
        + count_tokens(&state_template);
    let mut code_block = String::new();
    let mut dropped: Vec<String> = Vec::new();
    let mut downgraded: Vec<(String, DetailLevel, DetailLevel)> = Vec::new();
    let mut current = files.clone();
    let mut rendered: Vec<(String, DetailLevel, String)> = Vec::new();

    loop {
        rendered.clear();
        code_block.clear();
        let mut code_budget = engine.config.budget_tokens.saturating_sub(fixed).max(1_000);
        let mut skipped: Vec<SelectedFile> = Vec::new();

        for file in &current {
            if file.detail_level == DetailLevel::Drop {
                dropped.push(file.path.clone());
                continue;
            }
            if let Ok((content, actual)) = engine.render_file(file) {
                let cost = count_tokens(&content) + FILE_HEADER_OVERHEAD;
                if cost > code_budget {
                    skipped.push(file.clone());
                    continue;
                }
                code_budget -= cost;
                code_block.push_str(&format!(
                    "<file path=\"{}\" detail=\"{}\">\n{}\n</file>\n\n",
                    file.path,
                    detail_str(actual),
                    content.trim_end()
                ));
                rendered.push((file.path.clone(), actual, content));
            }
        }

        if code_budget > engine.config.budget_tokens / 10 && !rendered.is_empty() {
            break;
        }

        let mut mutated = false;
        for file in skipped {
            let mut candidate = file.clone();
            if candidate.downgrade() {
                downgraded.push((
                    candidate.path.clone(),
                    DetailLevel::Full,
                    candidate.detail_level,
                ));
                current.retain(|f| f.path != candidate.path);
                current.push(candidate);
                mutated = true;
            } else {
                dropped.push(file.path.clone());
            }
        }

        if !mutated {
            for file in rendered
                .iter()
                .rev()
                .map(|(p, d, _)| SelectedFile {
                    path: p.clone(),
                    package: String::new(),
                    role: crate::model::FileRole::Implementation,
                    detail_level: if *d == DetailLevel::Codemap {
                        DetailLevel::Slice
                    } else {
                        DetailLevel::Drop
                    },
                    lines: None,
                    score: 0.0,
                    language: None,
                    reason: None,
                })
                .collect::<Vec<_>>()
            {
                if current.iter().any(|f| f.path == file.path) {
                    current.retain(|f| f.path != file.path);
                    downgraded.push((file.path.clone(), DetailLevel::Codemap, file.detail_level));
                    mutated = true;
                }
            }
        }

        if !mutated {
            break;
        }
    }

    dropped.sort();
    dropped.dedup();
    downgraded.sort_by(|a, b| a.0.cmp(&b.0));
    downgraded.dedup_by(|a, b| a.0 == b.0);

    let code_section = format!("# CODE CONTEXT\n\n{code_block}");
    sections.push(("CODE CONTEXT".to_string(), count_tokens(&code_section)));
    sections.push(("TASK".to_string(), count_tokens(&task_section)));
    sections.push(("OUTPUT FORMAT".to_string(), count_tokens(&output_section)));

    let mut prompt = String::new();
    prompt.push_str(&role_block);
    prompt.push_str(&arch_section);
    prompt.push('\n');
    prompt.push_str(&code_section);
    prompt.push('\n');
    prompt.push_str(&task_section);
    prompt.push('\n');
    prompt.push_str(&output_section);
    if !state_template.is_empty() {
        prompt.push('\n');
        prompt.push_str(&state_template);
    }
    if !state_rendered.is_empty() {
        prompt.push('\n');
        prompt.push_str(&state_rendered);
    }

    let _ = prior_read;
    engine.record_sent(rendered.iter().map(|(path, _, _)| path.clone()));

    Assembled {
        token_count: count_tokens(&prompt),
        prompt,
        sections,
        dropped,
        downgraded,
    }
}

fn detail_str(detail: DetailLevel) -> &'static str {
    match detail {
        DetailLevel::Full => "full",
        DetailLevel::Codemap => "codemap",
        DetailLevel::Slice => "slice",
        DetailLevel::Drop => "drop",
    }
}

pub fn render_state_template() -> String {
    "# STATE\n\n```json\n{\n  \"files_read\": [],\n  \"open_questions\": [],\n  \"next_files_to_read\": []\n}\n```\n".to_string()
}

pub fn render_state_block(state: &StateBlock) -> String {
    let json = serde_json::to_string_pretty(state).unwrap_or_else(|_| "{}".to_string());
    format!("# STATE (from previous phase)\n\n```json\n{json}\n```\n")
}

pub fn synthetic_state(engine: &Engine, selected: &[SelectedFile], task: &str) -> StateBlock {
    let mut files_read: Vec<String> = selected
        .iter()
        .filter(|s| s.detail_level == DetailLevel::Full)
        .map(|s| s.path.clone())
        .collect();
    if files_read.is_empty() {
        files_read = selected
            .iter()
            .take(3)
            .map(|s| s.path.clone())
            .collect();
    }
    let candidates: Vec<String> = engine
        .files
        .iter()
        .filter(|f| !files_read.contains(&f.path))
        .filter(|f| {
            crate::scan::classify_role(&f.path) == crate::model::FileRole::SharedTypes
        })
        .map(|f| f.path.clone())
        .take(5)
        .collect();
    StateBlock {
        files_read: crate::engine::dedupe_preserve(files_read),
        open_questions: vec![format!("Confirm the intended behaviour for: {task}")],
        next_files_to_read: crate::engine::dedupe_preserve(candidates),
        phase: 0,
        notes: Some("auto-generated by monorepoprompt".to_string()),
    }
}

pub fn render_selected(manifest: &RepoManifest, selected: &[SelectedFile]) -> String {
    let mut out = String::from("[\n");
    for (i, s) in selected.iter().enumerate() {
        let package = if s.package.is_empty() {
            manifest
                .package_for_path(&s.path)
                .map(|p| p.name.clone())
                .unwrap_or_else(|| "<root>".to_string())
        } else {
            s.package.clone()
        };
        let comma = if i + 1 == selected.len() { "" } else { "," };
        out.push_str(&format!(
            "  {{\"path\": \"{}\", \"package\": \"{}\", \"detail_level\": \"{}\", \"score\": {:.3}}}{}\n",
            s.path,
            package,
            detail_str(s.detail_level),
            s.score,
            comma
        ));
    }
    out.push_str("]\n");
    out
}

#[allow(dead_code)]
fn unused(engine: &Engine, file: &SelectedFile) -> String {
    let role = file.role;
    let _ = default_detail_for(role, &engine.config);
    let content = std::fs::read_to_string(engine.root.join(&file.path)).unwrap_or_default();
    codemap(&file.path, &content, 10);
    let _ = first_window(&content, 100);
    let _ = slice_of(&file.path, &content, 1, 10);
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_str_round_trips() {
        assert_eq!(detail_str(DetailLevel::Full), "full");
        assert_eq!(detail_str(DetailLevel::Codemap), "codemap");
        assert_eq!(detail_str(DetailLevel::Slice), "slice");
    }

    #[test]
    fn state_block_renders_json() {
        let state = StateBlock {
            files_read: vec!["a.ts".to_string()],
            open_questions: vec!["q".to_string()],
            next_files_to_read: vec!["b.ts".to_string()],
            phase: 2,
            notes: None,
        };
        let rendered = render_state_block(&state);
        assert!(rendered.contains("\"files_read\""));
        assert!(rendered.contains("a.ts"));
        assert!(rendered.contains("\"phase\": 2"));
    }

    #[test]
    fn template_lists_state_fields() {
        let template = render_state_template();
        assert!(template.contains("next_files_to_read"));
    }
}