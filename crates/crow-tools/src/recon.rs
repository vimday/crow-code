use crate::{Tool, ToolContext};
use anyhow::Result;

pub struct ListDirTool;

#[async_trait::async_trait]
impl Tool for ListDirTool {
    fn name(&self) -> &'static str {
        "list_dir"
    }

    fn description(&self) -> &'static str {
        "List directory contents"
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> Result<String> {
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
            ctx.frozen_root,
            &v_cmd,
            &exec_config,
            &crow_verifier::types::AciConfig::compact(),
            None,
        ).await?;
        Ok(result.test_run.truncated_log)
    }
}

pub struct SearchTool;

#[async_trait::async_trait]
impl Tool for SearchTool {
    fn name(&self) -> &'static str {
        "search"
    }

    fn description(&self) -> &'static str {
        "Search for a pattern across files"
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> Result<String> {
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
            ctx.frozen_root,
            &v_cmd,
            &exec_config,
            &crow_verifier::types::AciConfig::compact(),
            None,
        ).await?;
        Ok(result.test_run.truncated_log)
    }
}

pub struct FetchUrlTool;

#[async_trait::async_trait]
impl Tool for FetchUrlTool {
    fn name(&self) -> &'static str {
        "fetch_url"
    }

    fn description(&self) -> &'static str {
        "Fetch and process the content of a public URL"
    }

    async fn execute(&self, args: serde_json::Value, _ctx: &ToolContext<'_>) -> Result<String> {
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
            anyhow::bail!("{url} returned HTTP {status}", url = parsed.url);
        }
        if let Some(ct) = res.headers().get(reqwest::header::CONTENT_TYPE) {
            let ct_str = ct.to_str().unwrap_or("");
            if !ct_str.contains("text/") && !ct_str.contains("application/json") {
                anyhow::bail!("Unsupported Content-Type '{ct_str}'. Only text or json supported.");
            }
        }

        let mut text = res.text().await?;
        if text.len() > max_fetch_bytes {
            text.truncate(max_fetch_bytes);
            text.push_str("...\n\n[SYSTEM WARNING: Response truncated to 50KB]");
        }
        Ok(text)
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
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> Result<String> {
        #[derive(serde::Deserialize)]
        struct Args { path: crow_patch::WorkspacePath }
        let parsed: Args = serde_json::from_value(args)?;
        let v_cmd = crow_probe::VerificationCommand {
            program: "file".to_string(),
            args: vec!["--".to_string(), parsed.path.as_str().to_string()],
            cwd: None,
        };
        let exec_config = crow_verifier::ExecutionConfig { timeout: std::time::Duration::from_secs(10), max_output_bytes: 512 * 1024 };
        let result = crow_verifier::executor::execute(ctx.frozen_root, &v_cmd, &exec_config, &crow_verifier::types::AciConfig::compact(), None).await?;
        Ok(result.test_run.truncated_log)
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
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> Result<String> {
        #[derive(serde::Deserialize)]
        struct Args { path: crow_patch::WorkspacePath }
        let parsed: Args = serde_json::from_value(args)?;
        let v_cmd = crow_probe::VerificationCommand {
            program: "wc".to_string(),
            args: vec!["-l".to_string(), "-c".to_string(), "--".to_string(), parsed.path.as_str().to_string()],
            cwd: None,
        };
        let exec_config = crow_verifier::ExecutionConfig { timeout: std::time::Duration::from_secs(10), max_output_bytes: 512 * 1024 };
        let result = crow_verifier::executor::execute(ctx.frozen_root, &v_cmd, &exec_config, &crow_verifier::types::AciConfig::compact(), None).await?;
        Ok(result.test_run.truncated_log)
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
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> Result<String> {
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
        let result = crow_verifier::executor::execute(ctx.frozen_root, &v_cmd, &exec_config, &crow_verifier::types::AciConfig::compact(), None).await?;
        Ok(result.test_run.truncated_log)
    }
}

pub struct ReadFilesTool;

#[async_trait::async_trait]
impl Tool for ReadFilesTool {
    fn name(&self) -> &'static str {
        "read_files"
    }
    fn description(&self) -> &'static str {
        "Read multiple files from the workspace"
    }
    async fn execute(&self, args: serde_json::Value, ctx: &ToolContext<'_>) -> Result<String> {
        #[derive(serde::Deserialize)]
        struct Args { paths: Vec<crow_patch::WorkspacePath> }
        let parsed: Args = serde_json::from_value(args)?;
        
        use std::io::{BufRead, BufReader};
        const MAX_FILE_BYTES: u64 = 50 * 1024;
        const MAX_FILE_LINES: usize = 500;

        let mut file_contents = String::from("[READ FILES RESULT]\n");
        for path in parsed.paths {
            let abs_path = path.to_absolute(ctx.frozen_root);
            let file_size = std::fs::metadata(&abs_path).map(|m| m.len()).unwrap_or(0);
            
            let content = match std::fs::File::open(&abs_path) {
                Ok(file) => {
                    let reader = BufReader::new(file);
                    let mut text = String::new();
                    let mut lines_count = 0;
                    let mut bytes_read = 0;
                    let mut was_truncated = false;

                    for line_res in reader.lines() {
                        match line_res {
                            Ok(line) => {
                                if bytes_read + line.len() > MAX_FILE_BYTES as usize {
                                    let allowed = (MAX_FILE_BYTES as usize).saturating_sub(bytes_read);
                                    text.push_str(crow_patch::util::safe_truncate(&line, allowed));
                                    was_truncated = true;
                                    lines_count += 1;
                                    break;
                                }
                                text.push_str(&line);
                                text.push('\n');
                                bytes_read += line.len() + 1;
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
                        format!("{}\n\n[SYSTEM WARNING: File truncated. Original size: {} bytes, showing first {} lines only.]", text.trim_end(), file_size, lines_count)
                    } else {
                        text.trim_end().to_string()
                    }
                }
                Err(_) => "<file not found or unreadable>".into(),
            };
            file_contents.push_str(&format!("--- {} ---\n{}\n\n", path.as_str(), content));
        }
        
        file_contents.push_str("Please proceed with your task, or read more files if necessary.");
        Ok(file_contents)
    }
}
