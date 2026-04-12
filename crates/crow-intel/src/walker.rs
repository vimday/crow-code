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

impl Default for RepoWalker {
    fn default() -> Self {
        Self::new()
    }
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
                        
                        let combined = format!("// ─── File: {} ────────────────────────\n{}\n\n", rel_path.display(), skeleton);
                        
                        if out.len() + combined.len() >= self.max_bytes {
                            let remaining = self.max_bytes.saturating_sub(out.len());
                            let safe_end = combined.char_indices()
                                .take_while(|&(idx, _)| idx < remaining)
                                .last()
                                .map(|(idx, ch)| idx + ch.len_utf8())
                                .unwrap_or(0);
                                
                            out.push_str(&combined[..safe_end]);
                            if !out.ends_with("\n\n... [REPO MAP TRUNCATED DUE TO BUDGET] ...\n") {
                                out.push_str("\n\n... [REPO MAP TRUNCATED DUE TO BUDGET] ...\n");
                            }
                            break; // Abort traversing further
                        } else {
                            out.push_str(&combined);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn build_repo_map_respects_hard_budget() {
        let dir = tempdir().unwrap();
        let file1 = dir.path().join("a.rs");
        let file2 = dir.path().join("b.rs");
        let file3 = dir.path().join("c.rs");
        
        fs::write(&file1, "fn a() { println!(\"a\"); }").unwrap();
        fs::write(&file2, "fn b() { println!(\"long padding string here \"); }").unwrap();
        fs::write(&file3, "fn c() { println!(\"c\"); }").unwrap(); // Should not fit
        
        // Very tight budget: 100 bytes
        let walker = RepoWalker::new().with_max_bytes(100);
        let map = walker.build_repo_map(dir.path()).unwrap();
        
        // Assert length won't exceed budget + truncation message
        let truncate_msg_len = "\n\n... [REPO MAP TRUNCATED DUE TO BUDGET] ...\n".len();
        assert!(map.map_text.len() <= 100 + truncate_msg_len, "Map text exceeded hard budget limits!");
        assert!(map.map_text.contains("TRUNCATED DUE TO"));
        assert!(map.map_text.contains("File: a.rs"));
        // c.rs should be fully cut off
        assert!(!map.map_text.contains("File: c.rs"));
    }
}
