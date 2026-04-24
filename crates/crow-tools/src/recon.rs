use crate::{Tool, ToolContext, ToolOutput};
use anyhow::Result;

pub struct ListDirTool;

#[async_trait::async_trait]
impl Tool for ListDirTool {
    fn name(&self) -> &'static str {
        "list_dir"
    }

    fn description(&self) -> &'static str {
        "List directory contents with detailed file information"
    }

    fn is_read_only(&self) -> bool { true }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory path to list (relative to workspace root)"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> Result<ToolOutput> {
        #[derive(serde::Deserialize)]
        struct Args {
            path: crow_patch::WorkspacePath,
        }
        let parsed: Args = serde_json::from_value(args)?;
        
        let v_cmd = crow_probe::VerificationCommand {
            program: "ls".to_string(),
            args: vec!["-la".to_string(), "--".to_string(), parsed.path.as_str().to_string()],
            cwd: None,
        };
        let exec_config = crow_verifier::ExecutionConfig {
            timeout: std::time::Duration::from_secs(10),
            max_output_bytes: 512 * 1024,
        };
        let result = crow_verifier::executor::execute(
            ctx.workspace_root,
            &v_cmd,
            &exec_config,
            &crow_verifier::types::AciConfig::compact(),
            None,
        ).await?;
        Ok(ToolOutput::success(result.test_run.truncated_log))
    }
}

pub struct SearchTool;

#[async_trait::async_trait]
impl Tool for SearchTool {
    fn name(&self) -> &'static str {
        "grep"
    }

    fn description(&self) -> &'static str {
        "Search for a regex pattern across files using ripgrep. Returns matching lines with file paths and line numbers."
    }

    fn is_read_only(&self) -> bool { true }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regex pattern to search for"
                },
                "path": {
                    "type": "string",
                    "description": "Directory or file path to search within (default: workspace root)"
                },
                "glob": {
                    "type": "string",
                    "description": "Glob filter for file types (e.g. '*.rs', '*.py')"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> Result<ToolOutput> {
        #[derive(serde::Deserialize)]
        struct Args {
            pattern: String,
            path: Option<crow_patch::WorkspacePath>,
            glob: Option<String>,
        }
        let parsed: Args = serde_json::from_value(args)?;

        let mut a = vec![
            "-rn".to_string(),
            "-e".to_string(),
            parsed.pattern,
        ];
        if let Some(g) = parsed.glob {
            a.push("-g".to_string());
            a.push(g);
        }
        a.push("--".to_string());
        if let Some(p) = parsed.path {
            a.push(p.as_str().to_string());
        } else {
            a.push(".".to_string());
        }

        let v_cmd = crow_probe::VerificationCommand {
            program: "rg".to_string(),
            args: a,
            cwd: None,
        };
        let exec_config = crow_verifier::ExecutionConfig {
            timeout: std::time::Duration::from_secs(10),
            max_output_bytes: 512 * 1024,
        };
        let result = crow_verifier::executor::execute(
            ctx.workspace_root,
            &v_cmd,
            &exec_config,
            &crow_verifier::types::AciConfig::compact(),
            None,
        ).await?;
        Ok(ToolOutput::success(result.test_run.truncated_log))
    }
}

pub struct FetchUrlTool;

#[async_trait::async_trait]
impl Tool for FetchUrlTool {
    fn name(&self) -> &'static str {
        "fetch_url"
    }

    fn description(&self) -> &'static str {
        "Fetch and process the text content of a public URL"
    }

    fn is_read_only(&self) -> bool { true }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch"
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, args: serde_json::Value, _ctx: &ToolContext<'_>) -> Result<ToolOutput> {
        #[derive(serde::Deserialize)]
        struct Args {
            url: String,
        }
        let parsed: Args = serde_json::from_value(args)?;

        let max_fetch_bytes = 1024 * 50;
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(10))
            .user_agent("crow-code-agent/1.0")
            .build()?;

        let res = client.get(&parsed.url).send().await?;
        let status = res.status();
        if !status.is_success() {
            return Ok(ToolOutput::error(format!("{url} returned HTTP {status}", url = parsed.url)));
        }
        if let Some(ct) = res.headers().get(reqwest::header::CONTENT_TYPE) {
            let ct_str = ct.to_str().unwrap_or("");
            if !ct_str.contains("text/") && !ct_str.contains("application/json") {
                return Ok(ToolOutput::error(format!("Unsupported Content-Type '{ct_str}'. Only text or json supported.")));
            }
        }

        let mut text = res.text().await?;
        if text.len() > max_fetch_bytes {
            text.truncate(max_fetch_bytes);
            text.push_str("...\n\n[SYSTEM WARNING: Response truncated to 50KB]");
        }
        Ok(ToolOutput::success(text))
    }
}

pub struct FileInfoTool;

#[async_trait::async_trait]
impl Tool for FileInfoTool {
    fn name(&self) -> &'static str {
        "file_info"
    }
    fn description(&self) -> &'static str {
        "Show file metadata (size, type, permissions)"
    }

    fn is_read_only(&self) -> bool { true }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path" }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> Result<ToolOutput> {
        #[derive(serde::Deserialize)]
        struct Args { path: crow_patch::WorkspacePath }
        let parsed: Args = serde_json::from_value(args)?;
        let v_cmd = crow_probe::VerificationCommand {
            program: "file".to_string(),
            args: vec!["--".to_string(), parsed.path.as_str().to_string()],
            cwd: None,
        };
        let exec_config = crow_verifier::ExecutionConfig { timeout: std::time::Duration::from_secs(10), max_output_bytes: 512 * 1024 };
        let result = crow_verifier::executor::execute(ctx.workspace_root, &v_cmd, &exec_config, &crow_verifier::types::AciConfig::compact(), None).await?;
        Ok(ToolOutput::success(result.test_run.truncated_log))
    }
}

pub struct WordCountTool;

#[async_trait::async_trait]
impl Tool for WordCountTool {
    fn name(&self) -> &'static str {
        "word_count"
    }
    fn description(&self) -> &'static str {
        "Count lines, words, and bytes in a file"
    }

    fn is_read_only(&self) -> bool { true }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path" }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> Result<ToolOutput> {
        #[derive(serde::Deserialize)]
        struct Args { path: crow_patch::WorkspacePath }
        let parsed: Args = serde_json::from_value(args)?;
        let v_cmd = crow_probe::VerificationCommand {
            program: "wc".to_string(),
            args: vec!["-l".to_string(), "-c".to_string(), "--".to_string(), parsed.path.as_str().to_string()],
            cwd: None,
        };
        let exec_config = crow_verifier::ExecutionConfig { timeout: std::time::Duration::from_secs(10), max_output_bytes: 512 * 1024 };
        let result = crow_verifier::executor::execute(ctx.workspace_root, &v_cmd, &exec_config, &crow_verifier::types::AciConfig::compact(), None).await?;
        Ok(ToolOutput::success(result.test_run.truncated_log))
    }
}

pub struct DirTreeTool;

#[async_trait::async_trait]
impl Tool for DirTreeTool {
    fn name(&self) -> &'static str {
        "dir_tree"
    }
    fn description(&self) -> &'static str {
        "Show directory tree structure with a depth limit"
    }

    fn is_read_only(&self) -> bool { true }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Root directory path" },
                "max_depth": { "type": "integer", "description": "Max tree depth (default: 3, max: 10)" }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> Result<ToolOutput> {
        #[derive(serde::Deserialize)]
        struct Args { path: crow_patch::WorkspacePath, max_depth: Option<u32> }
        let parsed: Args = serde_json::from_value(args)?;
        let depth = parsed.max_depth.unwrap_or(3).min(10);
        let v_cmd = crow_probe::VerificationCommand {
            program: "tree".to_string(),
            args: vec!["-L".to_string(), depth.to_string(), "--".to_string(), parsed.path.as_str().to_string()],
            cwd: None,
        };
        let exec_config = crow_verifier::ExecutionConfig { timeout: std::time::Duration::from_secs(10), max_output_bytes: 512 * 1024 };
        let result = crow_verifier::executor::execute(ctx.workspace_root, &v_cmd, &exec_config, &crow_verifier::types::AciConfig::compact(), None).await?;
        Ok(ToolOutput::success(result.test_run.truncated_log))
    }
}

pub struct ReadFilesTool;

#[async_trait::async_trait]
impl Tool for ReadFilesTool {
    fn name(&self) -> &'static str {
        "read_file"
    }
    fn description(&self) -> &'static str {
        "Read the contents of a file from the workspace. Supports optional line-range selection."
    }

    fn is_read_only(&self) -> bool { true }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path relative to workspace root"
                },
                "start_line": {
                    "type": "integer",
                    "description": "Optional 1-indexed start line for partial reads"
                },
                "end_line": {
                    "type": "integer",
                    "description": "Optional 1-indexed end line (inclusive) for partial reads"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> Result<ToolOutput> {
        #[derive(serde::Deserialize)]
        struct Args {
            path: crow_patch::WorkspacePath,
            start_line: Option<usize>,
            end_line: Option<usize>,
        }
        let parsed: Args = serde_json::from_value(args)?;
        
        use std::io::{BufRead, BufReader};
        const MAX_FILE_BYTES: usize = 50 * 1024;
        const MAX_FILE_LINES: usize = 500;

        let abs_path = parsed.path.to_absolute(ctx.workspace_root);
        let file_size = std::fs::metadata(&abs_path).map(|m| m.len()).unwrap_or(0);
        
        let content = match std::fs::File::open(&abs_path) {
            Ok(file) => {
                let reader = BufReader::new(file);
                let mut text = String::new();
                let mut lines_count = 0usize;
                let mut bytes_read = 0usize;
                let mut was_truncated = false;

                let start = parsed.start_line.unwrap_or(1).max(1);
                let end = parsed.end_line.unwrap_or(usize::MAX);

                for (idx, line_res) in reader.lines().enumerate() {
                    let line_num = idx + 1;
                    match line_res {
                        Ok(line) => {
                            if line_num < start {
                                continue;
                            }
                            if line_num > end {
                                break;
                            }
                            // Format with line number for reference
                            let formatted = format!("{line_num}: {line}\n");
                            if bytes_read + formatted.len() > MAX_FILE_BYTES {
                                let allowed = MAX_FILE_BYTES.saturating_sub(bytes_read);
                                text.push_str(crow_patch::util::safe_truncate(&formatted, allowed));
                                was_truncated = true;
                                lines_count += 1;
                                break;
                            }
                            text.push_str(&formatted);
                            bytes_read += formatted.len();
                            lines_count += 1;

                            if lines_count >= MAX_FILE_LINES {
                                if file_size > bytes_read as u64 {
                                    was_truncated = true;
                                }
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }

                if was_truncated {
                    format!("{trimmed}\n\n[SYSTEM WARNING: File truncated. Original size: {file_size} bytes, showing {lines_count} lines.]\n\nIf you need to see more, use start_line/end_line to read specific ranges.", trimmed = text.trim_end())
                } else {
                    text.trim_end().to_string()
                }
            }
            Err(e) => {
                return Ok(ToolOutput::error(format!("Could not read file '{path}': {e}", path = parsed.path.as_str())));
            }
        };
        
        // Record file state for staleness tracking
        if let Some(ref store) = ctx.file_state {
            let mtime = crate::file_state::get_file_mtime(&abs_path).await;
            store.record(abs_path, mtime);
        }

        Ok(ToolOutput::success(content))
    }
}
