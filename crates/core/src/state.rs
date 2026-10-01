use crate::model::StateBlock;
use regex::Regex;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::OnceLock;

fn json_block_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?s)```(?:json|jsonc)?\s*(\{.*?\})\s*```").expect("valid state regex")
    })
}

fn all_json_blocks(response: &str) -> Vec<String> {
    json_block_re()
        .captures_iter(response)
        .map(|c| c[1].to_string())
        .collect()
}

fn state_header_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?is)#+\s*STATE\b(.*)$").expect("valid header regex"))
}

pub fn parse_state(response: &str) -> anyhow::Result<StateBlock> {
    if let Ok(value) = serde_json::from_str::<Value>(response.trim()) {
        if let Some(state) = from_json(&value) {
            return Ok(state);
        }
    }

    for candidate in all_json_blocks(response).iter().rev() {
        if let Ok(value) = serde_json::from_str::<Value>(candidate) {
            if let Some(state) = from_json(&value) {
                return Ok(state);
            }
        }
    }

    if let Some(caps) = state_header_re().captures(response) {
        let tail = &caps[1];
        if let Ok(value) = serde_json::from_str::<Value>(tail.trim()) {
            if let Some(state) = from_json(&value) {
                return Ok(state);
            }
        }
        if let Some(state) = parse_bullet_list(tail) {
            return Ok(state);
        }
    }

    if let Some(state) = parse_bullet_list(response) {
        return Ok(state);
    }

    anyhow::bail!("no STATE block found in model response")
}

fn from_json(value: &Value) -> Option<StateBlock> {
    let obj = value.as_object()?;
    let files_read = string_list(obj.get("files_read"));
    let open_questions = string_list(obj.get("open_questions"));
    let next_files = string_list(obj.get("next_files_to_read"));

    if files_read.is_empty() && open_questions.is_empty() && next_files.is_empty() {
        return None;
    }

    Some(StateBlock {
        files_read,
        open_questions,
        next_files_to_read: next_files,
        phase: obj
            .get("phase")
            .and_then(|p| p.as_u64())
            .unwrap_or(0) as usize,
        notes: obj
            .get("notes")
            .and_then(|n| n.as_str())
            .map(|s| s.to_string()),
    })
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| i.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        Some(Value::String(s)) => vec![s.trim().to_string()],
        _ => Vec::new(),
    }
}

fn parse_bullet_list(text: &str) -> Option<StateBlock> {
    let re = Regex::new(r"(?im)^\s*(?:[-*]|\d+\.)\s*").ok()?;
    let mut current: Option<&str> = None;
    let mut files_read = Vec::new();
    let mut open_questions = Vec::new();
    let mut next_files = Vec::new();

    for line in text.lines() {
        let stripped = re.replace(line, "");
        let trimmed = stripped.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lowered = trimmed.to_ascii_lowercase();
        current = if lowered.starts_with("files_read")
            || lowered.starts_with("files read")
        {
            Some("files_read")
        } else if lowered.starts_with("open_questions") || lowered.starts_with("open questions")
        {
            Some("open_questions")
        } else if lowered.starts_with("next_files_to_read")
            || lowered.starts_with("next files to read")
            || lowered.starts_with("next files")
        {
            Some("next_files_to_read")
        } else {
            current
        };

        let value = trimmed
            .split_once(':')
            .map(|(_, v)| v.trim())
            .unwrap_or("")
            .trim_matches(['`', '*', '"', '\''])
            .to_string();

        if value.is_empty() || value == "[]" || value == "none" {
            continue;
        }
        for item in value.split(',') {
            let item = item.trim().trim_matches(['`', '*', '"', '\'']);
            if item.is_empty() || item == "[]" {
                continue;
            }
            match current {
                Some("files_read") => files_read.push(item.to_string()),
                Some("open_questions") => open_questions.push(item.to_string()),
                Some("next_files_to_read") => next_files.push(item.to_string()),
                _ => {}
            }
        }
    }

    if files_read.is_empty() && open_questions.is_empty() && next_files.is_empty() {
        return None;
    }
    Some(StateBlock {
        files_read,
        open_questions,
        next_files_to_read: next_files,
        phase: 0,
        notes: None,
    })
}

pub fn merge_state(prior: &StateBlock, next: &StateBlock) -> StateBlock {
    let mut files_read = prior.files_read.clone();
    let mut seen: HashSet<String> = files_read.iter().cloned().collect();
    for path in &next.files_read {
        if seen.insert(path.clone()) {
            files_read.push(path.clone());
        }
    }

    let mut open_questions = prior.open_questions.clone();
    for q in &next.open_questions {
        if !open_questions.contains(q) {
            open_questions.push(q.clone());
        }
    }

    let mut next_files: Vec<String> = next
        .next_files_to_read
        .iter()
        .filter(|p| !seen.contains(*p))
        .cloned()
        .collect();
    if next_files.is_empty() {
        next_files = prior
            .next_files_to_read
            .iter()
            .filter(|p| !seen.contains(*p))
            .cloned()
            .collect();
    }

    StateBlock {
        files_read,
        open_questions,
        next_files_to_read: crate::engine::dedupe_preserve(next_files),
        phase: next.phase.max(prior.phase + 1),
        notes: next.notes.clone().or_else(|| prior.notes.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fenced_json_block() {
        let response = "Here is my analysis.\n\n```json\n{\"files_read\": [\"a.ts\"], \"open_questions\": [\"why?\"], \"next_files_to_read\": [\"b.ts\"]}\n```\n";
        let state = parse_state(response).unwrap();
        assert_eq!(state.files_read, vec!["a.ts".to_string()]);
        assert_eq!(state.open_questions, vec!["why?".to_string()]);
        assert_eq!(state.next_files_to_read, vec!["b.ts".to_string()]);
    }

    #[test]
    fn parses_bare_json_response() {
        let response = "{\"files_read\": [\"x/y.ts\"]}";
        let state = parse_state(response).unwrap();
        assert_eq!(state.files_read.len(), 1);
    }

    #[test]
    fn parses_state_header_with_bullets() {
        let response = "# STATE\n- files_read: `a.ts`, `b.ts`\n- open_questions: `is this safe?`\n- next_files_to_read: `c.ts`\n";
        let state = parse_state(response).unwrap();
        assert_eq!(state.files_read.len(), 2);
        assert_eq!(state.next_files_to_read, vec!["c.ts".to_string()]);
    }

    #[test]
    fn picks_last_fenced_state_block() {
        let response = "```json\n{\"files_read\": [\"first.ts\"]}\n```\nmore text\n```json\n{\"files_read\": [\"second.ts\"], \"next_files_to_read\": [\"third.ts\"]}\n```";
        let state = parse_state(response).unwrap();
        assert_eq!(state.files_read, vec!["second.ts".to_string()]);
    }

    #[test]
    fn errors_when_no_state_present() {
        assert!(parse_state("just prose without state").is_err());
    }

    #[test]
    fn merge_accumulates_files_read() {
        let prior = StateBlock {
            files_read: vec!["a.ts".to_string()],
            open_questions: vec![],
            next_files_to_read: vec!["b.ts".to_string()],
            phase: 0,
            notes: None,
        };
        let next = StateBlock {
            files_read: vec!["a.ts".to_string(), "b.ts".to_string()],
            open_questions: vec!["q1".to_string()],
            next_files_to_read: vec!["c.ts".to_string()],
            phase: 0,
            notes: None,
        };
        let merged = merge_state(&prior, &next);
        assert_eq!(merged.files_read, vec!["a.ts", "b.ts"]);
        assert_eq!(merged.open_questions, vec!["q1"]);
        assert_eq!(merged.next_files_to_read, vec!["c.ts"]);
        assert_eq!(merged.phase, 1);
    }

    #[test]
    fn merge_drops_already_read_from_next() {
        let prior = StateBlock {
            files_read: vec!["a.ts".to_string()],
            open_questions: vec![],
            next_files_to_read: vec![],
            phase: 0,
            notes: None,
        };
        let next = StateBlock {
            files_read: vec!["a.ts".to_string()],
            open_questions: vec![],
            next_files_to_read: vec!["a.ts".to_string()],
            phase: 0,
            notes: None,
        };
        let merged = merge_state(&prior, &next);
        assert!(merged.next_files_to_read.is_empty());
    }

    #[test]
    fn ignores_empty_state_objects() {
        assert!(parse_state("```json\n{\"files_read\": [], \"open_questions\": [], \"next_files_to_read\": []}\n```").is_err());
    }
}