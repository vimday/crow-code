use crate::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::Mutex;
use tokio::process::Child;
use tokio::io::AsyncWriteExt;
use std::process::Stdio;

/// Maximum bytes to return when checking bash status.
const MAX_STATUS_OUTPUT_BYTES: usize = 50 * 1024; // 50 KB

pub struct BackgroundTask {
    pub id: String,
    pub command: String,
    pub log_file: PathBuf,
    pub child: Child,
    pub start_time: std::time::Instant,
}

pub struct BackgroundProcessManager {
    tasks: Mutex<HashMap<String, BackgroundTask>>,
    next_id: std::sync::atomic::AtomicUsize,
}

impl Default for BackgroundProcessManager {
    fn default() -> Self {
        Self::new()
    }
}

impl BackgroundProcessManager {
    pub fn new() -> Self {
        Self {
            tasks: Mutex::new(HashMap::new()),
            next_id: std::sync::atomic::AtomicUsize::new(1),
        }
    }

    /// Spawns a background bash command and returns its task ID.
    pub async fn spawn(&self, command: String, cwd: &std::path::Path) -> Result<String> {
        let task_id = format!("bg-{}", self.next_id.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
        
        let temp_dir = std::env::temp_dir().join("crow-bg-tasks");
        tokio::fs::create_dir_all(&temp_dir).await?;
        let log_file = temp_dir.join(format!("{task_id}.log"));
        
        // Truncate/create log file
        tokio::fs::write(&log_file, "").await?;

        // We use stdio piped and then a detached task to copy output to the file.
        // This ensures the file receives both stdout and stderr in real-time.
        let mut child = tokio::process::Command::new("bash")
            .arg("-c")
            .arg(&command)
            .current_dir(cwd)
            .env("PAGER", "cat")
            .env("GIT_PAGER", "cat")
            .env("TERM", "dumb")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;

        let mut stdout = match child.stdout.take() {
            Some(out) => out,
            None => anyhow::bail!("Failed to capture stdout"),
        };
        let mut stderr = match child.stderr.take() {
            Some(err) => err,
            None => anyhow::bail!("Failed to capture stderr"),
        };
        let log_path_clone = log_file.clone();
        
        // Spawn a background copier to stream outputs to the log file.
        tokio::spawn(async move {
            let file = tokio::fs::OpenOptions::new().append(true).create(true).open(&log_path_clone).await;
            if let Ok(mut file) = file {
                // Read from stdout and stderr concurrently
                let mut stdout_buf = [0u8; 1024];
                let mut stderr_buf = [0u8; 1024];
                let mut stdout_done = false;
                let mut stderr_done = false;
                
                loop {
                    if stdout_done && stderr_done {
                        break;
                    }
                    
                    tokio::select! {
                        res = tokio::io::AsyncReadExt::read(&mut stdout, &mut stdout_buf), if !stdout_done => {
                            match res {
                                Ok(0) | Err(_) => stdout_done = true, // EOF or error
                                Ok(n) => { let _ = file.write_all(&stdout_buf[..n]).await; }
                            }
                        }
                        res = tokio::io::AsyncReadExt::read(&mut stderr, &mut stderr_buf), if !stderr_done => {
                            match res {
                                Ok(0) | Err(_) => stderr_done = true, // EOF or error
                                Ok(n) => { let _ = file.write_all(&stderr_buf[..n]).await; }
                            }
                        }
                    }
                }
            }
        });

        let task = BackgroundTask {
            id: task_id.clone(),
            command: command.clone(),
            log_file,
            child,
            start_time: std::time::Instant::now(),
        };

        let mut tasks = self.tasks.lock().await;
        tasks.insert(task_id.clone(), task);

        Ok(task_id)
    }

    /// Read the latest output from a background task.
    pub async fn status(&self, task_id: &str) -> Result<String> {
        let mut tasks = self.tasks.lock().await;
        let task = match tasks.get_mut(task_id) {
            Some(t) => t,
            None => anyhow::bail!("Task ID '{task_id}' not found. It may have never existed or was already cleaned up."),
        };

        // Check if it's still running
        let is_running = match task.child.try_wait() {
            Ok(None) => true,
            Ok(Some(_)) => false,
            Err(_) => false, // Treat error as stopped
        };

        let elapsed = task.start_time.elapsed().as_secs();
        let log_content = tokio::fs::read_to_string(&task.log_file).await.unwrap_or_default();
        let truncated_log = crow_patch::safe_truncate(&log_content, MAX_STATUS_OUTPUT_BYTES);
        let log_suffix = if log_content.len() > MAX_STATUS_OUTPUT_BYTES {
            "\n\n[SYSTEM WARNING: Log truncated to 50KB]"
        } else {
            ""
        };

        let status_str = if is_running { "RUNNING" } else { "STOPPED" };

        Ok(format!(
            "Task ID: {task_id}\nCommand: {cmd}\nStatus: {status_str}\nElapsed: {elapsed}s\n\n--- Output ---\n{truncated_log}{log_suffix}",
            cmd = task.command,
        ))
    }

    /// Kill a background task.
    pub async fn kill(&self, task_id: &str) -> Result<String> {
        let mut tasks = self.tasks.lock().await;
        if let Some(mut task) = tasks.remove(task_id) {
            match task.child.kill().await {
                Ok(_) => Ok(format!("Task '{task_id}' successfully killed.")),
                Err(e) => Ok(format!("Attempted to kill task '{task_id}' but encountered error: {e}")),
            }
        } else {
            anyhow::bail!("Task ID '{task_id}' not found.")
        }
    }
}

// ─── Tools ─────────────────────────────────────────────────────────

pub struct BashStatusTool;

#[async_trait::async_trait]
impl Tool for BashStatusTool {
    fn name(&self) -> &'static str {
        "bash_status"
    }

    fn description(&self) -> &'static str {
        "Check the status and read the latest output of a background bash task."
    }

    fn is_read_only(&self) -> bool { true }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The ID of the background task (e.g., bg-1)"
                }
            },
            "required": ["task_id"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> Result<ToolOutput> {
        #[derive(serde::Deserialize)]
        struct Args { task_id: String }
        let parsed: Args = serde_json::from_value(args)?;

        let bg_manager = match &ctx.background_manager {
            Some(mgr) => mgr,
            None => return Ok(ToolOutput::error("Background task management is not available in this context.")),
        };

        match bg_manager.status(&parsed.task_id).await {
            Ok(output) => Ok(ToolOutput::success(output)),
            Err(e) => Ok(ToolOutput::error(e.to_string())),
        }
    }
}

pub struct BashKillTool;

#[async_trait::async_trait]
impl Tool for BashKillTool {
    fn name(&self) -> &'static str {
        "bash_kill"
    }

    fn description(&self) -> &'static str {
        "Kill a running background bash task."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The ID of the background task to kill (e.g., bg-1)"
                }
            },
            "required": ["task_id"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> Result<ToolOutput> {
        #[derive(serde::Deserialize)]
        struct Args { task_id: String }
        let parsed: Args = serde_json::from_value(args)?;

        let bg_manager = match &ctx.background_manager {
            Some(mgr) => mgr,
            None => return Ok(ToolOutput::error("Background task management is not available in this context.")),
        };

        match bg_manager.kill(&parsed.task_id).await {
            Ok(output) => Ok(ToolOutput::success(output)),
            Err(e) => Ok(ToolOutput::error(e.to_string())),
        }
    }
}
