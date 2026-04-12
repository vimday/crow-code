use crate::skeleton::ASTProcessor;
use std::fs;
use std::path::Path;

pub struct RepoMap {
    pub map_text: String,
}

pub struct RepoWalker {
    processor: ASTProcessor,
    max_bytes: usize,
}

impl RepoWalker {
    pub fn new() -> Self {
        Self {
            processor: ASTProcessor::new(),
            // Set 500KB default limit to prevent monorepo string explosion
            max_bytes: 500 * 1024,
        }
    }

    pub fn with_max_bytes(mut self, max: usize) -> Self {
        self.max_bytes = max;
        self
    }

    /// Walk the workspace, skip target/ and node_modules/, and build a combined map up to max_bytes.
    pub fn build_repo_map(&self, root: &Path) -> Result<RepoMap, String> {
        let mut map_text = String::new();
        self.walk_dir(root, root, &mut map_text)?;
        Ok(RepoMap { map_text })
    }

    fn walk_dir(&self, dir: &Path, root: &Path, out: &mut String) -> Result<(), String> {
        if out.len() >= self.max_bytes {
            return Ok(());
        }

        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return Ok(()), // gracefully skip unreadable dirs
        };

        let mut paths: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
        paths.sort();

        for path in paths {
            if out.len() >= self.max_bytes {
                if !out.ends_with("\n\n... [REPO MAP TRUNCATED DUE TO BUDGET] ...\n") {
                    out.push_str("\n\n... [REPO MAP TRUNCATED DUE TO BUDGET] ...\n");
                }
                break;
            }

            if path.is_dir() {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                if name == "target" || name == "node_modules" || name == ".git" || name == "vendor" || name == ".venv" {
                    continue;
                }
                self.walk_dir(&path, root, out)?;
            } else if path.is_file() {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if ["rs", "ts", "js", "jsx", "tsx"].contains(&ext) {
                    if let Ok(source) = fs::read_to_string(&path) {
                        let rel_path = path.strip_prefix(root).unwrap_or(&path);
                        let skeleton = self.processor.generate_skeleton(&source, &path).unwrap_or(source);
                        
                        out.push_str(&format!("// ─── File: {} ────────────────────────\n", rel_path.display()));
                        out.push_str(&skeleton);
                        out.push_str("\n\n");
                    }
                }
            }
        }
        Ok(())
    }
}
