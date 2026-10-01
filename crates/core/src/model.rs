use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DetailLevel {
    Full,
    Codemap,
    Slice,
    Drop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileRole {
    EntryPoint,
    SharedTypes,
    Implementation,
    Test,
    Config,
    Docs,
}

impl FileRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            FileRole::EntryPoint => "entry_point",
            FileRole::SharedTypes => "shared_types",
            FileRole::Implementation => "implementation",
            FileRole::Test => "test",
            FileRole::Config => "config",
            FileRole::Docs => "docs",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Package {
    pub name: String,
    pub path: String,
    pub language: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub framework: Option<String>,
    pub manifest: String,
    #[serde(default)]
    pub entry_points: Vec<String>,
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default)]
    pub external_deps: Vec<String>,
    #[serde(default)]
    pub file_count: usize,
    #[serde(default)]
    pub lines_of_code: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyEdge {
    pub from: String,
    pub to: String,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoManifest {
    pub root: String,
    pub generated_by: String,
    #[serde(default)]
    pub languages: BTreeMap<String, usize>,
    pub packages: Vec<Package>,
    pub dependency_graph: Vec<DependencyEdge>,
    #[serde(default)]
    pub entry_points: Vec<String>,
    #[serde(default)]
    pub shared_contracts: Vec<String>,
}

impl RepoManifest {
    pub fn package(&self, name: &str) -> Option<&Package> {
        self.packages.iter().find(|p| p.name == name)
    }

    pub fn package_for_path(&self, rel_path: &str) -> Option<&Package> {
        self.packages
            .iter()
            .filter(|p| {
                p.path == "."
                    || rel_path.starts_with(&format!("{}/", p.path))
                    || rel_path == p.path
            })
            .max_by_key(|p| p.path.len())
    }

    pub fn package_names(&self) -> Vec<&str> {
        self.packages.iter().map(|p| p.name.as_str()).collect()
    }

    pub fn direct_dependents(&self, name: &str) -> Vec<&str> {
        self.dependency_graph
            .iter()
            .filter(|e| e.to == name)
            .map(|e| e.from.as_str())
            .collect()
    }

    pub fn direct_dependencies(&self, name: &str) -> Vec<&str> {
        self.dependency_graph
            .iter()
            .filter(|e| e.from == name)
            .map(|e| e.to.as_str())
            .collect()
    }

    pub fn transitive_closure(&self, root: &str) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        let mut stack = vec![root.to_string()];
        while let Some(current) = stack.pop() {
            for dep in self.direct_dependencies(&current) {
                if !seen.iter().any(|s| s == dep) {
                    seen.push(dep.to_string());
                    stack.push(dep.to_string());
                }
            }
        }
        seen
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectedFile {
    pub path: String,
    pub package: String,
    pub role: FileRole,
    pub detail_level: DetailLevel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<(usize, usize)>,
    pub score: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl SelectedFile {
    pub fn downgrade(&mut self) -> bool {
        match self.detail_level {
            DetailLevel::Full => {
                self.detail_level = DetailLevel::Codemap;
                true
            }
            DetailLevel::Codemap => {
                self.detail_level = DetailLevel::Slice;
                self.lines = Some(self.lines.unwrap_or((1, 80)));
                true
            }
            DetailLevel::Slice => {
                self.detail_level = DetailLevel::Drop;
                true
            }
            DetailLevel::Drop => false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StateBlock {
    #[serde(default)]
    pub files_read: Vec<String>,
    #[serde(default)]
    pub open_questions: Vec<String>,
    #[serde(default)]
    pub next_files_to_read: Vec<String>,
    #[serde(default)]
    pub phase: usize,
    #[serde(default)]
    pub notes: Option<String>,
}

impl StateBlock {
    pub fn has_read(&self, path: &str) -> bool {
        self.files_read.iter().any(|p| p == path)
    }
}