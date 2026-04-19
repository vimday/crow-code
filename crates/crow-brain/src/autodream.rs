//! Background memory consolidation for long-term intelligence.
//!
//! Provides the `AutoDream` background daemon for indexing, pruning,
//! and consolidating ledger records into actionable `MemoryFragment`s.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The structure of a consolidated memory fragment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryFragment {
    pub topic: String,
    pub insights: Vec<String>,
    pub learned_at: chrono::DateTime<chrono::Utc>,
    pub related_files: Vec<String>,
}

/// AutoDream implements the async background knowledge consolidation.
pub struct AutoDream<'a> {
    workspace: &'a Path,
    memory_dir: PathBuf,
}

impl<'a> AutoDream<'a> {
    pub fn new(workspace: &'a Path) -> Result<Self> {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        use std::hash::{Hash, Hasher};
        workspace.to_string_lossy().hash(&mut hasher);
        let hash = format!("{:x}", hasher.finish());

        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .map(PathBuf::from)
            .context("Could not determine home directory")?;

        let memory_dir = home.join(".crow").join("memory").join(hash);
        std::fs::create_dir_all(&memory_dir)?;

        Ok(Self {
            workspace,
            memory_dir,
        })
    }

    /// Run the background dream daemon
    pub async fn execute_dream_cycle(&self, client: &dyn crate::LlmClient) -> Result<()> {
        println!("  🌙 [AutoDream] Initiating background memory consolidation...");

        // 1. Orient: Find all sessions and ledgers for this workspace

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        use std::hash::{Hash, Hasher};
        self.workspace.to_string_lossy().hash(&mut hasher);
        let hash = format!("{:x}", hasher.finish());

        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .map(PathBuf::from)
            .context("Could not determine home directory")?;
        let ledger_path = home
            .join(".crow")
            .join("ledger")
            .join(format!("{hash}.jsonl"));

        if !ledger_path.exists() {
            println!("  🌙 [AutoDream] No recent memories to consolidate.");
            return Ok(());
        }

        // 2. Gather: Pull events from ledger
        let ledger_content = std::fs::read_to_string(&ledger_path)?;
        let lines: Vec<&str> = ledger_content.lines().collect();
        let event_count = lines.len();

        println!("  🌙 [AutoDream] Collected {event_count} raw events from the ledger.");

        if event_count < 10 {
            // Not enough to justify an LLM call yet.
            println!("  🌙 [AutoDream] Insufficient volume for deep sleep consolidation. Waiting for more data.");
            return Ok(());
        }

        // 3. Consolidate: Ask LLM to extract meta-knowledge
        println!("  🌙 [AutoDream] Extracting high-value architectural invariants from traces...");

        const PROMPT_TEMPLATE: &str = "\
You are an AutoDream background worker running over `{workspace}`. 
Your job is to read raw traces and extract domain invariants, traps to avoid, and structural constraints.
Trace size: {event_count} records. 

Please output a JSON array of `MemoryFragment` structured objects describing what was learned.
Limit to 3 critical architectural or context insights.";

        let prompt = PROMPT_TEMPLATE
            .replace("{workspace}", &self.workspace.display().to_string())
            .replace("{event_count}", &event_count.to_string());

        let messages = vec![crate::compiler::ChatMessage::user(&prompt)];

        match client.generate(&messages).await {
            Ok(response) => {
                println!("  🌙 [AutoDream] Subconscious processing complete.");

                // 4. Prune / Store
                let cleaned = crate::compiler::extract_json_block(&response);
                match serde_json::from_str::<Vec<MemoryFragment>>(cleaned) {
                    Ok(fragments) => {
                        let validated = serde_json::to_string_pretty(&fragments)?;
                        let fragment_path = self
                            .memory_dir
                            .join(format!("memory_{}.json", chrono::Utc::now().timestamp()));
                        std::fs::write(&fragment_path, validated)?;

                        println!(
                            "  🌙 [AutoDream] Deep long-term memory written to: {}",
                            fragment_path.display()
                        );
                    }
                    Err(e) => {
                        eprintln!("  🌙 [AutoDream] Discarded malformed memory fragment: {e}");
                    }
                }

                // We would then truncate or rotate the ledger here to compress the log
            }
            Err(e) => {
                eprintln!("  🌙 [AutoDream] Dream interrupted by error: {e}");
            }
        }

        Ok(())
    }
}
