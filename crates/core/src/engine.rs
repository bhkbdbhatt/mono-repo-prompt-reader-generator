use crate::arch::build_architecture_map;
use crate::codemap::{codemap, first_window, slice_of};
use crate::config::Config;
use crate::model::{
    DetailLevel, FileRole, RepoManifest, SelectedFile, StateBlock,
};
use crate::scan::{scan_repo, ScannedFile};
use crate::tokens::{count_tokens, truncate_to_tokens};
use anyhow::{Context, Result};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

pub struct Engine {
    pub root: PathBuf,
    pub manifest: RepoManifest,
    pub files: Vec<ScannedFile>,
    pub config: Config,
    sent: RefCell<HashSet<String>>,
}

impl Engine {
    pub fn new(root: &Path, config: Config) -> Result<Engine> {
        let scan = scan_repo(root)?;
        Ok(Engine {
            root: scan.manifest.root.clone().into(),
            manifest: scan.manifest,
            files: scan.files,
            config,
            sent: RefCell::new(HashSet::new()),
        })
    }

    pub fn from_manifest(root: &Path, manifest: RepoManifest, config: Config) -> Result<Engine> {
        let scan = scan_repo(root)?;
        Ok(Engine {
            root: root.to_path_buf(),
            manifest,
            files: scan.files,
            config,
            sent: RefCell::new(HashSet::new()),
        })
    }

    pub fn record_sent(&self, paths: impl IntoIterator<Item = String>) {
        if let Ok(mut sent) = self.sent.try_borrow_mut() {
            sent.extend(paths);
        }
    }

    pub fn record_sent_selected(&self, selected: &[SelectedFile]) {
        self.record_sent(selected.iter().map(|s| s.path.clone()));
    }

    pub fn forget_sent(&self, paths: impl IntoIterator<Item = String>) {
        if let Ok(mut sent) = self.sent.try_borrow_mut() {
            for path in paths {
                sent.remove(&path);
            }
        }
    }

    pub fn reset_sent(&self) {
        if let Ok(mut sent) = self.sent.try_borrow_mut() {
            sent.clear();
        }
    }

    pub fn architecture_map(&self) -> crate::arch::ArchitectureMap {
        build_architecture_map(&self.manifest)
    }

    pub fn select_files(
        &self,
        task: &str,
        budget: usize,
    ) -> Result<Vec<SelectedFile>> {
        crate::select::select_files(self, task, budget)
    }

    pub fn assemble(
        &self,
        task: &str,
        selected: &[SelectedFile],
        state: Option<&StateBlock>,
    ) -> Result<String> {
        let prompt = crate::assemble::assemble_prompt(self, task, selected, state)?;
        self.record_sent_selected(selected);
        Ok(prompt)
    }

    pub fn sent_paths(&self) -> Vec<String> {
        match self.sent.try_borrow() {
            Ok(sent) => sent.iter().cloned().collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn build(&self, task: &str, budget: usize) -> Result<(String, Vec<SelectedFile>)> {
        let selected = self.select_files(task, budget)?;
        let prompt = self.assemble(task, &selected, None)?;
        Ok((prompt, selected))
    }

    pub fn next_phase(
        &self,
        task: &str,
        prior_state: &StateBlock,
        llm_response: &str,
        budget: usize,
    ) -> Result<(String, Vec<SelectedFile>, StateBlock)> {
        let parsed = crate::state::parse_state(llm_response).unwrap_or_default();
        let merged = crate::state::merge_state(prior_state, &parsed);
        let requested: Vec<String> = merged.next_files_to_read.clone();
        let selected = crate::select::select_for_next_phase(
            self,
            task,
            budget,
            &requested,
            &merged.files_read,
        )?;
        let prompt = self.assemble(task, &selected, Some(&merged))?;
        Ok((prompt, selected, merged))
    }

    pub fn read_file(&self, rel_path: &str) -> Result<String> {
        let abs = self.abs_path(rel_path)?;
        std::fs::read_to_string(&abs)
            .with_context(|| format!("reading {}", abs.display()))
    }

    pub fn abs_path(&self, rel_path: &str) -> Result<PathBuf> {
        let candidate = self.root.join(rel_path);
        let canonical_root = self
            .root
            .canonicalize()
            .unwrap_or_else(|_| self.root.clone());
        let canonical = candidate
            .canonicalize()
            .with_context(|| format!("resolving {}", rel_path))?;
        if !canonical.starts_with(&canonical_root) {
            anyhow::bail!("path escapes repository root: {rel_path}");
        }
        Ok(canonical)
    }

    pub fn render_file(&self, selected: &SelectedFile) -> Result<(String, DetailLevel)> {
        let content = match self.read_file(&selected.path) {
            Ok(c) => c,
            Err(_) => return Ok((String::from("// unreadable"), DetailLevel::Drop)),
        };
        match selected.detail_level {
            DetailLevel::Full => Ok((content, DetailLevel::Full)),
            DetailLevel::Codemap => Ok((
                codemap(&selected.path, &content, 120),
                DetailLevel::Codemap,
            )),
            DetailLevel::Slice => {
                let (start, end) = match selected.lines {
                    Some((s, e)) => (s, e),
                    None => first_window(&content, 1_200),
                };
                Ok((slice_of(&selected.path, &content, start, end), DetailLevel::Slice))
            }
            DetailLevel::Drop => Ok((String::new(), DetailLevel::Drop)),
        }
    }
}

pub fn default_detail_for(role: FileRole, config: &Config) -> DetailLevel {
    let raw = match role {
        FileRole::EntryPoint => &config.detail_defaults.entry_points,
        FileRole::SharedTypes => &config.detail_defaults.shared_types,
        FileRole::Implementation => &config.detail_defaults.implementation,
        FileRole::Test => &config.detail_defaults.tests,
        FileRole::Config => "slice",
        FileRole::Docs => "slice",
    };
    match raw.trim() {
        "full" => DetailLevel::Full,
        "codemap" => DetailLevel::Codemap,
        "slice" => DetailLevel::Slice,
        _ => DetailLevel::Codemap,
    }
}

pub fn token_budget_report(parts: &BTreeMap<String, usize>, budget: usize) -> bool {
    parts.values().sum::<usize>() <= budget
}

pub fn dedupe_preserve(paths: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for p in paths {
        if seen.insert(p.clone()) {
            out.push(p);
        }
    }
    out
}

pub fn clip(text: &str, max_tokens: usize) -> String {
    truncate_to_tokens(text, max_tokens)
}

pub fn tokens(text: &str) -> usize {
    count_tokens(text)
}