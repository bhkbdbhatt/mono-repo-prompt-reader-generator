use crate::codemap::first_window;
use crate::engine::{default_detail_for, Engine};
use crate::model::{DetailLevel, FileRole, SelectedFile, StateBlock};
use crate::scan::ScannedFile;
use std::collections::{HashMap, HashSet};

const MAX_PROBE_BYTES: usize = 4_000;

struct Term {
    text: String,
    weight: f64,
}

fn split_terms(text: &str) -> Vec<Term> {
    let mut terms: Vec<Term> = Vec::new();
    for raw in text.split(|c: char| !c.is_alphanumeric() && c != '_') {
        let token = raw.trim_matches('_').to_ascii_lowercase();
        if token.len() < 3 || is_stopword(&token) {
            continue;
        }
        let weight = if token.chars().all(|c| c.is_numeric()) {
            0.5
        } else {
            1.5
        };
        let has_separator = token.contains('_');
        terms.push(Term {
            text: token.clone(),
            weight,
        });
        if has_separator {
            for part in token.split('_') {
                if part.len() >= 3 && !is_stopword(part) {
                    terms.push(Term {
                        text: part.to_string(),
                        weight: 1.0,
                    });
                }
            }
        }
    }
    terms
}

fn is_stopword(token: &str) -> bool {
    matches!(
        token,
        "the"
            | "and"
            | "for"
            | "with"
            | "from"
            | "into"
            | "this"
            | "that"
            | "how"
            | "does"
            | "can"
            | "why"
            | "what"
            | "when"
            | "are"
            | "was"
            | "were"
            | "been"
            | "has"
            | "had"
            | "not"
            | "but"
            | "you"
            | "your"
            | "our"
            | "use"
            | "using"
            | "add"
            | "get"
            | "set"
            | "run"
            | "make"
            | "new"
            | "code"
            | "file"
            | "files"
            | "please"
            | "help"
    )
}

fn path_tokens(path: &str) -> Vec<String> {
    path.split(['/', '\\', '.', '-', '_'])
        .filter(|t| !t.is_empty())
        .map(|t| t.to_ascii_lowercase())
        .collect()
}

fn keyword_score(task_terms: &[Term], file: &ScannedFile, content: &str) -> (f64, usize) {
    if task_terms.is_empty() {
        return (0.0, 0);
    }
    let toks = path_tokens(&file.path);
    let mut score = 0.0;
    let mut matched: Vec<&str> = Vec::new();

    for term in task_terms {
        let mut hit = 0.0;
        if file.path.to_ascii_lowercase().contains(&term.text) {
            hit += 3.0 * term.weight;
        }
        for tok in &toks {
            if *tok == term.text {
                hit += 2.5 * term.weight;
            } else if tok.contains(&term.text) && term.text.len() >= 4 {
                hit += 1.2 * term.weight;
            }
        }
        let occurrences = count_occurrences(content, &term.text).min(12);
        hit += occurrences as f64 * 0.55 * term.weight;
        if hit > 0.0 {
            matched.push(&term.text);
        }
        score += hit;
    }

    if score > 0.0 {
        matched.sort_unstable();
        matched.dedup();
        score *= 1.0 + (matched.len() as f64 * 0.08);
    }

    (score, matched.len())
}

fn role_bonus(role: FileRole) -> f64 {
    match role {
        FileRole::SharedTypes => 1.6,
        FileRole::EntryPoint => 1.3,
        FileRole::Implementation => 0.6,
        FileRole::Test => 0.0,
        FileRole::Config => 0.1,
        FileRole::Docs => 0.0,
    }
}

#[cfg(test)]
fn score_file(task_terms: &[Term], file: &ScannedFile, content: &str) -> f64 {
    let (score, _) = keyword_score(task_terms, file, content);
    score + role_bonus(file.role) + size_penalty(file.size)
}

fn size_penalty(size: u64) -> f64 {
    if size > 120_000 {
        1.4
    } else {
        0.0
    }
}

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack.matches(needle).count()
}

fn normalize_requested(paths: &[String]) -> HashSet<String> {
    paths
        .iter()
        .map(|p| p.trim_start_matches("./").to_string())
        .collect()
}

pub fn select_files(engine: &Engine, task: &str, budget: usize) -> anyhow::Result<Vec<SelectedFile>> {
    let task_terms = split_terms(task);
    let cache = build_content_cache(engine);
    let mut scored: Vec<(f64, &ScannedFile, Option<String>)> = Vec::new();

    let relevant_packages = relevant_package_names(engine, task);
    let task_empty = task_terms.is_empty();

    for file in &engine.files {
        let content = cache.get(&file.path).cloned().unwrap_or_default();
        let (base, matched_terms) = keyword_score(&task_terms, file, &content);
        let package_relevant = relevant_packages.contains(&file.package);

        let admit = task_empty
            || base > 0.0
            || package_relevant
            || file.role == FileRole::SharedTypes;
        if !admit {
            continue;
        }
        if file.role == FileRole::Docs {
            continue;
        }

        let mut score = base + role_bonus(file.role) + size_penalty(file.size);
        let reason = if package_relevant {
            Some(format!("package `{}` matches task", file.package))
        } else if matched_terms > 0 {
            Some(format!("keyword match on {matched_terms} task term(s)"))
        } else {
            Some("shared contract".to_string())
        };

        if package_relevant {
            score += 2.5;
        }
        if file.role == FileRole::Test {
            score -= 1.2;
        }

        scored.push((score, file, reason));
    }

    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.path.cmp(&b.1.path))
    });

    let mut selected = Vec::new();
    let mut used = 0usize;
    let mut packages_seen: HashMap<String, usize> = HashMap::new();

    for (score, file, reason) in scored {
        if selected.len() >= engine.config.max_files {
            break;
        }
        let detail = default_detail_for(file.role, &engine.config);
        let approx = estimate_cost(engine, &cache, file, detail);
        if used + approx > budget && selected.len() >= engine.config.min_files {
            continue;
        }
        let count = packages_seen.entry(file.package.clone()).or_insert(0);
        *count += 1;
        if *count > 25 && file.role == FileRole::Implementation {
            continue;
        }
        used += approx;
        selected.push(SelectedFile {
            path: file.path.clone(),
            package: file.package.clone(),
            role: file.role,
            detail_level: detail,
            lines: None,
            score,
            language: file.language.clone(),
            reason,
        });
    }

    if selected.is_empty() {
        for file in engine.files.iter().take(engine.config.min_files) {
            selected.push(SelectedFile {
                path: file.path.clone(),
                package: file.package.clone(),
                role: file.role,
                detail_level: default_detail_for(file.role, &engine.config),
                lines: None,
                score: 0.0,
                language: file.language.clone(),
                reason: Some("fallback: no task matches".to_string()),
            });
        }
    }

    Ok(selected)
}

fn relevant_package_names(engine: &Engine, task: &str) -> HashSet<String> {
    let task_lower = task.to_ascii_lowercase();
    let mut relevant: HashSet<String> = HashSet::new();
    for pkg in &engine.manifest.packages {
        let name_lower = pkg.name.to_ascii_lowercase();
        let last = name_lower.rsplit('/').next().unwrap_or(&name_lower).to_string();
        if name_lower.len() >= 3 && task_lower.contains(&name_lower) {
            relevant.insert(pkg.name.clone());
            for dep in engine.manifest.transitive_closure(&pkg.name) {
                relevant.insert(dep);
            }
            continue;
        }
        if last.len() >= 3 && task_lower.contains(&last) {
            relevant.insert(pkg.name.clone());
            for dep in engine.manifest.transitive_closure(&pkg.name) {
                relevant.insert(dep);
            }
        }
    }
    relevant
}

fn build_content_cache(engine: &Engine) -> HashMap<String, String> {
    let mut cache = HashMap::new();
    for file in &engine.files {
        if file.role == FileRole::Docs {
            continue;
        }
        if file.size > (MAX_PROBE_BYTES * 8) as u64 {
            continue;
        }
        if let Ok(abs) = engine.abs_path(&file.path) {
            if let Ok(bytes) = std::fs::read(&abs) {
                let bytes = if bytes.len() > MAX_PROBE_BYTES {
                    bytes[..MAX_PROBE_BYTES].to_vec()
                } else {
                    bytes
                };
                let text = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
                cache.insert(file.path.clone(), text);
            }
        }
    }
    cache
}

fn estimate_cost(
    engine: &Engine,
    cache: &HashMap<String, String>,
    file: &ScannedFile,
    detail: DetailLevel,
) -> usize {
    let content_len = cache
        .get(&file.path)
        .map(|c| c.len())
        .unwrap_or(file.size as usize);
    let approx_full = content_len / 4;
    let approx = match detail {
        DetailLevel::Full => approx_full,
        DetailLevel::Codemap => (content_len / 4).min(600),
        DetailLevel::Slice => 300,
        DetailLevel::Drop => 0,
    };
    let _ = engine;
    approx + 20
}

pub fn select_for_next_phase(
    engine: &Engine,
    task: &str,
    budget: usize,
    requested: &[String],
    already_read: &[String],
) -> anyhow::Result<Vec<SelectedFile>> {
    let wanted = normalize_requested(requested);
    let claimed_read = already_read.iter().cloned().collect::<HashSet<String>>();
    let delivered = engine.sent_paths().into_iter().collect::<HashSet<String>>();
    let baseline = select_files(engine, task, budget)?;

    let mut out: Vec<SelectedFile> = Vec::new();
    let mut used = 0usize;

    for path in &wanted {
        let Some(file) = engine.files.iter().find(|f| &f.path == path) else {
            continue;
        };
        if delivered.contains(path) || claimed_read.contains(path) {
            continue;
        }

        let detail = match file.role {
            FileRole::SharedTypes | FileRole::EntryPoint => DetailLevel::Full,
            _ => DetailLevel::Codemap,
        };

        if let Ok(content) = engine.read_file(path) {
            let cost = count_lines_cost(&content, detail);
            if used + cost > budget {
                continue;
            }
            used += cost;
            let (start, end) = first_window(&content, 1_200);
            out.push(SelectedFile {
                path: file.path.clone(),
                package: file.package.clone(),
                role: file.role,
                detail_level: detail,
                lines: if detail == DetailLevel::Slice {
                    Some((start, end))
                } else {
                    None
                },
                score: 100.0,
                language: file.language.clone(),
                reason: Some("requested by model STATE.next_files_to_read".to_string()),
            });
        }
    }

    for file in baseline {
        if delivered.contains(&file.path) || claimed_read.contains(&file.path) {
            continue;
        }
        if out.iter().any(|s| s.path == file.path) {
            continue;
        }
        out.push(file);
        if used >= budget {
            break;
        }
    }

    Ok(out)
}

fn count_lines_cost(content: &str, detail: DetailLevel) -> usize {
    match detail {
        DetailLevel::Full => crate::tokens::count_tokens(content),
        DetailLevel::Codemap => (content.len() / 4).min(600),
        DetailLevel::Slice => 300,
        DetailLevel::Drop => 0,
    }
}

pub fn state_aware_paths(engine: &Engine, state: Option<&StateBlock>) -> HashSet<String> {
    let _ = engine;
    match state {
        Some(s) => s.files_read.iter().cloned().collect(),
        None => HashSet::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_task_terms_and_drops_stopwords() {
        let terms = split_terms("trace the auth flow from login");
        let words: Vec<&str> = terms.iter().map(|t| t.text.as_str()).collect();
        assert!(words.contains(&"auth"));
        assert!(words.contains(&"login"));
        assert!(!words.contains(&"the"));
    }

    #[test]
    fn splits_snake_case_into_parts() {
        let terms = split_terms("refresh_token");
        let words: Vec<&str> = terms.iter().map(|t| t.text.as_str()).collect();
        assert!(words.contains(&"refresh_token"));
        assert!(words.contains(&"token"));
    }

    #[test]
    fn short_tokens_are_ignored() {
        let terms = split_terms("a b c ab de");
        assert!(terms.is_empty());
    }

    #[test]
    fn path_tokens_split_on_separators() {
        assert_eq!(
            path_tokens("apps/web/main.ts"),
            vec!["apps", "web", "main", "ts"]
        );
    }

    #[test]
    fn scoring_prefers_matching_paths() {
        let auth = ScannedFile {
            path: "packages/auth/src/login.ts".to_string(),
            abs: std::path::PathBuf::new(),
            size: 2_000,
            role: FileRole::Implementation,
            package: "auth".to_string(),
            language: Some("typescript".to_string()),
        };
        let billing = ScannedFile {
            path: "packages/billing/src/invoice.ts".to_string(),
            abs: std::path::PathBuf::new(),
            size: 2_000,
            role: FileRole::Implementation,
            package: "billing".to_string(),
            language: Some("typescript".to_string()),
        };
        let terms = split_terms("trace auth login");
        let auth_score = score_file(&terms, &auth, "login user auth token");
        let billing_score = score_file(&terms, &billing, "invoice total");
        assert!(auth_score > billing_score);
    }

    #[test]
    fn shared_types_get_priority_bonus() {
        let terms = split_terms("session");
        let types = ScannedFile {
            path: "packages/shared/session-store.ts".to_string(),
            abs: std::path::PathBuf::new(),
            size: 1_000,
            role: FileRole::SharedTypes,
            package: "shared".to_string(),
            language: Some("typescript".to_string()),
        };
        let impl_file = ScannedFile {
            path: "packages/x/session-store.ts".to_string(),
            abs: std::path::PathBuf::new(),
            size: 1_000,
            role: FileRole::Implementation,
            package: "x".to_string(),
            language: Some("typescript".to_string()),
        };
        assert!(
            score_file(&terms, &types, "session") > score_file(&terms, &impl_file, "session"),
            "shared types should outrank identical implementation files"
        );
    }

    #[test]
    fn test_files_are_penalized() {
        let terms = split_terms("login");
        let test = ScannedFile {
            path: "packages/auth/login.test.ts".to_string(),
            abs: std::path::PathBuf::new(),
            size: 1_000,
            role: FileRole::Test,
            package: "auth".to_string(),
            language: Some("typescript".to_string()),
        };
        let src = ScannedFile {
            path: "packages/auth/login.ts".to_string(),
            abs: std::path::PathBuf::new(),
            size: 1_000,
            role: FileRole::Implementation,
            package: "auth".to_string(),
            language: Some("typescript".to_string()),
        };
        assert!(score_file(&terms, &test, "login") < score_file(&terms, &src, "login"));
    }
}