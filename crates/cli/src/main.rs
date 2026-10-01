use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use monorepoprompt_core as core;
use monorepoprompt_core::assemble::synthetic_state;
use monorepoprompt_core::model::StateBlock;
use std::io::{Read, Write};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "monorepoprompt",
    version,
    about = "Layered phased context engine for monorepos",
    long_about = "Builds a surgical, layered prompt from a monorepo and a natural-language task.\nStages: scan -> architecture map -> file selection -> prompt assembly -> state loop."
)]
struct Cli {
    #[command(flatten)]
    global: GlobalArgs,
    #[command(subcommand)]
    command: Command,
}

#[derive(Args, Clone)]
struct GlobalArgs {
    /// Override token budget (defaults to config or 60000).
    #[arg(long, global = true)]
    budget: Option<usize>,
    /// Role prompt placed in the ROLE section.
    #[arg(long, global = true)]
    role: Option<String>,
    /// Path to monorepoprompt.yaml (default: search upward from <path>).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Suppress progress info on stderr.
    #[arg(long, global = true, short = 'q')]
    quiet: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Stage 1 only: emit repo_manifest.json.
    Scan {
        /// Repository root.
        path: PathBuf,
        /// Write JSON to this file instead of stdout.
        #[arg(long, short = 'o')]
        output: Option<PathBuf>,
    },
    /// Stages 1-2: emit architecture_map.md.
    Map {
        /// Repository root.
        path: PathBuf,
        /// Write Markdown to this file instead of stdout.
        #[arg(long, short = 'o')]
        output: Option<PathBuf>,
        /// Also emit the Mermaid DAG to stderr.
        #[arg(long)]
        mermaid: bool,
    },
    /// Full pipeline: select, assemble, emit prompt.
    Build {
        /// Repository root.
        path: PathBuf,
        /// Natural-language task description.
        #[arg(long, short = 't')]
        task: String,
        /// Write the prompt to this file instead of stdout.
        #[arg(long, short = 'o')]
        output: Option<PathBuf>,
        /// Copy the prompt to the clipboard.
        #[arg(long)]
        clipboard: bool,
        /// Emit selected_files.json alongside the prompt.
        #[arg(long)]
        selection: Option<PathBuf>,
        /// Run the interactive state loop (reads LLM replies from stdin).
        #[arg(long, short = 'i')]
        interactive: bool,
        /// Number of phases for the interactive loop.
        #[arg(long, default_value_t = 3)]
        phases: usize,
        /// Print per-section token breakdown to stderr.
        #[arg(long)]
        explain: bool,
    },
    /// Parse a model response's STATE block and emit the next prompt.
    Next {
        /// Repository root.
        path: PathBuf,
        #[arg(long, short = 't')]
        task: String,
        /// Prior STATE JSON (file or '-' for stdin).
        #[arg(long)]
        state: Option<PathBuf>,
        /// Model response containing a STATE block (file or '-' for stdin).
        #[arg(long)]
        response: Option<PathBuf>,
        /// Write the next prompt to this file instead of stdout.
        #[arg(long, short = 'o')]
        output: Option<PathBuf>,
    },
    /// Start the MCP stdio server.
    Mcp {
        /// Repository root to operate on.
        path: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Scan { path, output } => cmd_scan(&cli.global, path, output),
        Command::Map {
            path,
            output,
            mermaid,
        } => cmd_map(&cli.global, path, output, mermaid),
        Command::Build {
            path,
            task,
            output,
            clipboard,
            selection,
            interactive,
            phases,
            explain,
        } => cmd_build(
            &cli.global,
            path,
            task,
            output,
            clipboard,
            selection,
            interactive,
            phases,
            explain,
        ),
        Command::Next {
            path,
            task,
            state,
            response,
            output,
        } => cmd_next(&cli.global, path, task, state, response, output),
        Command::Mcp { path } => cmd_mcp(&cli.global, path),
    }
}

fn load_config(global: &GlobalArgs, root: &std::path::Path) -> Result<core::Config> {
    let mut config = match &global.config {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("reading config {}: {e}", path.display()))?;
            serde_yaml_parse(&text)?
        }
        None => core::Config::load(root)?,
    };
    if let Some(budget) = global.budget {
        config.budget_tokens = budget;
    }
    if let Some(role) = &global.role {
        config.role = role.clone();
    }
    Ok(config)
}

fn serde_yaml_parse(text: &str) -> Result<core::Config> {
    core::Config::parse_yaml(text)
}

fn info(global: &GlobalArgs, message: &str) {
    if !global.quiet {
        eprintln!("[monorepoprompt] {message}");
    }
}

fn cmd_scan(global: &GlobalArgs, path: PathBuf, output: Option<PathBuf>) -> Result<()> {
    let config = load_config(global, &path)?;
    let engine = core::Engine::new(&path, config)?;
    let json = serde_json::to_string_pretty(&engine.manifest)?;
    write_out(&json, output.as_deref(), "repo_manifest.json")?;
    info(
        global,
        &format!(
            "scanned {} packages, {} files",
            engine.manifest.packages.len(),
            engine.files.len()
        ),
    );
    Ok(())
}

fn cmd_map(
    global: &GlobalArgs,
    path: PathBuf,
    output: Option<PathBuf>,
    mermaid: bool,
) -> Result<()> {
    let config = load_config(global, &path)?;
    let engine = core::Engine::new(&path, config)?;
    let map = engine.architecture_map();
    if mermaid {
        eprintln!("{}", map.dag_mermaid);
    }
    write_out(&map.markdown, output.as_deref(), "architecture_map.md")?;
    info(global, &format!("architecture map: {} tokens", map.token_count));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_build(
    global: &GlobalArgs,
    path: PathBuf,
    task: String,
    output: Option<PathBuf>,
    clipboard: bool,
    selection: Option<PathBuf>,
    interactive: bool,
    phases: usize,
    explain: bool,
) -> Result<()> {
    let config = load_config(global, &path)?;
    let engine = core::Engine::new(&path, config)?;
    let budget = engine.config.budget_tokens;

    if interactive {
        return interactive_loop(&engine, &task, budget, phases.max(1), output.as_deref());
    }

    let selected = engine.select_files(&task, budget)?;
    let assembled = core::assemble(&engine, &task, &selected, None);

    if explain {
        info(global, "section token breakdown:");
        for (name, tokens) in &assembled.sections {
            eprintln!("  {name:<20} {tokens:>8}");
        }
        if !assembled.dropped.is_empty() {
            eprintln!("  dropped: {}", assembled.dropped.join(", "));
        }
    }

    if let Some(sel) = &selection {
        let rendered = core::assemble::render_selected(&engine.manifest, &selected);
        std::fs::write(sel, rendered)?;
        info(global, &format!("wrote {}", sel.display()));
    }

    write_out(&assembled.prompt, output.as_deref(), "prompt.md")?;

    if clipboard {
        copy_to_clipboard(&assembled.prompt)?;
        info(global, "copied prompt to clipboard");
    }

    info(
        global,
        &format!(
            "prompt: {} tokens / {} budget ({} files)",
            assembled.token_count,
            budget,
            selected.len()
        ),
    );
    Ok(())
}

fn interactive_loop(
    engine: &core::Engine,
    task: &str,
    budget: usize,
    phases: usize,
    output: Option<&std::path::Path>,
) -> Result<()> {
    let selected = engine.select_files(task, budget)?;
    let mut state = synthetic_state(engine, &selected, task);
    let assembled = core::assemble(engine, task, &selected, None);
    emit_phase(1, phases, &assembled.prompt, output)?;

    let stdin = std::io::stdin();
    for phase in 2..=phases {
        eprintln!("[monorepoprompt] paste model response for phase {} (empty line to stop)", phase - 1);
        let mut buffer = String::new();
        if stdin.read_line(&mut buffer)? == 0 {
            break;
        }
        if buffer.trim().is_empty() {
            break;
        }
        match core::parse_state(&buffer) {
            Ok(parsed) => {
                state = core::merge_state(&state, &parsed);
            }
            Err(_) => {
                eprintln!("[monorepoprompt] no STATE block found; using prior state");
            }
        }
        let (next_prompt, next_selected, next_state) =
            engine.next_phase(task, &state, &buffer, budget)?;
        state = next_state;
        eprintln!(
            "[monorepoprompt] phase {} pulls {} files (state: {} read so far)",
            phase,
            next_selected.len(),
            state.files_read.len()
        );
        emit_phase(phase, phases, &next_prompt, output)?;
    }
    Ok(())
}

fn emit_phase(
    phase: usize,
    phases: usize,
    prompt: &str,
    output: Option<&std::path::Path>,
) -> Result<()> {
    match output {
        Some(path) => {
            let target = if phases > 1 {
                let stem = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "prompt".to_string());
                let ext = path
                    .extension()
                    .map(|e| e.to_string_lossy().to_string())
                    .unwrap_or_else(|| "md".to_string());
                path.with_file_name(format!("{stem}.phase{phase}.{ext}"))
            } else {
                path.to_path_buf()
            };
            std::fs::write(&target, prompt)?;
            eprintln!("[monorepoprompt] phase {phase}/{phases} -> {}", target.display());
        }
        None => {
            println!("\n===== PHASE {phase}/{phases} =====\n");
            println!("{prompt}");
        }
    }
    Ok(())
}

fn cmd_next(
    global: &GlobalArgs,
    path: PathBuf,
    task: String,
    state: Option<PathBuf>,
    response: Option<PathBuf>,
    output: Option<PathBuf>,
) -> Result<()> {
    let config = load_config(global, &path)?;
    let engine = core::Engine::new(&path, config)?;
    let budget = engine.config.budget_tokens;

    let prior: StateBlock = match &state {
        Some(p) => serde_json::from_str(&read_input(p)?).unwrap_or_default(),
        None => {
            let selected = engine.select_files(&task, budget)?;
            synthetic_state(&engine, &selected, &task)
        }
    };

    let response_text = match &response {
        Some(p) => read_input(p)?,
        None => {
            let mut buffer = String::new();
            std::io::stdin().read_to_string(&mut buffer)?;
            buffer
        }
    };

    let (prompt, selected, merged) = engine.next_phase(&task, &prior, &response_text, budget)?;
    info(
        global,
        &format!(
            "phase {} -> {} files ({} already read)",
            merged.phase,
            selected.len(),
            merged.files_read.len()
        ),
    );
    write_out(&prompt, output.as_deref(), "prompt.phase.next.md")?;
    Ok(())
}

fn cmd_mcp(global: &GlobalArgs, path: PathBuf) -> Result<()> {
    let config = load_config(global, &path)?;
    let engine = core::Engine::new(&path, config)?;
    info(global, "MCP stdio server ready");
    mcp_server::serve(engine)
}

fn read_input(path: &std::path::Path) -> Result<String> {
    if path == std::path::Path::new("-") {
        let mut buffer = String::new();
        std::io::stdin().read_to_string(&mut buffer)?;
        return Ok(buffer);
    }
    std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))
}

fn write_out(content: &str, output: Option<&std::path::Path>, default_name: &str) -> Result<()> {
    match output {
        Some(path) => {
            std::fs::write(path, content)?;
            eprintln!("[monorepoprompt] wrote {}", path.display());
        }
        None => {
            let mut stdout = std::io::stdout();
            stdout.write_all(content.as_bytes())?;
            if !content.ends_with('\n') {
                stdout.write_all(b"\n")?;
            }
            let _ = default_name;
        }
    }
    Ok(())
}

#[cfg(windows)]
fn copy_to_clipboard(text: &str) -> Result<()> {
    let clip = crate::clipboard::Clipboard::new();
    clip.set(text)
        .map_err(|e| anyhow::anyhow!("clipboard failed: {e}"))
}

#[cfg(not(windows))]
fn copy_to_clipboard(_text: &str) -> Result<()> {
    anyhow::bail!("--clipboard is only supported on Windows in v1")
}

#[cfg(windows)]
mod clipboard {
    use anyhow::Result;
    use std::ptr;

    #[link(name = "user32")]
    extern "system" {
        fn OpenClipboard(hwnd: *mut std::ffi::c_void) -> i32;
        fn CloseClipboard() -> i32;
        fn EmptyClipboard() -> i32;
        fn SetClipboardData(format: u32, data: *mut std::ffi::c_void) -> i32;
    }

    const CF_UNICODETEXT: u32 = 13;
    const GMEM_MOVEABLE: u32 = 0x0002;

    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalAlloc(flags: u32, bytes: usize) -> *mut std::ffi::c_void;
        fn GlobalLock(mem: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
        fn GlobalUnlock(mem: *mut std::ffi::c_void) -> i32;
    }

    pub struct Clipboard;

    impl Clipboard {
        pub fn new() -> Self {
            Clipboard
        }

        pub fn set(&self, text: &str) -> Result<()> {
            let utf16: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            let bytes = utf16.len() * std::mem::size_of::<u16>();
            unsafe {
                if OpenClipboard(ptr::null_mut()) == 0 {
                    anyhow::bail!("could not open clipboard");
                }
                let handle = GlobalAlloc(GMEM_MOVEABLE, bytes);
                if handle.is_null() {
                    CloseClipboard();
                    anyhow::bail!("clipboard allocation failed");
                }
                let target = GlobalLock(handle);
                if target.is_null() {
                    CloseClipboard();
                    anyhow::bail!("clipboard lock failed");
                }
                ptr::copy_nonoverlapping(
                    utf16.as_ptr() as *const u8,
                    target as *mut u8,
                    bytes,
                );
                GlobalUnlock(handle);
                EmptyClipboard();
                if SetClipboardData(CF_UNICODETEXT, handle) == 0 {
                    CloseClipboard();
                    anyhow::bail!("SetClipboardData failed");
                }
                CloseClipboard();
            }
            Ok(())
        }
    }
}

mod mcp_server;