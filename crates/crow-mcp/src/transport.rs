//! Asynchronous stdio transport for MCP.
//!
//! Spawns a child process and implements a JSON-RPC 2.0 full-duplex transport
//! over its stdin and stdout using `tokio::process`. Handles request/response
//! correlation via message IDs.

use crate::types::{Id, Request, Response};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;

pub struct StdioTransport {
    _child: Child,
    stdin_tx: mpsc::Sender<String>,
    pending_requests: Arc<Mutex<HashMap<u64, oneshot::Sender<Option<serde_json::Value>>>>>,
    next_id: AtomicU64,
    _reader_task: JoinHandle<()>,
    _writer_task: JoinHandle<()>,
}

impl StdioTransport {
    /// Start an MCP server defined by `cmd` and its `args`.
    pub fn spawn(cmd: &str, args: &[&str]) -> Result<Self> {
        let mut child = Command::new(cmd)
            .args(args)
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // Pass stderr through for debugging
            .spawn()
            .with_context(|| format!("Failed to spawn MCP server '{}'", cmd))?;

        let stdin = child.stdin.take().context("Failed to open child stdin")?;
        let stdout = child.stdout.take().context("Failed to open child stdout")?;

        let pending_requests: Arc<Mutex<HashMap<u64, oneshot::Sender<Option<serde_json::Value>>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(32);

        // Writer task
        let writer_task = tokio::spawn({
            let mut stdin = stdin;
            async move {
                while let Some(msg) = stdin_rx.recv().await {
                    if let Err(e) = stdin.write_all(msg.as_bytes()).await {
                        eprintln!("[MCP Transport] Failed to write to stdin: {}", e);
                        break;
                    }
                    if let Err(e) = stdin.write_all(b"\n").await {
                        eprintln!("[MCP Transport] Failed to write newline to stdin: {}", e);
                        break;
                    }
                    if let Err(e) = stdin.flush().await {
                        eprintln!("[MCP Transport] Failed to flush stdin: {}", e);
                        break;
                    }
                }
            }
        });

        // Reader task
        let reader_task = tokio::spawn({
            let pending_requests = Arc::clone(&pending_requests);
            let mut reader = BufReader::new(stdout);
            async move {
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) => break, // EOF
                        Ok(_) => {
                            let trimmed = line.trim();
                            if trimmed.is_empty() {
                                continue;
                            }

                            // Parse JSON-RPC message
                            match serde_json::from_str::<serde_json::Value>(trimmed) {
                                Ok(value) => {
                                    // Is it a response? Look for id.
                                    if let Some(id_val) = value.get("id") {
                                        if let Some(id_num) = id_val.as_u64() {
                                            // Handle response by waking pending request
                                            let mut pending = pending_requests.lock().await;
                                            if let Some(tx) = pending.remove(&id_num) {
                                                // Convert to full Response type internally or just pass value
                                                let _ = tx.send(Some(value));
                                            }
                                        }
                                    } else {
                                        // Notifications, etc. (we ignore for now in this MVP)
                                    }
                                }
                                Err(e) => {
                                    eprintln!("[MCP Transport] Failed to parse JSON-RPC: {}", e);
                                    eprintln!("[MCP Transport] Raw line: {}", trimmed);
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("[MCP Transport] Read error: {}", e);
                            break;
                        }
                    }
                }
            }
        });

        Ok(Self {
            _child: child,
            stdin_tx,
            pending_requests,
            next_id: AtomicU64::new(1),
            _reader_task: reader_task,
            _writer_task: writer_task,
        })
    }

    /// Send a request and wait for a response
    pub async fn send_request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<Response> {
        let id_num = self.next_id.fetch_add(1, Ordering::SeqCst);
        let id = Id::Number(id_num);

        let req = Request {
            jsonrpc: "2.0".into(),
            id: id.clone(),
            method: method.into(),
            params,
        };

        let req_json = serde_json::to_string(&req)?;

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending_requests.lock().await;
            pending.insert(id_num, tx);
        }

        self.stdin_tx
            .send(req_json)
            .await
            .context("Transport channel closed")?;

        match rx.await {
            Ok(Some(resp_value)) => {
                let resp: Response = serde_json::from_value(resp_value)
                    .context("Failed to deserialize JSON-RPC response")?;

                if let Some(err) = &resp.error {
                    bail!("JSON-RPC Error {}: {}", err.code, err.message);
                }

                Ok(resp)
            }
            Ok(None) => bail!("Received empty response"),
            Err(_) => bail!("Response channel dropped unexpectedly (did the reader task fail?)"),
        }
    }

    /// Send a notification (no response expected)
    pub async fn send_notification(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<()> {
        let notif = crate::types::Notification {
            jsonrpc: "2.0".into(),
            method: method.into(),
            params,
        };

        let notif_json = serde_json::to_string(&notif)?;
        self.stdin_tx
            .send(notif_json)
            .await
            .context("Transport channel closed")?;

        Ok(())
    }
}
