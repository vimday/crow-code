use crate::config::CrowConfig;
use crate::context::ConversationManager;
use crate::runtime::SessionRuntime;
use crate::tui::state::{TuiMessage, CancellationToken};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use crow_patch::SnapshotId;
use crate::session::{Session, SessionStore};

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
}

/// The state tracking for the active thread.
pub struct CodexThread {
    pub status: TurnStatus,
    pub cancellation: Option<CancellationToken>,
}

/// ThreadManager sits between the TUI loop and the SessionRuntime, decoupling
/// synchronous UI events from the autonomous MCTS solver pipeline.
pub struct ThreadManager {
    runtime: Arc<Mutex<SessionRuntime>>,
    messages: Arc<Mutex<ConversationManager>>,
    config: CrowConfig,
    ui_tx: mpsc::UnboundedSender<TuiMessage>,
    thread_state: Arc<Mutex<CodexThread>>,
    session_id: Arc<Mutex<Option<String>>>,
}

impl ThreadManager {
    pub fn new(
        runtime: Arc<Mutex<SessionRuntime>>,
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
            let mut observer = crate::event::TuiEventHandler::with_cancellation(
                ui_tx.clone(),
                token.clone(),
            );
            
            let mut rt_guard = rt_clone.lock().await;
            let mut msgs_guard = msgs_clone.lock().await;

            let result = rt_guard
                .execute_turn_with_observer(&cfg_clone, &prompt, &mut msgs_guard, &mut observer)
                .await;
            
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
                                store.load(&crate::session::SessionId(sid.clone())).unwrap_or_else(|_| {
                                    Session::new(std::path::Path::new(&cfg_clone.workspace), "Interaction")
                                })
                            } else {
                                // Default task name
                                Session::new(std::path::Path::new(&cfg_clone.workspace), &prompt_clone)
                            };
                            
                            current_session.save_messages(&msgs_guard.as_messages());
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
            state.cancellation = None;
        });
    }
}
