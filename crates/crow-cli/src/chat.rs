use crate::config::CrowConfig;
use crate::session::{Session, SessionStore};
use anyhow::Result;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::path::Path;

/// Enter continuous chat REPL mode
pub async fn run_repl(cfg: &CrowConfig) -> Result<()> {
    println!("🦅 Entering Crow Agent REPL");
    println!("   Type your instructions for the workspace.");
    println!("   Commands: /exit (quit), /clear (clear context)");
    println!();

    let mut rl = DefaultEditor::new()?;
    // We try to load rustyline history if available in `.crow/`
    let history_path = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    let history_file = Path::new(&history_path).join(".crow").join("repl_history.txt");
    let _ = rl.load_history(&history_file);

    let store = SessionStore::open().ok();
    if store.is_none() {
        println!("⚠️ Warning: Could not open structured SessionStore. Session persistence might be degraded.");
    }

    let mut session = Session::new(&cfg.workspace, "Interactive REPL Session");
    println!("   Session ID: {}", session.id.0);
    println!("   Workspace:  {}\n", cfg.workspace.display());

    let mut messages = crate::context::ConversationManager::new(vec![]);

    loop {
        let readline = rl.readline("crow> ");
        match readline {
            Ok(line) => {
                let input = line.trim();
                if input.is_empty() {
                    continue;
                }
                
                rl.add_history_entry(input)?;
                let _ = rl.save_history(&history_file);

                if input == "/exit" || input == "/quit" {
                    println!("Goodbye!");
                    break;
                }
                
                if input == "/clear" {
                    messages = crate::context::ConversationManager::new(vec![]);
                    println!("🧹 Context cleared.");
                    continue;
                }

                // Add user message to the ongoing context is handled in run_conversation_turn

                println!("💭 Analyzing workspace and synthesizing IntentPlan...");
                
                // Crucible Loop Execution
                match crate::run_conversation_turn(cfg, input, &mut messages).await {
                    Ok(_) => {
                        println!("\n─── 🔄 Session Context Synced ───");
                        // We do NOT stop on success; we let the agent state accumulate!
                        session.save_messages(&messages.as_messages());
                        if let Some(store) = &store {
                            if let Err(e) = store.save(&session) {
                                println!("⚠️ Could not persist session trace: {:?}", e);
                            }
                        }
                    }
                    Err(e) => {
                        println!("\n❌ Task Execution Halted:");
                        println!("{:?}", e);
                        println!("(You can provide follow-up instructions to resolve the failure)");
                    }
                }
            }
            Err(ReadlineError::Interrupted) => {
                println!("^C (Type /exit to leave)");
                continue;
            }
            Err(ReadlineError::Eof) => {
                // Ctrl+D
                println!("Goodbye!");
                break;
            }
            Err(err) => {
                println!("Error reading input: {:?}", err);
                break;
            }
        }
    }

    Ok(())
}
