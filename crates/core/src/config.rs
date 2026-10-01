use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetailDefaults {
    #[serde(default = "default_entry_points")]
    pub entry_points: String,
    #[serde(default = "default_shared_types")]
    pub shared_types: String,
    #[serde(default = "default_implementation")]
    pub implementation: String,
    #[serde(default = "default_tests")]
    pub tests: String,
}

fn default_entry_points() -> String {
    "full".to_string()
}
fn default_shared_types() -> String {
    "full".to_string()
}
fn default_implementation() -> String {
    "codemap".to_string()
}
fn default_tests() -> String {
    "slice".to_string()
}

impl Default for DetailDefaults {
    fn default() -> Self {
        Self {
            entry_points: default_entry_points(),
            shared_types: default_shared_types(),
            implementation: default_implementation(),
            tests: default_tests(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_role")]
    pub role: String,
    #[serde(default = "default_budget")]
    pub budget_tokens: usize,
    #[serde(default)]
    pub detail_defaults: DetailDefaults,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default = "default_output_format")]
    pub output_format: String,
    #[serde(default = "default_state_block")]
    pub state_block: bool,
    #[serde(default = "default_min_files")]
    pub min_files: usize,
    #[serde(default = "default_max_files")]
    pub max_files: usize,
    #[serde(default = "default_map_tokens")]
    pub map_token_budget: usize,
    #[serde(default = "default_slice_lines")]
    pub slice_lines: usize,
}

fn default_role() -> String {
    "You are a senior architect.".to_string()
}
fn default_budget() -> usize {
    60_000
}
fn default_output_format() -> String {
    "markdown".to_string()
}
fn default_state_block() -> bool {
    true
}
fn default_min_files() -> usize {
    5
}
fn default_max_files() -> usize {
    400
}
fn default_map_tokens() -> usize {
    2000
}
fn default_slice_lines() -> usize {
    80
}

impl Default for Config {
    fn default() -> Self {
        Self {
            role: "You are a senior architect.".to_string(),
            budget_tokens: 60_000,
            detail_defaults: DetailDefaults::default(),
            exclude: default_excludes(),
            output_format: "markdown".to_string(),
            state_block: true,
            min_files: default_min_files(),
            max_files: default_max_files(),
            map_token_budget: default_map_tokens(),
            slice_lines: default_slice_lines(),
        }
    }
}

fn default_excludes() -> Vec<String> {
    vec![
        "**/node_modules/**".to_string(),
        "**/dist/**".to_string(),
        "**/build/**".to_string(),
        "**/target/**".to_string(),
        "**/*.lock".to_string(),
        "**/coverage/**".to_string(),
    ]
}

pub const CONFIG_FILENAME: &str = "monorepoprompt.yaml";

impl Config {
    pub fn parse_yaml(text: &str) -> anyhow::Result<Config> {
        let mut config: Config = serde_yaml::from_str(text)?;
        config.apply_merges()?;
        Ok(config)
    }

    pub fn load(root: &Path) -> anyhow::Result<Config> {
        let path = find_config(root);
        let Some(path) = path else {
            return Ok(Config::default());
        };
        let text = std::fs::read_to_string(&path)?;
        Config::parse_yaml(&text)
    }

    fn apply_merges(&mut self) -> anyhow::Result<()> {
        let defaults = DetailDefaults::default();
        if self.detail_defaults.entry_points.trim().is_empty() {
            self.detail_defaults.entry_points = defaults.entry_points;
        }
        if self.detail_defaults.shared_types.trim().is_empty() {
            self.detail_defaults.shared_types = defaults.shared_types;
        }
        if self.detail_defaults.implementation.trim().is_empty() {
            self.detail_defaults.implementation = defaults.implementation;
        }
        if self.detail_defaults.tests.trim().is_empty() {
            self.detail_defaults.tests = defaults.tests;
        }
        if self.budget_tokens == 0 {
            self.budget_tokens = default_budget();
        }
        if self.role.trim().is_empty() {
            self.role = default_role();
        }
        Ok(())
    }

    pub fn merged_excludes(&self) -> HashSet<String> {
        let mut set: HashSet<String> = HashSet::new();
        for pattern in self.exclude.iter().chain(default_excludes().iter()) {
            set.insert(pattern.clone());
        }
        set
    }
}

pub fn find_config(root: &Path) -> Option<PathBuf> {
    let mut current = Some(root);
    while let Some(dir) = current {
        let candidate = dir.join(CONFIG_FILENAME);
        if candidate.is_file() {
            return Some(candidate);
        }
        current = dir.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

fn temp_dir() -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "mrp-config-test-{}-{}",
        std::process::id(),
        unique
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

    #[test]
    fn defaults_are_sensible() {
        let config = Config::default();
        assert_eq!(config.budget_tokens, 60_000);
        assert_eq!(config.detail_defaults.implementation, "codemap");
        assert!(config.state_block);
    }

    #[test]
    fn reads_yaml_config() {
        let dir = temp_dir();
        let mut file = std::fs::File::create(dir.join(CONFIG_FILENAME)).unwrap();
        writeln!(
            file,
            "role: \"You are a staff engineer.\"\nbudget_tokens: 12000\ndetail_defaults:\n  implementation: slice\noutput_format: markdown\nstate_block: false\nexclude:\n  - \"**/vendor/**\""
        )
        .unwrap();
        let config = Config::load(&dir).unwrap();
        assert_eq!(config.role, "You are a staff engineer.");
        assert_eq!(config.budget_tokens, 12000);
        assert_eq!(config.detail_defaults.implementation, "slice");
        assert_eq!(config.detail_defaults.entry_points, "full");
        assert!(!config.state_block);
        assert!(config.merged_excludes().contains("**/vendor/**"));
        assert!(config.merged_excludes().contains("**/node_modules/**"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_config_returns_default() {
        let dir = temp_dir();
        let config = Config::load(&dir).unwrap();
        assert_eq!(config.budget_tokens, 60_000);
        std::fs::remove_dir_all(&dir).ok();
    }
}