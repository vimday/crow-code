use crate::config::CrowConfig;
use crate::context::ConversationManager;
use crate::runtime::SessionRuntime;
use crate::session::{Session, SessionStore};
use crate::tui::state::{CancellationToken, TuiMessage};
use crow_patch::SnapshotId;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

/// Represents the deterministic state of an agent Turn lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnStatus {
    Idle,
    InProgress,
    Completed(Option<SnapshotId>),
    Aborted,
    Failed(String),
}

/// A specific operational command sent to the Thread Manager.
pub enum Op {
    Input(String),
    Interrupt,
    Clear,
    SwarmRun(String),
}

/// The state tracking for the active thread.
pub struct CodexThread {
    pub status: TurnStatus,
    pub cancellation: Option<CancellationToken>,
}

/// ThreadManager sits between the TUI loop and the SessionRuntime, decoupling
/// synchronous UI events from the autonomous MCTS solver pipeline.
pub struct ThreadManager {
    runtime: Arc<SessionRuntime>,
    messages: Arc<Mutex<ConversationManager>>,
    config: CrowConfig,
    ui_tx: mpsc::UnboundedSender<TuiMessage>,
    thread_state: Arc<Mutex<CodexThread>>,
    session_id: Arc<Mutex<Option<String>>>,
    swarm_state: Arc<Mutex<std::collections::HashMap<String, CodexThread>>>,
}

impl ThreadManager {
    pub fn new(
        runtime: Arc<SessionRuntime>,
        messages: Arc<Mutex<ConversationManager>>,
        config: CrowConfig,
        ui_tx: mpsc::UnboundedSender<TuiMessage>,
        initial_session_id: Option<String>,
    ) -> Self {
        Self {
            runtime,
            messages,
            config,
            ui_tx,
            thread_state: Arc::new(Mutex::new(CodexThread {
                status: TurnStatus::Idle,
                cancellation: None,
            })),
            session_id: Arc::new(Mutex::new(initial_session_id)),
            swarm_state: Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Submit an operation to the Thread Manager pipeline deterministically.
    pub async fn submit(&self, op: Op) {
        match op {
            Op::Input(prompt) => {
                let mut state = self.thread_state.lock().await;
                if state.status == TurnStatus::InProgress {
                    // Refuse input if turn is actively processing
                    return;
                }

                let token = CancellationToken::new();
                state.status = TurnStatus::InProgress;
                state.cancellation = Some(token.clone());

                // Spawn autonomous turn
                self.spawn_turn(prompt, token);
            }
            Op::Interrupt => {
                let mut state = self.thread_state.lock().await;
                if state.status == TurnStatus::InProgress {
                    if let Some(token) = &state.cancellation {
                        token.cancel();
                        state.status = TurnStatus::Aborted;
                        let _ = self.ui_tx.send(TuiMessage::SessionComplete);
                    }
                }
            }
            Op::Clear => {
                let mut msgs = self.messages.lock().await;
                msgs.set_system(vec![]);
            }
            Op::SwarmRun(prompt) => {
                let token = CancellationToken::new();
                self.spawn_swarm(prompt, token).await;
            }
        }
    }

    fn spawn_turn(&self, prompt: String, token: CancellationToken) {
        let rt_clone = Arc::clone(&self.runtime);
        let msgs_clone = Arc::clone(&self.messages);
        let cfg_clone = self.config.clone();
        let ui_tx = self.ui_tx.clone();
        let thread_state = Arc::clone(&self.thread_state);
        let thread_state_sid = Arc::clone(&self.session_id);
        let prompt_clone = prompt.clone();

        tokio::spawn(async move {
            let mut observer =
                crate::event::TuiEventHandler::with_cancellation(ui_tx.clone(), token.clone());

            // Clone messages to prevent locking ConversationManager for the duration of the run
            let mut local_msgs = msgs_clone.lock().await.clone();

            let result = rt_clone
                .execute_turn_with_observer(&cfg_clone, &prompt, &mut local_msgs, &mut observer)
                .await;
            
            // Sync mutated messages back
            *msgs_clone.lock().await = local_msgs.clone();

            let mut state = thread_state.lock().await;
            if token.is_cancelled() {
                state.status = TurnStatus::Aborted;
                let _ = ui_tx.send(TuiMessage::TurnComplete(false));
            } else {
                match result {
                    Ok(snapshot_id) => {
                        state.status = TurnStatus::Completed(Some(snapshot_id.clone()));
                        let _ = ui_tx.send(TuiMessage::TurnComplete(true));

                        // Async persistence after turn completion
                        if let Ok(store) = SessionStore::open() {
                            let mut sid_guard = thread_state_sid.lock().await;
                            let mut current_session = if let Some(ref sid) = *sid_guard {
                                store
                                    .load(&crate::session::SessionId(sid.clone()))
                                    .unwrap_or_else(|_| {
                                        Session::new(
                                            std::path::Path::new(&cfg_clone.workspace),
                                            "Interaction",
                                        )
                                    })
                            } else {
                                // Default task name
                                Session::new(
                                    std::path::Path::new(&cfg_clone.workspace),
                                    &prompt_clone,
                                )
                            };

                            current_session.save_messages(&local_msgs.as_messages());
                            current_session.push_snapshot(snapshot_id);

                            if store.save(&current_session).is_ok() {
                                *sid_guard = Some(current_session.id.0.clone());
                            }
                        }
                    }
                    Err(e) => {
                        state.status = TurnStatus::Failed(e.to_string());
                        let _ = ui_tx.send(TuiMessage::TurnComplete(false));
                    }
                }
            }
        });
    }

    async fn spawn_swarm(&self, prompt: String, token: CancellationToken) {
        let rt_clone = Arc::clone(&self.runtime);
        let msgs_clone = Arc::clone(&self.messages);
        let ui_tx = self.ui_tx.clone();
        let swarm_state = Arc::clone(&self.swarm_state);
        let prompt_clone = prompt.clone();

        // Register swarm task
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros();
        // Extract out action args
        let id = format!("swarm-{:04x}", (ts % 0xffff) as u16);

        {
            let mut s = swarm_state.lock().await;
            s.insert(
                id.clone(),
                CodexThread {
                    status: TurnStatus::InProgress,
                    cancellation: Some(token.clone()),
                },
            );
        }

        let _ = ui_tx.send(TuiMessage::SwarmStarted(id.clone(), prompt_clone.clone()));

        tokio::spawn(async move {
            let mut observer =
                crate::event::TuiEventHandler::with_cancellation(ui_tx.clone(), token.clone());

            let (compiler, root, mcp, sys_msgs) = {
                let msgs_guard = msgs_clone.lock().await;
                (
                    Arc::clone(&rt_clone.compiler),
                    rt_clone.workspace.clone(),
                    Arc::clone(&rt_clone.mcp_manager),
                    msgs_guard.as_messages(),
                )
            };

            // Reconstruct compiler instance since it clones easily
            let compiler_instance = (*compiler).clone();
            let mut worker = crate::subagent::SubagentWorker::new(compiler_instance);
            // Replace worker id for consistency with the UI
            worker.id = id.clone();

            let result = worker
                .execute(
                    &prompt_clone,
                    &[], // No specific focus paths initially
                    "Swarm background task initiated via TUI.",
                    sys_msgs,
                    &root,
                    Some(&mcp),
                    &mut observer,
                )
                .await;

            {
                let mut s = swarm_state.lock().await;
                if let Some(mut state) = s.remove(&id) {
                    if token.is_cancelled() {
                        state.status = TurnStatus::Aborted;
                        let _ = ui_tx.send(TuiMessage::SwarmComplete(id.clone(), false));
                    } else {
                        match result {
                            Ok(_) => {
                                state.status = TurnStatus::Completed(None);
                                let _ = ui_tx.send(TuiMessage::SwarmComplete(id.clone(), true));
                            }
                            Err(e) => {
                                state.status = TurnStatus::Failed(e.to_string());
                                let _ = ui_tx.send(TuiMessage::SwarmComplete(id.clone(), false));
                            }
                        }
                    }
                }
            }
        });
    }
}
