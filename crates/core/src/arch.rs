use crate::model::RepoManifest;
use crate::tokens::count_tokens;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ArchitectureMap {
    pub markdown: String,
    pub dag_mermaid: String,
    pub layers: BTreeMap<String, Vec<String>>,
    pub communication_patterns: Vec<CommunicationPattern>,
    pub token_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunicationPattern {
    pub kind: String,
    pub evidence: Vec<String>,
}

const GRPC_MARKERS: &[&str] = &["grpc.Dial", "grpc.NewServer", "@grpc/"];

const QUEUE_MARKERS: &[&str] = &[
    "amqp.",
    "pika.",
    "BullMQ",
    "Queue(",
    "@upstash/redis",
    "kafka",
    "confluent",
    "ar://",
];

const BUS_MARKERS: &[&str] = &[
    "EventEmitter",
    "mitt(",
    "eventbus",
    "socket.io",
    "SocketIOServer",
    "WebSocketServer",
    "nats.connect",
    "ioredis",
];

const DB_MARKERS: &[&str] = &[
    "prisma",
    "@prisma/client",
    "sequelize",
    "typeorm",
    "mongoose",
    "sqlalchemy",
    "diesel::",
    "sqlx::",
    "psycopg",
    "createPool",
    "knex",
    "drizzle",
];

const AUTH_MARKERS: &[&str] = &[
    "jsonwebtoken",
    "jose",
    "passport",
    "next-auth",
    "@auth/core",
    "bcrypt",
    "argon2",
    "OAuth",
    "session(",
];

const MAX_EVIDENCE_PER_KIND: usize = 4;
const MAX_MAP_TOKENS: usize = 2000;

pub fn build_architecture_map(manifest: &RepoManifest) -> ArchitectureMap {
    let layers = compute_layers(manifest);
    let patterns = detect_patterns(manifest);
    let dag_mermaid = render_mermaid_dag(manifest, &layers);
    let ascii = render_ascii_layers(manifest, &layers);
    let table = render_package_table(manifest);
    let comms = render_communication_patterns(&patterns);
    let contracts = render_contracts(manifest);
    let entry_points = render_entry_points(manifest);
    let langs = render_languages(manifest);

    let mut markdown = String::new();
    markdown.push_str(&format!(
        "- packages: {}\n- dependency edges: {}\n\n",
        manifest.packages.len(),
        manifest.dependency_graph.len()
    ));
    markdown.push_str("## Package Layers\n\n```\n");
    markdown.push_str(&ascii);
    markdown.push_str("```\n\n");
    markdown.push_str(&format!("## Languages\n\n{langs}\n\n"));
    markdown.push_str(&table);
    markdown.push_str(&entry_points);
    markdown.push_str(&contracts);
    markdown.push_str(&comms);
    markdown.push_str("\n## Dependency DAG (mermaid)\n\n```mermaid\n");
    markdown.push_str(&dag_mermaid);
    markdown.push_str("```\n");

    if count_tokens(&markdown) > MAX_MAP_TOKENS {
        markdown = condense(manifest, &layers, &patterns);
    }

    ArchitectureMap {
        token_count: count_tokens(&markdown),
        markdown,
        dag_mermaid,
        layers,
        communication_patterns: patterns,
    }
}

fn compute_layers(manifest: &RepoManifest) -> BTreeMap<String, Vec<String>> {
    let names: Vec<&str> = manifest.package_names();
    let depth: BTreeMap<&str, usize> = names
        .iter()
        .map(|n| (*n, layer_depth(manifest, n, &mut HashSet::new())))
        .collect();
    let mut layers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for name in names {
        let d = depth.get(name).copied().unwrap_or(0);
        layers
            .entry(format!("layer {d}"))
            .or_default()
            .push(name.to_string());
    }
    for v in layers.values_mut() {
        v.sort();
    }
    layers
}

fn layer_depth(manifest: &RepoManifest, name: &str, guard: &mut HashSet<String>) -> usize {
    if !guard.insert(name.to_string()) {
        return 0;
    }
    let deps = manifest.direct_dependencies(name);
    if deps.is_empty() {
        return 0;
    }
    let max = deps
        .iter()
        .map(|d| layer_depth(manifest, d, guard) + 1)
        .max()
        .unwrap_or(0);
    guard.remove(name);
    max
}

fn render_ascii_layers(manifest: &RepoManifest, layers: &BTreeMap<String, Vec<String>>) -> String {
    let mut out = String::new();
    for (layer, members) in layers {
        out.push_str(&format!("{layer}:\n"));
        for m in members {
            let framework = manifest
                .package(m)
                .and_then(|p| p.framework.clone())
                .map(|f| format!(" ({f})"))
                .unwrap_or_default();
            out.push_str(&format!("  - {m}{framework}\n"));
        }
    }
    out
}

fn render_package_table(manifest: &RepoManifest) -> String {
    if manifest.packages.is_empty() {
        return String::new();
    }
    let mut out = String::from("## Packages\n\n| package | path | lang | framework | files | deps |\n");
    out.push_str("|---|---|---|---|---|---|\n");
    let mut sorted: Vec<&crate::model::Package> = manifest.packages.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    for p in sorted {
        out.push_str(&format!(
            "| {} | `{}` | {} | {} | {} | {} |\n",
            p.name,
            p.path,
            p.language,
            p.framework.clone().unwrap_or_else(|| "-".to_string()),
            p.file_count,
            p.deps.join(", ")
        ));
    }
    out.push('\n');
    out
}

fn render_entry_points(manifest: &RepoManifest) -> String {
    if manifest.entry_points.is_empty() {
        return String::new();
    }
    let mut out = String::from("## Entry Points\n\n");
    for entry in manifest.entry_points.iter().take(20) {
        out.push_str(&format!("- `{entry}`\n"));
    }
    out.push('\n');
    out
}

fn render_contracts(manifest: &RepoManifest) -> String {
    if manifest.shared_contracts.is_empty() {
        return String::new();
    }
    let mut out = String::from("## Shared Contracts\n\n");
    for contract in manifest.shared_contracts.iter().take(20) {
        out.push_str(&format!("- `{contract}`\n"));
    }
    out.push('\n');
    out
}

fn render_languages(manifest: &RepoManifest) -> String {
    if manifest.languages.is_empty() {
        return String::from("- unknown\n\n");
    }
    let mut parts: Vec<String> = manifest
        .languages
        .iter()
        .map(|(lang, count)| format!("{lang} ({count})"))
        .collect();
    parts.sort();
    let mut out = format!("- {}\n\n", parts.join(", "));
    out.push_str("");
    out
}

fn render_communication_patterns(patterns: &[CommunicationPattern]) -> String {
    if patterns.is_empty() {
        return String::from("## Communication Patterns\n\n- none detected\n\n");
    }
    let mut out = String::from("## Communication Patterns\n\n");
    for p in patterns {
        out.push_str(&format!("- **{}**: {}\n", p.kind, p.evidence.join(", ")));
    }
    out.push('\n');
    out
}

fn render_mermaid_dag(manifest: &RepoManifest, layers: &BTreeMap<String, Vec<String>>) -> String {
    let mut out = String::from("graph TD\n");
    for members in layers.values() {
        for m in members {
            out.push_str(&format!("  {}\n", mermaid_id(m)));
        }
    }
    for edge in &manifest.dependency_graph {
        out.push_str(&format!(
            "  {} --> {}\n",
            mermaid_id(&edge.from),
            mermaid_id(&edge.to)
        ));
    }
    out
}

fn mermaid_id(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect();
    format!("n{cleaned}")
}

fn condense(
    manifest: &RepoManifest,
    layers: &BTreeMap<String, Vec<String>>,
    patterns: &[CommunicationPattern],
) -> String {
    let mut out = String::from("Condensed view (full map exceeded the token budget).\n\n");
    out.push_str(&render_ascii_layers(manifest, layers));
    out.push_str(&render_communication_patterns(patterns));
    let mut edges: Vec<String> = manifest
        .dependency_graph
        .iter()
        .map(|e| format!("{} -> {}", e.from, e.to))
        .collect();
    edges.sort();
    edges.dedup();
    edges.truncate(120);
    out.push_str("\n## Edges\n\n");
    for e in edges {
        out.push_str(&format!("- {e}\n"));
    }
    out
}

fn detect_patterns(manifest: &RepoManifest) -> Vec<CommunicationPattern> {
    let mut found: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    for pkg in &manifest.packages {
        let _ = pkg;
    }
    for dep in all_deps(manifest) {
        let lower = dep.to_ascii_lowercase();
        for marker in GRPC_MARKERS {
            if lower.contains(&marker.to_ascii_lowercase()) {
                push_evidence(&mut found, "grpc", marker);
            }
        }
        for marker in QUEUE_MARKERS {
            if lower.contains(&marker.to_ascii_lowercase()) {
                push_evidence(&mut found, "queue", marker);
            }
        }
        for marker in BUS_MARKERS {
            if lower.contains(&marker.to_ascii_lowercase()) {
                push_evidence(&mut found, "event-bus/websocket", marker);
            }
        }
        for marker in DB_MARKERS {
            if lower.contains(&marker.to_ascii_lowercase()) {
                push_evidence(&mut found, "database-orm", marker);
            }
        }
        for marker in AUTH_MARKERS {
            if lower.contains(&marker.to_ascii_lowercase()) {
                push_evidence(&mut found, "auth", marker);
            }
        }
    }
    let mut patterns: Vec<CommunicationPattern> = Vec::new();
    for (kind, mut evidence) in found {
        evidence.sort();
        evidence.dedup();
        evidence.truncate(MAX_EVIDENCE_PER_KIND);
        patterns.push(CommunicationPattern {
            kind: kind.to_string(),
            evidence,
        });
    }
    patterns.sort_by(|a, b| a.kind.cmp(&b.kind));
    patterns
}

fn push_evidence(map: &mut BTreeMap<&'static str, Vec<String>>, kind: &'static str, marker: &str) {
    map.entry(kind).or_default().push(marker.to_string());
}

fn all_deps(manifest: &RepoManifest) -> BTreeSet<String> {
    let mut deps: BTreeSet<String> = BTreeSet::new();
    for pkg in &manifest.packages {
        for d in &pkg.external_deps {
            deps.insert(d.clone());
        }
    }
    deps
}

pub fn file_role_hint(path: &str) -> Option<&'static str> {
    let stem = Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    match stem.as_str() {
        "index" | "main" | "app" | "server" => Some("entry point"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DependencyEdge, Package};

    fn fixture() -> RepoManifest {
        RepoManifest {
            root: "/tmp/repo".to_string(),
            generated_by: "test".to_string(),
            languages: BTreeMap::from([("typescript".to_string(), 12)]),
            packages: vec![
                Package {
                    name: "web".to_string(),
                    path: "apps/web".to_string(),
                    language: "node".to_string(),
                    framework: Some("next.js".to_string()),
                    manifest: "apps/web/package.json".to_string(),
                    entry_points: vec!["apps/web/main.ts".to_string()],
                    deps: vec!["ui".to_string()],
                    external_deps: vec!["next".to_string(), "prisma".to_string()],
                    file_count: 5,
                    lines_of_code: 400,
                },
                Package {
                    name: "ui".to_string(),
                    path: "packages/ui".to_string(),
                    language: "node".to_string(),
                    framework: None,
                    manifest: "packages/ui/package.json".to_string(),
                    entry_points: vec!["packages/ui/index.ts".to_string()],
                    deps: vec![],
                    external_deps: vec!["react".to_string()],
                    file_count: 3,
                    lines_of_code: 200,
                },
            ],
            dependency_graph: vec![DependencyEdge {
                from: "web".to_string(),
                to: "ui".to_string(),
                kind: "workspace".to_string(),
            }],
            entry_points: vec!["apps/web/main.ts".to_string()],
            shared_contracts: vec!["packages/ui/types.ts".to_string()],
        }
    }

    #[test]
    fn computes_layers_with_dependencies_below_dependents() {
        let map = build_architecture_map(&fixture());
        assert_eq!(map.layers.get("layer 0").unwrap(), &vec!["ui".to_string()]);
        assert_eq!(map.layers.get("layer 1").unwrap(), &vec!["web".to_string()]);
    }

    #[test]
    fn map_stays_within_token_budget() {
        let map = build_architecture_map(&fixture());
        assert!(map.token_count <= 2000, "got {}", map.token_count);
    }

    #[test]
    fn map_has_no_duplicate_top_level_heading() {
        let map = build_architecture_map(&fixture());
        let headings: Vec<&str> = map
            .markdown
            .lines()
            .filter(|l| l.starts_with("# ") && !l.starts_with("## "))
            .collect();
        assert!(headings.is_empty(), "map must not add its own h1: {headings:?}");
        assert!(map.markdown.starts_with("- packages:"));
    }

    #[test]
    fn detects_communication_patterns() {
        let map = build_architecture_map(&fixture());
        let kinds: Vec<&str> = map
            .communication_patterns
            .iter()
            .map(|p| p.kind.as_str())
            .collect();
        assert!(kinds.contains(&"database-orm"));
    }

    #[test]
    fn renders_mermaid_edges() {
        let map = build_architecture_map(&fixture());
        assert!(map.dag_mermaid.contains("nweb --> nui"));
    }

    #[test]
    fn handles_cycle_without_infinite_loop() {
        let mut manifest = fixture();
        manifest.dependency_graph.push(DependencyEdge {
            from: "ui".to_string(),
            to: "web".to_string(),
            kind: "workspace".to_string(),
        });
        let map = build_architecture_map(&manifest);
        assert!(map.markdown.contains("Package Layers"));
    }
}