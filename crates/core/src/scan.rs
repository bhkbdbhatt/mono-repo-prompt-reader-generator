use crate::model::{DependencyEdge, FileRole, Package, RepoManifest};
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

const MANIFESTS: &[(&str, &str)] = &[
    ("package.json", "node"),
    ("Cargo.toml", "rust"),
    ("go.mod", "go"),
    ("pyproject.toml", "python"),
    ("setup.py", "python"),
    ("build.gradle", "java"),
    ("build.gradle.kts", "java"),
    ("pom.xml", "java"),
];

const IGNORED_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".git",
    ".venv",
    "venv",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".turbo",
    ".cache",
    "vendor",
    "coverage",
    ".gradle",
    ".idea",
    ".vscode",
    "third_party",
];

const SHARED_CONTRACT_NAMES: &[&str] = &[
    "types",
    "interfaces",
    "schema",
    "schemas",
    "contracts",
    "models",
    "proto",
    "messages",
    "dto",
    "dtos",
    "events",
    "shared",
];

const ENTRY_STEMS: &[&str] = &[
    "main", "index", "app", "server", "cli", "mod", "lib", "__main__", "program", "start",
];

pub fn normalize(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn is_ignored_dir(name: &str) -> bool {
    IGNORED_DIRS.contains(&name)
}

fn language_for(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "ts" | "tsx" | "mts" | "cts" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "rs" => "rust",
        "go" => "go",
        "py" | "pyi" => "python",
        "java" | "kt" | "kts" | "scala" => "jvm",
        "rb" => "ruby",
        "php" => "php",
        "cs" => "csharp",
        "swift" => "swift",
        "c" | "h" => "c",
        "cc" | "cpp" | "hpp" | "cxx" => "cpp",
        "sql" => "sql",
        "proto" => "proto",
        "graphql" | "gql" => "graphql",
        "sh" | "bash" => "shell",
        "yml" | "yaml" => "yaml",
        "toml" => "toml",
        "json" => "json",
        "md" | "mdx" => "markdown",
        "prisma" => "prisma",
        "" => return None,
        _ => return None,
    })
}

fn is_probably_text(path: &Path) -> bool {
    language_for(path).is_some() && (!matches!(language_for(path), Some("json")))
}

pub fn classify_role(rel_path: &str) -> FileRole {
    let lower = rel_path.to_ascii_lowercase();
    let file_stem = Path::new(rel_path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    if matches!(
        lower.as_str(),
        "readme.md" | "docs/readme.md" | "contributing.md" | "architecture.md"
    ) || lower.starts_with("docs/")
        || lower.ends_with("/readme.md")
    {
        return FileRole::Docs;
    }

    if lower.contains("/test/")
        || lower.contains("/tests/")
        || lower.contains("/__tests__/")
        || lower.contains("/spec/")
        || file_stem.ends_with("_test")
        || file_stem.ends_with(".test")
        || file_stem.ends_with(".spec")
        || file_stem.starts_with("test_")
        || file_stem == "test"
        || file_stem.starts_with("bench")
    {
        return FileRole::Test;
    }

    if file_stem.ends_with(".d.ts") || lower.ends_with(".proto") || lower.ends_with(".prisma") {
        return FileRole::SharedTypes;
    }
    if SHARED_CONTRACT_NAMES.contains(&file_stem.as_str()) {
        return FileRole::SharedTypes;
    }

    let in_contract_dir = rel_path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .any(|segment| {
            matches!(
                segment.to_ascii_lowercase().as_str(),
                "types" | "interfaces" | "contracts" | "schemas" | "proto" | "shared" | "models"
            )
        });
    if in_contract_dir {
        return FileRole::SharedTypes;
    }

    let is_entry = ENTRY_STEMS.contains(&file_stem.as_str())
        && !file_stem.starts_with("test")
        && file_stem != "lib.test";
    if is_entry {
        return FileRole::EntryPoint;
    }

    if matches!(
        path_extension(rel_path).as_str(),
        "json" | "yaml" | "yml" | "toml" | "ini" | "env" | "config"
    ) {
        return FileRole::Config;
    }

    if matches!(
        path_extension(rel_path).as_str(),
        "md" | "mdx" | "txt" | "rst"
    ) {
        return FileRole::Docs;
    }

    FileRole::Implementation
}

pub fn path_extension(rel_path: &str) -> String {
    Path::new(rel_path)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

pub struct ScanResult {
    pub manifest: RepoManifest,
    pub files: Vec<ScannedFile>,
}

#[derive(Debug, Clone)]
pub struct ScannedFile {
    pub path: String,
    pub abs: PathBuf,
    pub size: u64,
    pub role: FileRole,
    pub package: String,
    pub language: Option<String>,
}

pub fn scan_repo(root: &Path) -> Result<ScanResult> {
    let root = root
        .canonicalize()
        .with_context(|| format!("cannot resolve repo root: {}", root.display()))?;

    let mut walker = ignore::WalkBuilder::new(&root);
    walker
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .require_git(false)
        .parents(true)
        .follow_links(false)
        .max_depth(Some(12));

    let mut files: Vec<ScannedFile> = Vec::new();
    let mut languages: BTreeMap<String, usize> = BTreeMap::new();
    let mut manifest_dirs: BTreeSet<PathBuf> = BTreeSet::new();

    for entry in walker.build() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        if path
            .components()
            .any(|c| c.as_os_str().to_string_lossy().split('/').any(is_ignored_dir))
        {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let rel = normalize(path.strip_prefix(&root).unwrap_or(path));
        let rel = rel.trim_start_matches("./").to_string();
        if rel.is_empty() || rel.ends_with(".lock") || rel.ends_with("lock.json") {
            continue;
        }

        if let Some((_, lang)) = MANIFESTS.iter().find(|(m, _)| *m == name) {
            if path.parent().is_some() {
                manifest_dirs.insert(path.parent().unwrap().to_path_buf());
                *languages.entry((*lang).to_string()).or_insert(0) += 1;
            }
            continue;
        }

        if !is_probably_text(path) {
            continue;
        }
        if let Some(lang) = language_for(path) {
            *languages.entry(lang.to_string()).or_insert(0) += 1;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let role = classify_role(&rel);
        if role == FileRole::Docs && meta.len() > 400_000 {
            continue;
        }
        files.push(ScannedFile {
            package: String::new(),
            path: rel,
            abs: path.to_path_buf(),
            size: meta.len(),
            role,
            language: language_for(path).map(|s| s.to_string()),
        });
    }

    let packages = build_packages(&root, &manifest_dirs, &files)?;
    for f in &mut files {
        f.package = package_for_path(&packages, &f.path)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "<root>".to_string());
    }

    let dependency_graph = build_graph(&packages);
    let entry_points = files
        .iter()
        .filter(|f| f.role == FileRole::EntryPoint)
        .map(|f| f.path.clone())
        .take(40)
        .collect();
    let shared_contracts = files
        .iter()
        .filter(|f| f.role == FileRole::SharedTypes)
        .map(|f| f.path.clone())
        .take(40)
        .collect();

    let manifest = RepoManifest {
        root: normalize(&root),
        generated_by: format!("monorepoprompt {}", env!("CARGO_PKG_VERSION")),
        languages,
        packages,
        dependency_graph,
        entry_points,
        shared_contracts,
    };

    Ok(ScanResult { manifest, files })
}

fn package_for_path<'a>(packages: &'a [Package], rel_path: &str) -> Option<&'a Package> {
    packages
        .iter()
        .filter(|p| {
            p.path == "."
                || rel_path.starts_with(&format!("{}/", p.path))
                || rel_path == p.path
        })
        .max_by_key(|p| p.path.len())
}

fn build_packages(
    root: &Path,
    manifest_dirs: &BTreeSet<PathBuf>,
    files: &[ScannedFile],
) -> Result<Vec<Package>> {
    let mut packages: Vec<Package> = Vec::new();
    let mut seen_names: HashSet<String> = HashSet::new();

    let mut dirs: Vec<PathBuf> = manifest_dirs.iter().cloned().collect();
    dirs.sort_by_key(|d| d.components().count());

    for dir in dirs {
        let rel_dir = normalize(dir.strip_prefix(root).unwrap_or(&dir));
        let rel_dir = if rel_dir.is_empty() {
            ".".to_string()
        } else {
            rel_dir
        };

        for (manifest_name, lang) in MANIFESTS {
            let manifest_path = dir.join(manifest_name);
            if !manifest_path.is_file() {
                continue;
            }
            if *lang == "rust" && is_virtual_cargo_manifest(&manifest_path) {
                continue;
            }
            let parsed = parse_manifest(&manifest_path, lang)?;
            let base_name = parsed
                .name
                .clone()
                .unwrap_or_else(|| default_package_name(&rel_dir));
            let name = if seen_names.contains(&base_name) {
                format!("{}#{}", base_name, rel_dir)
            } else {
                base_name.clone()
            };
            seen_names.insert(name.clone());

            let scope_files: Vec<&ScannedFile> = files
                .iter()
                .filter(|f| package_for_path_raw(&rel_dir, &f.path))
                .collect();
            let lines_of_code = scope_files
                .iter()
                .filter(|f| f.role != FileRole::Config)
                .map(|f| f.size as usize / 32)
                .sum();

            let framework = detect_framework(&parsed.external_deps, lang);
            let entry_points = scope_files
                .iter()
                .filter(|f| f.role == FileRole::EntryPoint)
                .map(|f| f.path.clone())
                .take(10)
                .collect();

            packages.push(Package {
                name,
                path: rel_dir.clone(),
                language: (*lang).to_string(),
                framework,
                manifest: normalize(manifest_path.strip_prefix(root).unwrap_or(&manifest_path)),
                entry_points,
                deps: parsed.deps,
                external_deps: parsed.external_deps,
                file_count: scope_files.len(),
                lines_of_code,
            });
            break;
        }
    }

    if packages.is_empty() {
        packages.push(Package {
            name: default_package_name("."),
            path: ".".to_string(),
            language: files
                .first()
                .and_then(|f| f.language.clone())
                .unwrap_or_else(|| "unknown".to_string()),
            framework: None,
            manifest: "<none>".to_string(),
            entry_points: files
                .iter()
                .filter(|f| f.role == FileRole::EntryPoint)
                .map(|f| f.path.clone())
                .take(10)
                .collect(),
            deps: Vec::new(),
            external_deps: Vec::new(),
            file_count: files.len(),
            lines_of_code: files.iter().map(|f| f.size as usize / 32).sum(),
        });
    }

    Ok(packages)
}

fn package_for_path_raw(pkg_path: &str, rel_path: &str) -> bool {
    pkg_path == "." || rel_path.starts_with(&format!("{}/", pkg_path)) || rel_path == pkg_path
}

fn default_package_name(rel_dir: &str) -> String {
    if rel_dir == "." {
        return "root".to_string();
    }
    Path::new(rel_dir)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "root".to_string())
}

struct ParsedManifest {
    name: Option<String>,
    deps: Vec<String>,
    external_deps: Vec<String>,
}

fn is_virtual_cargo_manifest(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(value) = toml::from_str::<toml::Value>(&text) else {
        return false;
    };
    value.get("package").is_none() && value.get("workspace").is_some()
}

fn parse_manifest(path: &Path, lang: &str) -> Result<ParsedManifest> {
    match lang {
        "node" => parse_package_json(path),
        "rust" => parse_cargo_toml(path),
        "go" => parse_go_mod(path),
        "python" => parse_pyproject(path),
        "java" => parse_java_manifest(path),
        _ => Ok(ParsedManifest {
            name: None,
            deps: Vec::new(),
            external_deps: Vec::new(),
        }),
    }
}

fn parse_package_json(path: &Path) -> Result<ParsedManifest> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let value: Value = serde_json::from_str(&text)
        .with_context(|| format!("parsing {}", path.display()))?;
    let mut deps = Vec::new();
    let mut external = Vec::new();
    for key in ["dependencies", "devDependencies", "peerDependencies"] {
        if let Some(map) = value.get(key).and_then(|v| v.as_object()) {
            for (name, _) in map {
                if is_builtin_js(name) {
                    continue;
                }
                external.push(name.clone());
            }
        }
    }
    if let Some(ws) = value.get("workspaces") {
        match ws {
            Value::Array(items) => {
                for item in items {
                    if let Some(s) = item.as_str() {
                        deps.push(s.to_string());
                    }
                }
            }
            Value::Object(map) => {
                if let Some(Value::Array(items)) = map.get("packages") {
                    for item in items {
                        if let Some(s) = item.as_str() {
                            deps.push(s.to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    deps.sort();
    deps.dedup();
    external.sort();
    external.dedup();
    Ok(ParsedManifest {
        name: value
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        deps,
        external_deps: external,
    })
}

fn is_builtin_js(name: &str) -> bool {
    matches!(
        name,
        "react"
            | "react-dom"
            | "next"
            | "vue"
            | "express"
            | "fastify"
            | "typescript"
            | "jest"
            | "vitest"
    )
}

fn parse_cargo_toml(path: &Path) -> Result<ParsedManifest> {
    let text = std::fs::read_to_string(path)?;
    let value: toml::Value = toml::from_str(&text)
        .with_context(|| format!("parsing {}", path.display()))?;
    let name = value
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .map(|s| s.to_string());
    let mut deps = Vec::new();
    let mut external = Vec::new();
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(map) = value.get(section).and_then(|v| v.as_table()) {
            for (dep_name, spec) in map {
                let is_path = spec
                    .get("path")
                    .and_then(|p| p.as_str())
                    .map(|p| !p.is_empty())
                    .unwrap_or(false)
                    || spec
                        .get("workspace")
                        .and_then(|p| p.as_bool())
                        .unwrap_or(false);
                if is_path {
                    deps.push(dep_name.clone());
                } else {
                    external.push(dep_name.clone());
                }
            }
        }
    }
    if let Some(ws) = value.get("workspace").and_then(|w| w.as_table()) {
        if let Some(members) = ws.get("members").and_then(|m| m.as_array()) {
            for m in members {
                if let Some(s) = m.as_str() {
                    deps.push(s.to_string());
                }
            }
        }
    }
    deps.sort();
    deps.dedup();
    external.sort();
    external.dedup();
    Ok(ParsedManifest {
        name,
        deps,
        external_deps: external,
    })
}

fn parse_go_mod(path: &Path) -> Result<ParsedManifest> {
    let text = std::fs::read_to_string(path)?;
    let mut name = None;
    let mut deps = Vec::new();
    let mut external = Vec::new();
    let module_re = regex::Regex::new(r"(?m)^module\s+(\S+)")?;
    let require_re = regex::Regex::new(r"(?m)^\s*(?:require\s+)?([\w\.\-/~]+\.[\w\.\-/~]+)\s+v")?;
    let replace_re = regex::Regex::new(r"(?m)replace\s+(\S+)\s*=>\s*(\.\S*)")?;
    if let Some(cap) = module_re.captures(&text) {
        name = Some(cap[1].to_string());
    }
    for cap in replace_re.captures_iter(&text) {
        if cap[2].starts_with('.') {
            deps.push(cap[1].to_string());
        } else {
            external.push(cap[1].to_string());
        }
    }
    for cap in require_re.captures_iter(&text) {
        external.push(cap[1].to_string());
    }
    deps.sort();
    deps.dedup();
    external.sort();
    external.dedup();
    Ok(ParsedManifest {
        name,
        deps,
        external_deps: external,
    })
}

fn parse_pyproject(path: &Path) -> Result<ParsedManifest> {
    let text = std::fs::read_to_string(path)?;
    let mut name = None;
    let mut external = Vec::new();
    if let Ok(value) = toml::from_str::<toml::Value>(&text) {
        name = value
            .get("project")
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                value
                    .get("tool")
                    .and_then(|t| t.get("poetry"))
                    .and_then(|p| p.get("name"))
                    .and_then(|n| n.as_str())
                    .map(|s| s.to_string())
            });
        for key in ["dependencies", "optional-dependencies"] {
            if let Some(arr) = value.get("project").and_then(|p| p.get(key)).and_then(|d| d.as_array())
            {
                for item in arr {
                    if let Some(s) = item.as_str() {
                        let cleaned = s
                            .split(['[', '<', '>', '=', ';', ' '])
                            .next()
                            .unwrap_or(s)
                            .to_string();
                        if !cleaned.is_empty() {
                            external.push(cleaned);
                        }
                    }
                }
            }
        }
    }
    external.sort();
    external.dedup();
    Ok(ParsedManifest {
        name,
        deps: Vec::new(),
        external_deps: external,
    })
}

fn parse_java_manifest(path: &Path) -> Result<ParsedManifest> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut name = None;
    let mut external = Vec::new();
    if path.extension().map(|e| e == "xml").unwrap_or(false) {
        let re = regex::Regex::new(r"(?s)<artifactId>([^<]+)</artifactId>")?;
        if let Some(cap) = re.captures(&text) {
            name = Some(cap[1].to_string());
        }
        let dep_re = regex::Regex::new(r"(?s)<dependency>(.*?)</dependency>")?;
        for cap in dep_re.captures_iter(&text) {
            let inner = &cap[1];
            if let Some(a) = re.captures(inner) {
                external.push(a[1].to_string());
            }
        }
    } else {
        let re = regex::Regex::new(r#"(?m)^\s*implementation\s+["']([^"':]+)"#)?;
        for cap in re.captures_iter(&text) {
            external.push(cap[1].to_string());
        }
        let name_re = regex::Regex::new(r#"(?m)rootProject\.name\s*=\s*["']([^"']+)"#)?;
        if let Some(cap) = name_re.captures(&text) {
            name = Some(cap[1].to_string());
        }
    }
    external.sort();
    external.dedup();
    Ok(ParsedManifest {
        name,
        deps: Vec::new(),
        external_deps: external,
    })
}

fn detect_framework(external: &[String], lang: &str) -> Option<String> {
    let has = |needle: &str| external.iter().any(|d| d == needle);
    let has_prefix = |p: &str| external.iter().any(|d| d.starts_with(p));
    let found = match lang {
        "node" => {
            if has_prefix("@nestjs/") {
                Some("nestjs")
            } else if has("next") {
                Some("next.js")
            } else if has("nuxt") {
                Some("nuxt")
            } else if has("@remix-run/react") || has("@remix-run/node") {
                Some("remix")
            } else if has("@trpc/server") || has("@trpc/client") {
                Some("trpc")
            } else if has("express") {
                Some("express")
            } else if has("fastify") {
                Some("fastify")
            } else if has("@angular/core") {
                Some("angular")
            } else if has("svelte") {
                Some("svelte")
            } else if has("vue") {
                Some("vue")
            } else if has("react") || has("react-dom") {
                Some("react")
            } else if has("electron") {
                Some("electron")
            } else if has("astro") {
                Some("astro")
            } else {
                None
            }
        }
        "python" => {
            if has("django") || has_prefix("django-") {
                Some("django")
            } else if has("fastapi") {
                Some("fastapi")
            } else if has("flask") {
                Some("flask")
            } else if has("starlette") {
                Some("starlette")
            } else if has("sqlalchemy") {
                Some("sqlalchemy")
            } else {
                None
            }
        }
        "rust" => {
            if has("axum") {
                Some("axum")
            } else if has("actix-web") {
                Some("actix-web")
            } else if has("rocket") {
                Some("rocket")
            } else if has("tokio") {
                Some("tokio")
            } else if has("tauri") {
                Some("tauri")
            } else {
                None
            }
        }
        "go" => {
            if has("github.com/gin-gonic/gin") {
                Some("gin")
            } else if has("github.com/gofiber/fiber") {
                Some("fiber")
            } else if has("google.golang.org/grpc") {
                Some("grpc")
            } else {
                None
            }
        }
        _ => None,
    };
    found.map(|s| s.to_string())
}

fn normalize_dep_ref(dep: &str) -> String {
    dep.trim_end_matches('/').to_string()
}

fn build_graph(packages: &[Package]) -> Vec<DependencyEdge> {
    let name_index: HashMap<String, usize> = packages
        .iter()
        .enumerate()
        .map(|(i, p)| (p.name.clone(), i))
        .collect();
    let path_index: HashMap<String, usize> = packages
        .iter()
        .enumerate()
        .map(|(i, p)| (normalize_dep_ref(&p.path), i))
        .collect();
    let basename_index: HashMap<String, usize> = packages
        .iter()
        .enumerate()
        .filter(|(i, _)| !packages[*i].path.contains('/'))
        .map(|(i, p)| {
            (
                default_package_name(&p.path),
                i,
            )
        })
        .collect();

    let mut edges: Vec<DependencyEdge> = Vec::new();
    let mut seen: HashSet<(usize, usize)> = HashSet::new();

    for pkg in packages {
        let mut dep_refs: Vec<&String> = pkg.deps.iter().collect();
        for ext in &pkg.external_deps {
            if name_index.contains_key(ext) {
                dep_refs.push(ext);
            }
        }
        for dep in dep_refs {
            let cleaned = normalize_dep_ref(dep);
            if cleaned == "." || cleaned.is_empty() {
                continue;
            }
            let target = name_index
                .get(&cleaned)
                .copied()
                .or_else(|| path_index.get(&cleaned).copied())
                .or_else(|| {
                    cleaned
                        .trim_start_matches("./")
                        .trim_start_matches("../")
                        .split('/')
                        .next_back()
                        .and_then(|b| basename_index.get(b).copied())
                })
                .or_else(|| {
                    let last = cleaned.rsplit('/').next().unwrap_or(&cleaned);
                    name_index
                        .iter()
                        .find(|(n, _)| n.rsplit('/').next() == Some(last))
                        .map(|(_, i)| *i)
                });

            let Some(target_idx) = target else { continue };
            if packages[target_idx].name == pkg.name {
                continue;
            }
            let from_idx = packages
                .iter()
                .position(|p| p.name == pkg.name)
                .unwrap_or(0);
            if seen.insert((from_idx, target_idx)) {
                edges.push(DependencyEdge {
                    from: pkg.name.clone(),
                    to: packages[target_idx].name.clone(),
                    kind: "workspace".to_string(),
                });
            }
        }
    }

    edges.sort_by(|a, b| a.from.cmp(&b.from).then(a.to.cmp(&b.to)));
    edges.dedup_by(|a, b| a.from == b.from && a.to == b.to);
    edges
}

pub fn detect_cycles(manifest: &RepoManifest) -> Vec<Vec<String>> {
    let mut cycles: Vec<Vec<String>> = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut stack: Vec<String> = Vec::new();
    let names: Vec<String> = manifest.package_names().iter().map(|s| s.to_string()).collect();
    for name in names {
        if !visited.contains(&name) {
            let mut path_set: HashSet<String> = HashSet::new();
            dfs_cycle(
                &name,
                manifest,
                &mut visited,
                &mut stack,
                &mut path_set,
                &mut cycles,
            );
        }
    }
    cycles
}

fn dfs_cycle(
    node: &str,
    manifest: &RepoManifest,
    visited: &mut HashSet<String>,
    stack: &mut Vec<String>,
    path_set: &mut HashSet<String>,
    cycles: &mut Vec<Vec<String>>,
) {
    visited.insert(node.to_string());
    path_set.insert(node.to_string());
    stack.push(node.to_string());

    for dep in manifest.direct_dependencies(node) {
        if path_set.contains(dep) {
            if let Some(pos) = stack.iter().position(|n| n == dep) {
                cycles.push(stack[pos..].to_vec());
            }
        } else if !visited.contains(dep) {
            dfs_cycle(dep, manifest, visited, stack, path_set, cycles);
        }
    }

    stack.pop();
    path_set.remove(node);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_entry_points() {
        assert_eq!(classify_role("apps/web/main.ts"), FileRole::EntryPoint);
        assert_eq!(classify_role("src/index.js"), FileRole::EntryPoint);
        assert_eq!(classify_role("packages/core/lib.rs"), FileRole::EntryPoint);
    }

    #[test]
    fn classifies_shared_types() {
        assert_eq!(classify_role("packages/shared/types.ts"), FileRole::SharedTypes);
        assert_eq!(
            classify_role("packages/api/schemas/user.ts"),
            FileRole::SharedTypes
        );
        assert_eq!(
            classify_role("proto/events.proto"),
            FileRole::SharedTypes
        );
    }

    #[test]
    fn classifies_tests() {
        assert_eq!(classify_role("packages/a/src/foo.test.ts"), FileRole::Test);
        assert_eq!(
            classify_role("packages/a/tests/test_thing.py"),
            FileRole::Test
        );
        assert_eq!(classify_role("packages/a/__tests__/x.js"), FileRole::Test);
    }

    #[test]
    fn detects_javascript_frameworks() {
        assert_eq!(
            detect_framework(&["next".to_string()], "node"),
            Some("next.js".to_string())
        );
        assert_eq!(
            detect_framework(&["fastify".to_string()], "node"),
            Some("fastify".to_string())
        );
        assert_eq!(detect_framework(&[], "node"), None);
    }

    #[test]
    fn parses_go_module_name_and_replaces() {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("mrp-go-{}-{}", std::process::id(), unique));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let modfile = dir.join("go.mod");
        std::fs::write(
            &modfile,
            "module example.com/app\n\ngo 1.22\n\nrequire (\n\tgithub.com/gin-gonic/gin v1.9.1\n\tgolang.org/x/sys v0.15.0\n)\n\nreplace example.com/shared => ./shared\n",
        )
        .unwrap();
        let parsed = parse_go_mod(&modfile).unwrap();
        assert_eq!(parsed.name.as_deref(), Some("example.com/app"));
        assert!(parsed.deps.contains(&"example.com/shared".to_string()));
        assert!(parsed
            .external_deps
            .contains(&"github.com/gin-gonic/gin".to_string()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn normalizes_dependency_graph_edges() {
        let packages = vec![
            Package {
                name: "web".to_string(),
                path: "apps/web".to_string(),
                language: "node".to_string(),
                framework: Some("next.js".to_string()),
                manifest: "apps/web/package.json".to_string(),
                entry_points: vec![],
                deps: vec!["@acme/ui".to_string()],
                external_deps: vec![],
                file_count: 1,
                lines_of_code: 10,
            },
            Package {
                name: "@acme/ui".to_string(),
                path: "packages/ui".to_string(),
                language: "node".to_string(),
                framework: None,
                manifest: "packages/ui/package.json".to_string(),
                entry_points: vec![],
                deps: vec![],
                external_deps: vec![],
                file_count: 1,
                lines_of_code: 5,
            },
        ];
        let graph = build_graph(&packages);
        assert_eq!(graph.len(), 1);
        assert_eq!(graph[0].from, "web");
        assert_eq!(graph[0].to, "@acme/ui");
    }
}