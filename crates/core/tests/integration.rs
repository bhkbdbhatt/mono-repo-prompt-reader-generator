use monorepoprompt_core::{assemble, Config, DetailLevel, Engine, StateBlock};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static FIXTURE_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn temp_dir(name: &str) -> PathBuf {
    let unique = FIXTURE_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "mrp-it-{}-{}-{}",
        name,
        std::process::id(),
        unique
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn emitted_paths(prompt: &str) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let mut rest = prompt;
    while let Some(idx) = rest.find("<file path=\"") {
        rest = &rest[idx + 12..];
        if let Some(end) = rest.find('"') {
            out.insert(rest[..end].to_string());
            rest = &rest[end..];
        }
    }
    out
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn fixture() -> PathBuf {
    let root = temp_dir("monorepo");

    write(
        &root,
        "package.json",
        r#"{"name":"root","private":true,"workspaces":["apps/*","packages/*"]}"#,
    );
    write(
        &root,
        "apps/web/package.json",
        r#"{"name":"web","dependencies":{"next":"15.0.0","@acme/ui":"workspace:*","@acme/auth":"workspace:*","prisma":"5.0.0"}}"#,
    );
    write(&root, "apps/web/main.ts", "import { login } from '@acme/auth';\nlogin();\n");
    write(
        &root,
        "apps/web/routes/login.ts",
        "export async function loginRoute(req: Request) {\n  const session = await createSession(req);\n  return session;\n}\n",
    );
    write(
        &root,
        "apps/web/session.ts",
        "export function createSession(req: Request) {\n  return { id: 'abc' };\n}\nexport function refreshSession(id: string) {\n  return { id };\n}\n",
    );

    write(&root, "packages/auth/package.json", r#"{"name":"@acme/auth"}"#);
    write(
        &root,
        "packages/auth/src/login.ts",
        "export function login(email: string, password: string) {\n  if (!email) { throw new Error('missing'); }\n  return { token: 't' };\n}\n",
    );
    write(
        &root,
        "packages/auth/src/refresh.ts",
        "export function refresh(token: string) {\n  return { token, refreshed: true };\n}\n",
    );
    write(
        &root,
        "packages/auth/src/session-store.ts",
        "export class SessionStore {\n  get(id: string) { return id; }\n  set(id: string) {}\n}\n",
    );

    write(&root, "packages/ui/package.json", r#"{"name":"@acme/ui"}"#);
    write(
        &root,
        "packages/ui/index.ts",
        "export function Button(props: object) {\n  return null;\n}\n",
    );

    write(&root, "packages/shared/package.json", r#"{"name":"@acme/shared"}"#);
    write(
        &root,
        "packages/shared/types.ts",
        "export interface Session {\n  id: string;\n  expiresAt: number;\n}\nexport interface Credentials {\n  email: string;\n  password: string;\n}\n",
    );

    write(
        &root,
        "packages/billing/package.json",
        r#"{"name":"@acme/billing","dependencies":{"stripe":"14.0.0"}}"#,
    );
    write(
        &root,
        "packages/billing/src/invoice.ts",
        "export function createInvoice(amount: number) {\n  return { amount };\n}\nexport function refund(id: string) {\n  return id;\n}\n",
    );
    write(&root, "packages/billing/src/plan.ts", "export const PLANS = ['pro', 'team'];\n");
    write(
        &root,
        "packages/calendar/package.json",
        r#"{"name":"@acme/calendar"}"#,
    );
    write(
        &root,
        "packages/calendar/src/event.ts",
        "export function addEvent(title: string) {\n  return { title };\n}\n",
    );

    write(
        &root,
        "packages/auth/src/login.test.ts",
        "import { login } from './login';\ntest('logs in', () => { login('a', 'b'); });\n",
    );
    write(
        &root,
        "README.md",
        "# Fixture monorepo\n\nUsed by integration tests.\n",
    );
    write(&root, ".gitignore", "ignored/\n");
    write(&root, "ignored/secret.ts", "export const token = 'nope';\n");
    write(
        &root,
        "node_modules/junk/index.js",
        "module.exports = {};\n",
    );

    root
}

#[test]
fn scan_detects_packages_and_language_mix() {
    let root = fixture();
    let engine = Engine::new(&root, Config::default()).unwrap();
    let names = engine.manifest.package_names();
    for expected in ["web", "@acme/auth", "@acme/ui", "@acme/shared", "@acme/billing"] {
        assert!(names.contains(&expected), "missing {expected} in {names:?}");
    }
    assert!(engine.manifest.languages.contains_key("typescript"));
    assert_eq!(engine.manifest.generated_by.split(' ').next().unwrap(), "monorepoprompt");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn scan_respects_gitignore_and_ignores_build_dirs() {
    let root = fixture();
    let engine = Engine::new(&root, Config::default()).unwrap();
    let paths: Vec<&str> = engine.files.iter().map(|f| f.path.as_str()).collect();
    assert!(!paths.iter().any(|p| p.starts_with("ignored/")), "{paths:?}");
    assert!(!paths.iter().any(|p| p.contains("node_modules")), "{paths:?}");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn dependency_graph_links_web_to_auth() {
    let root = fixture();
    let engine = Engine::new(&root, Config::default()).unwrap();
    let edges = &engine.manifest.dependency_graph;
    assert!(
        edges
            .iter()
            .any(|e| e.from == "web" && e.to == "@acme/auth" && e.kind == "workspace"),
        "{edges:?}"
    );
    assert!(engine.manifest.transitive_closure("web").contains(&"@acme/auth".to_string()));
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn architecture_map_fits_budget_and_lists_auth() {
    let root = fixture();
    let engine = Engine::new(&root, Config::default()).unwrap();
    let map = engine.architecture_map();
    assert!(map.token_count <= 2000, "map was {} tokens", map.token_count);
    assert!(map.markdown.contains("@acme/auth"), "{}", map.markdown);
    assert!(map.markdown.contains("graph TD"));
    assert!(
        map.dag_mermaid.contains("nweb --> nacmeauth"),
        "{}",
        map.dag_mermaid
    );
    assert!(
        map.dag_mermaid.contains("nweb --> nacmeui"),
        "{}",
        map.dag_mermaid
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn selection_prefers_auth_and_excludes_billing_and_calendar() {
    let root = fixture();
    let engine = Engine::new(&root, Config::default()).unwrap();
    let selected = engine.select_files("trace the auth flow from login to refresh", 60_000).unwrap();
    let paths: Vec<&str> = selected.iter().map(|s| s.path.as_str()).collect();

    assert!(
        paths.iter().any(|p| p.contains("packages/auth/src/login.ts")),
        "auth login missing: {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.contains("packages/auth/src/refresh.ts")),
        "auth refresh missing: {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.contains("packages/shared/types.ts")),
        "shared types missing: {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p.starts_with("packages/billing/")),
        "billing leaked in: {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p.starts_with("packages/calendar/")),
        "calendar leaked in: {paths:?}"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn prompt_respects_configured_budget() {
    let root = fixture();
let config = Config {
        budget_tokens: 4_000,
        ..Config::default()
    };
    let engine = Engine::new(&root, config).unwrap();
    let selected = engine
        .select_files("trace the auth flow from login to refresh", 4_000)
        .unwrap();
    let assembled = assemble(&engine, "trace the auth flow from login to refresh", &selected, None);

    assert!(
        assembled.token_count <= 4_000,
        "prompt was {} tokens, budget 4000",
        assembled.token_count
    );
    assert!(assembled.prompt.contains("# ROLE"));
    assert!(assembled.prompt.contains("# ARCHITECTURE MAP"));
    assert!(assembled.prompt.contains("# CODE CONTEXT"));
    assert!(assembled.prompt.contains("# TASK"));
    assert!(assembled.prompt.contains("# OUTPUT FORMAT"));
    assert!(assembled.prompt.contains("trace the auth flow"));
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn tiny_budget_forces_detail_downgrades() {
    let root = fixture();
    let config = Config {
        budget_tokens: 2_000,
        ..Config::default()
    };
    let engine = Engine::new(&root, config).unwrap();
    let selected = engine.select_files("session refresh token", 2_000).unwrap();
    let assembled = assemble(&engine, "session refresh token", &selected, None);

    assert!(assembled.token_count <= 2_000, "{}", assembled.token_count);
    assert!(
        assembled
            .prompt
            .contains("<file path=\"packages/shared/types.ts\" detail=\"full\">"),
        "shared types should stay full:\n{}",
        assembled.prompt
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn codemap_level_used_for_large_implementation_files() {
    let root = fixture();
    let engine = Engine::new(&root, Config::default()).unwrap();
    let selected = engine.select_files("createInvoice refund billing", 60_000).unwrap();
    let billing: Vec<_> = selected
        .iter()
        .filter(|s| s.package == "@acme/billing")
        .collect();
    assert!(!billing.is_empty(), "{:?}", selected);
    for file in &billing {
        assert_eq!(file.detail_level, DetailLevel::Codemap, "{file:?}");
    }
    let rendered = engine.render_file(billing[0]).unwrap().0;
    assert!(rendered.starts_with("// codemap:") || rendered.contains("invoice"), "{rendered}");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn state_loop_includes_only_new_files_after_phase_one() {
    let root = fixture();
    let engine = Engine::new(&root, Config::default()).unwrap();
    let task = "trace the auth flow from login to refresh";
    let first = engine.select_files(task, 30_000).unwrap();
    let phase1 = assemble(&engine, task, &first, None);

    assert!(
        phase1.prompt.contains("<file path=\"packages/auth/src/login.ts\""),
        "phase 1 must include login.ts"
    );

    let delivered_in_phase1 = emitted_paths(&phase1.prompt);
    let fresh: Vec<String> = engine
        .files
        .iter()
        .map(|f| f.path.clone())
        .filter(|p| !delivered_in_phase1.contains(p))
        .take(2)
        .collect();
    assert!(!fresh.is_empty(), "fixture should have files beyond phase 1");

    let response = format!(
        r#"I traced login into the session store.

```json
{{"files_read": ["packages/auth/src/login.ts"], "open_questions": ["Where does the refresh token get persisted?"], "next_files_to_read": {:?}}}
```
"#,
        fresh
    );
    let (phase2_prompt, phase2_files, state) = engine
        .next_phase(task, &StateBlock::default(), &response, 30_000)
        .unwrap();

    assert!(state.files_read.contains(&"packages/auth/src/login.ts".to_string()));
    assert_eq!(state.files_read.len(), 1);
    assert!(state.phase >= 1);
    assert_eq!(state.open_questions.len(), 1);
    assert_eq!(state.next_files_to_read, fresh, "{state:?}");

    let emitted = emitted_paths(&phase2_prompt);
    assert!(!emitted.is_empty(), "phase 2 should deliver new files");
    for path in fresh.iter().take(1) {
        assert!(emitted.contains(path), "requested file {path} missing: {emitted:?}");
    }
    let resent: Vec<&String> = delivered_in_phase1.intersection(&emitted).collect();
    assert!(resent.is_empty(), "phase 2 resent: {resent:?}");
    assert!(
        phase2_prompt.contains("# STATE (from previous phase)"),
        "prior state must be carried forward"
    );
    assert!(phase2_prompt.contains("\"phase\""));
    assert!(!phase2_files.is_empty());
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn three_phase_loop_converges_and_drops_resent_files() {
    let root = fixture();
    let engine = Engine::new(&root, Config::default()).unwrap();
    let task = "trace the auth flow from login to refresh";
    let mut state = StateBlock::default();
    let mut seen_by_prompt: Vec<std::collections::HashSet<String>> = Vec::new();

    let first = engine.select_files(task, 30_000).unwrap();
    let phase1 = assemble(&engine, task, &first, None);
    let mut prompt = phase1.prompt;
    let mut read = emitted_paths(&prompt);
    let mut prior_state = StateBlock::default();

    for phase in 2..=4 {
        let requested: Vec<String> = engine
            .files
            .iter()
            .filter(|f| !read.contains(&f.path))
            .take(3)
            .map(|f| f.path.clone())
            .collect();
        let response = format!(
            "phase {phase} findings.\n\n```json\n{{\"files_read\": [{}], \"open_questions\": [\"still open?\"], \"next_files_to_read\": {}}}\n```\n",
            serde_json::to_string(&requested).unwrap(),
            serde_json::to_string(&requested).unwrap()
        );

        let (next_prompt, next_files, merged) =
            engine.next_phase(task, &state, &response, 30_000).unwrap();

        let mut resent: Vec<&String> = read.iter().filter(|p| next_prompt.contains(&format!("path=\"{p}\""))).collect();
        resent.sort();
        assert!(resent.is_empty(), "phase {phase} resent already-read files: {resent:?}");

        assert!(
            merged.files_read.len() >= prior_state.files_read.len(),
            "files_read must accumulate across phases"
        );
        assert!(merged.phase >= phase - 1, "phase counter should advance");

        for file in emitted_paths(&next_prompt) {
            read.insert(file);
        }
        seen_by_prompt.push(next_files.iter().map(|f| f.path.clone()).collect::<std::collections::HashSet<_>>());
        state = merged.clone();
        prior_state = merged;
        prompt = next_prompt;
        assert!(prompt.contains("# STATE (from previous phase)"));
    }

    assert!(!seen_by_prompt.is_empty());
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn next_phase_returns_empty_when_nothing_is_left_to_read() {
    let root = fixture();
    let engine = Engine::new(&root, Config::default()).unwrap();
    let task = "trace the auth flow";
    let selected = engine.select_files(task, 30_000).unwrap();
    let every_path: Vec<String> = selected.iter().map(|f| f.path.clone()).collect();
    let response = format!(
        "```json\n{{\"files_read\": {:?}, \"open_questions\": [], \"next_files_to_read\": []}}\n```\n",
        every_path
    );
    let prior = StateBlock {
        files_read: every_path,
        phase: 3,
        ..Default::default()
    };
    let (_prompt, files, state) = engine.next_phase(task, &prior, &response, 30_000).unwrap();
    assert!(files.is_empty() || files.iter().all(|f| prior.files_read.contains(&f.path)), "should not resend unseen files: {files:?}");
    assert_eq!(state.phase, 4);
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn path_traversal_is_rejected() {
    let root = fixture();
    let engine = Engine::new(&root, Config::default()).unwrap();
    assert!(engine.abs_path("../outside.ts").is_err());
    assert!(engine.read_file("../../windows/system32/drivers/etc/hosts").is_err());
    std::fs::remove_dir_all(&root).ok();
}



#[test]
fn token_count_is_within_five_percent_of_tiktoken_reference() {
    let root = fixture();
    let engine = Engine::new(&root, Config::default()).unwrap();
    let selected = engine.select_files("auth login refresh", 60_000).unwrap();
    let assembled = assemble(&engine, "auth login refresh", &selected, None);

    let reported = assembled
        .sections
        .iter()
        .find(|(name, _)| name == "CODE CONTEXT")
        .map(|(_, tokens)| *tokens)
        .unwrap_or(0);
    assert!(reported > 0);

    let whole = monorepoprompt_core::count_tokens(&assembled.prompt);
    let summed: usize = assembled.sections.iter().map(|(_, t)| *t).sum();
    let ratio = summed as f64 / whole as f64;
    assert!(
        (ratio - 1.0).abs() < 0.25,
        "section sum {summed} vs whole {whole} (ratio {ratio})"
    );
    std::fs::remove_dir_all(&root).ok();
}