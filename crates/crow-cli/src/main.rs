pub mod chat;
mod config;
pub mod crucible;
pub mod crucible_runner;
mod diff;
pub mod epistemic_ui;
pub mod event;
mod evidence_report;
pub mod mcts;
pub mod prompt;
pub mod render;
pub mod runtime;

pub mod snapshot;
pub mod thread_manager;
pub mod tui;

use anyhow::Result;
use config::CrowConfig;

use crow_probe::scan_workspace;
use std::env;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    let cmd = args.get(1).map(std::string::String::as_str);

    match cmd {
        Some("run") => run_dry_run(&args[2..]).await,
        Some("plan") => run_plan(&args[2..]).await,
        Some("compile") => run_compile_only(&args[2..]).await,
        Some("dry-run") => run_dry_run(&args[2..]).await,
        Some("session") => handle_session_command(&args[2..]).await,

        Some("dream") => run_autodream().await,
        Some("mcp") => handle_mcp_command(&args[2..]).await,

        Some("--help") | Some("-h") | Some("help") => {
            print_help();
            Ok(())
        }
        Some("yolo") => run_yolo(&args[2..]).await,
        Some("chat") => chat::run_repl(&CrowConfig::load()?).await,
        Some(cmd) if cmd == "-r" || cmd == "--resume" => {
            // Drop into Console 4.0 Ratatui Workbench with resume flag
            tui::app::run_workbench(&CrowConfig::load()?, true).await
        }
        Some(unknown) => {
            eprintln!("Unknown command: {unknown}");
            print_help();
            std::process::exit(1);
        }
        None => {
            // Drop into Console 4.0 Ratatui Workbench
            tui::app::run_workbench(&CrowConfig::load()?, false).await
        }
    }
}

fn print_help() {
    eprintln!(
        r#"
🦅 crow — evidence-driven coding agent

USAGE:
    crow <COMMAND> [OPTIONS]

COMMANDS:
    chat                      (or no args) Start the Continuous Chat REPL
    run <prompt>              Full autonomous loop (serial or MCTS)
    plan <prompt>             Compile and preview plan with evidence report
    compile <prompt>          Compile-only: show the IntentPlan JSON
    session list              List saved sessions
    session resume <id>       Resume a saved session
    yolo <prompt>             Fast-path native tool-calling mode (Codex style)

    dream                     Run background AutoDream memory consolidation
    mcp                       Manage MCP tools
    dry-run <prompt>          Alias for 'run'
    help                      Show this help

ENVIRONMENT:
    OPENAI_API_KEY            API key (or CROW_API_KEY)
    LLM_BASE_URL              Provider endpoint
    LLM_MODEL                 Model name
    LLM_PROVIDER              Provider type (openai, custom)
    CROW_WRITE_MODE           sandbox | write | danger (default: write)
    CROW_MCTS_BRANCHES        MCTS branch factor (default: 3)
    CROW_MAP_BUDGET           Repo map size budget in bytes

SAFETY:
    crow defaults to workspace-write mode. All mutations go through
    sandboxed verification before touching your workspace. Failed
    operations leave the workspace untouched (zero-pollution guarantee).
"#
    );
}

async fn handle_session_command(args: &[String]) -> Result<()> {
    let subcmd = args.first().map(std::string::String::as_str);
    match subcmd {
        Some("list") => {
            let store = crow_runtime::session::SessionStore::open()?;
            let sessions = store.list()?;
            if sessions.is_empty() {
                println!("No saved sessions.");
            } else {
                println!(
                    "  ID       │ Task                                     │ Snapshots │ Updated"
                );
                println!(
                    "  ─────────┼──────────────────────────────────────────┼───────────┼────────"
                );
                for s in &sessions {
                    println!("{s}");
                }
            }
            Ok(())
        }
        Some("resume") => {
            let id = args
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("Usage: crow session resume <session-id>"))?;
            println!("  (use `crow session resume-run <id>` to actually continue execution)");
            let store = crow_runtime::session::SessionStore::open()?;
            let session = store.load(&crow_runtime::session::SessionId(id.clone()))?;
            println!("Resuming session: {}", session.id.0);
            println!("  Workspace: {}", session.workspace_root.display());
            println!("  Task: {}", session.task);
            println!("  Messages: {}", session.restore_messages().len());
            println!("  Snapshots: {}", session.snapshot_timeline.len());
            Ok(())
        }
        Some("resume-run") => {
            let id = args
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("Usage: crow session resume-run <session-id>"))?;
            // Delegate to async resume
            resume_session_run(id).await
        }
        _ => {
            eprintln!("Usage: crow session <list|resume|resume-run>");
            Ok(())
        }
    }
}

async fn resume_session_run(session_id: &str) -> Result<()> {
    let cfg = CrowConfig::load()?;
    let runtime = crate::runtime::SessionRuntime::boot(&cfg).await?;
    runtime.resume(&cfg, session_id).await
}

// ─── Compile-Only command ───────────────────────────────────────────

async fn run_compile_only(args: &[String]) -> Result<()> {
    let prompt = args.join(" ");
    let cfg = CrowConfig::load()?;
    let runtime = crate::runtime::SessionRuntime::boot(&cfg).await?;
    runtime.compile_only(&cfg, &prompt).await
}

// ─── Plan command (Evidence-First Preview) ──────────────────────────

/// `crow plan <prompt>` — compile a plan and display a full evidence report
/// WITHOUT applying changes. This serves as a dry-run for verification tests.
async fn run_plan(args: &[String]) -> Result<()> {
    let prompt = args.join(" ");
    let cfg = CrowConfig::load()?;
    let runtime = crate::runtime::SessionRuntime::boot(&cfg).await?;
    runtime.generate_plan(&cfg, &prompt).await
}

async fn run_dry_run(args: &[String]) -> Result<()> {
    let cfg = CrowConfig::load()?;
    let prompt = args.join(" ");
    let mut messages = crow_runtime::context::ConversationManager::new(vec![]);
    let runtime = crate::runtime::SessionRuntime::boot(&cfg).await?;
    runtime
        .execute_turn(
            &cfg,
            &prompt,
            &mut messages,
            crate::event::ViewMode::default(),
        )
        .await
        .map(|_| ())
}

async fn run_yolo(args: &[String]) -> Result<()> {
    let cfg = CrowConfig::load()?;
    let prompt = args.join(" ");
    let mut messages = crow_runtime::context::ConversationManager::new(vec![]);
    let runtime = crate::runtime::SessionRuntime::boot(&cfg).await?;
    let mut observer = crate::event::CliEventHandler::new(crate::event::ViewMode::default());
    runtime
        .execute_native_turn(
            &cfg,
            &prompt,
            &mut messages,
            &mut observer,
        )
        .await
        .map(|_| ())
}

// `apply_sandbox_to_workspace` moved to `crow_workspace::applier`.

// ─── MCP Commands ───────────────────────────────────────────────────

async fn handle_mcp_command(args: &[String]) -> Result<()> {
    let subcmd = args.first().map(std::string::String::as_str);
    match subcmd {
        Some("list-tools") => {
            let server_name = args.get(1).map(std::string::String::as_str);
            let cfg = CrowConfig::load()?;

            let (name, mcp_cfg) = if let Some(n) = server_name {
                let conf = cfg
                    .mcp_servers
                    .get(n)
                    .ok_or_else(|| anyhow::anyhow!("MCP server '{n}' not found in config"))?;
                (n, conf)
            } else {
                if cfg.mcp_servers.is_empty() {
                    anyhow::bail!("No MCP servers configured in .crow/config.json");
                }
                // Just grab the first one
                let first = cfg.mcp_servers.iter().next().ok_or_else(|| {
                    anyhow::anyhow!(
                        "Expected at least one MCP server, but map was empty after check"
                    )
                })?;
                (first.0.as_str(), first.1)
            };

            println!(
                "🔌 Connecting to MCP Server: {} ({} {})",
                name,
                mcp_cfg.command,
                mcp_cfg.args.join(" ")
            );

            let args_refs: Vec<&str> = mcp_cfg
                .args
                .iter()
                .map(std::string::String::as_str)
                .collect();
            let client = crow_mcp::McpClient::spawn(&mcp_cfg.command, &args_refs)?;

            println!("  Initializing handshake...");
            let init = client.initialize().await?;
            println!(
                "  ✅ Server initialized: {} v{}",
                init.server_info.name, init.server_info.version
            );

            println!("  Fetching tools...");
            let tools = client.list_tools().await?;
            println!("\n🛠️  Available Tools ({}):", tools.tools.len());
            for tool in tools.tools {
                println!(
                    "  - {} : {}",
                    tool.name,
                    tool.description.as_deref().unwrap_or("No description")
                );
            }
            Ok(())
        }
        _ => {
            eprintln!("Usage: crow mcp list-tools [server-name]");
            Ok(())
        }
    }
}

// ─── Ledger ─────────────────────────────────────────────────────────

pub(crate) fn open_ledger(
    workspace: &std::path::Path,
) -> Result<crow_workspace::ledger::EventLedger> {
    use anyhow::Context;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    workspace.to_string_lossy().hash(&mut hasher);
    let hash = format!("{:x}", hasher.finish());

    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .context("Could not determine home directory")?;

    let ledger_dir = home.join(".crow").join("ledger");
    std::fs::create_dir_all(&ledger_dir)?;

    let log_path = ledger_dir.join(format!("{hash}.jsonl"));
    Ok(crow_workspace::ledger::EventLedger::open(&log_path)?)
}

// ─── AutoDream ──────────────────────────────────────────────────────

async fn run_autodream() -> Result<()> {
    let cfg = config::CrowConfig::load()?;
    println!("🦅 crow dream — Background Memory Consolidation");

    let dreamer = crow_brain::autodream::AutoDream::new(&cfg.workspace)?;
    let client = cfg.build_llm_client()?;
    dreamer.execute_dream_cycle(client.as_ref()).await?;

    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_apply_winning_plan_sandbox_only_does_not_append_ledger() {
        let workspace = TempDir::new().unwrap();
        let ledger_dir = workspace.path().join("ledger.jsonl");
        let ledger = crow_workspace::ledger::EventLedger::open(&ledger_dir).unwrap();

        let cfg = config::CrowConfig {
            workspace: workspace.path().to_path_buf(),
            write_mode: config::WriteMode::SandboxOnly,
            llm: Default::default(),
            map_budget: 1024,
            mcp_servers: Default::default(),
        };

        let sandbox = TempDir::new().unwrap();
        let plan = crow_patch::IntentPlan {
            base_snapshot_id: crow_patch::SnapshotId("snap-123".into()),
            rationale: "test".into(),
            is_partial: false,
            confidence: crow_patch::Confidence::High,
            requires_mcts: true,
            operations: vec![],
        };

        let snap_id = crow_patch::SnapshotId("snap-123".into());

        let ledger = std::sync::Mutex::new(ledger);
        let mut obs = crate::event::CliEventHandler::new(crate::event::ViewMode::default());
        crate::crucible_runner::apply_winning_plan(
            &cfg,
            sandbox.path(),
            &plan,
            "test-plan",
            &snap_id,
            &ledger,
            &mut obs,
        )
        .await
        .unwrap();

        // Ledger should be empty for SandboxOnly mode
        let contents = fs::read_to_string(&ledger_dir).unwrap_or_default();
        assert!(
            contents.is_empty(),
            "SandboxOnly should not emit PlanApplied event to ledger"
        );
    }
}
