use chrono::{DateTime, Utc};
use crow_patch::SnapshotId;
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

/// A unique identifier for an IntentPlan compilation.
pub type PlanId = String;

/// Events that accurately model the lifecycle and reality of the workspace state machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event_type")]
pub enum LedgerEvent {
    SnapshotCreated {
        id: SnapshotId,
        git_hash: String,
        timestamp: DateTime<Utc>,
    },
    PlanHydrated {
        plan_id: PlanId,
        snapshot_id: SnapshotId,
        timestamp: DateTime<Utc>,
    },
    PreflightStarted {
        plan_id: PlanId,
        sandbox_path: String,
        timestamp: DateTime<Utc>,
    },
    PreflightTested {
        plan_id: PlanId,
        passed: bool,
        duration_ms: u64,
        timestamp: DateTime<Utc>,
    },
    PlanApplied {
        plan_id: PlanId,
        snapshot_id: SnapshotId,
        timestamp: DateTime<Utc>,
    },
    PlanRolledBack {
        plan_id: PlanId,
        reason: String,
        timestamp: DateTime<Utc>,
    },
}

/// The Event Ledger powers deterministic replay, safety tracking, and the future `AutoDream` memory engine.
#[derive(Debug)]
pub struct EventLedger {
    log_path: PathBuf,
    events: Vec<LedgerEvent>,
}

impl EventLedger {
    /// Initialize or load the event ledger from the given log file.
    pub fn open(log_path: &Path) -> std::io::Result<Self> {
        let mut events = Vec::new();

        if log_path.exists() {
            let content = std::fs::read_to_string(log_path)?;
            for line in content.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(event) = serde_json::from_str::<LedgerEvent>(line) {
                    events.push(event);
                }
            }
        } else if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        Ok(Self {
            log_path: log_path.to_path_buf(),
            events,
        })
    }

    /// Append a new event to the ledger synchronously.
    pub fn append(&mut self, event: LedgerEvent) -> std::io::Result<()> {
        let serialized = serde_json::to_string(&event)?;

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)?;

        writeln!(file, "{}", serialized)?;

        self.events.push(event);
        Ok(())
    }

    /// Retrieve all recorded events.
    pub fn history(&self) -> &[LedgerEvent] {
        &self.events
    }
}
