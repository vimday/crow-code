//! File tree walker and repository context map generator.
//!
//! Provides the `RepoWalker` tool for breadth-first searching of workspaces
//! and concatenating file skeletons up to a configurable budget.

use crate::pagerank::{ContentMap, SymbolMap};
use crate::skeleton::ASTProcessor;
use std::path::Path;

pub struct ContextMap {
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

    /// Walk the workspace using breadth-first traversal.
    /// At each directory level, files are emitted before subdirectories,
    /// ensuring shallow/important files (Cargo.toml, src/main.rs) are seen
    /// by the LLM before deep nested paths consume the budget.
    pub fn build_context_map(&self, root: &Path) -> Result<ContextMap, String> {
        let mut file_contents: ContentMap = std::collections::HashMap::new();
        let mut file_definitions: SymbolMap = std::collections::HashMap::new();

        self.collect_all_files(root, &mut file_contents, &mut file_definitions);

        let active_files = std::collections::HashSet::new(); // Currently un-personalized, future proof for active context
        let ranked_files = crate::pagerank::compute_personalized_pagerank(
            &file_definitions,
            &file_contents,
            &active_files,
        );

        let mut out = String::new();
        for (path, _) in ranked_files {
            let source = &file_contents[&path];
            let skeleton = self
                .processor
                .generate_skeleton(source, &path)
                .unwrap_or_else(|_| String::new());

            let rel_path = path.strip_prefix(root).unwrap_or(&path);
            let combined = format!(
                "// ─── File: {} ────────────────────────\n{}\n\n",
                rel_path.display(),
                skeleton
            );

            if out.len() + combined.len() > self.max_bytes {
                let remaining = self.max_bytes.saturating_sub(out.len());
                let safe_end = combined
                    .char_indices()
                    .take_while(|&(idx, _)| idx < remaining)
                    .last()
                    .map(|(idx, ch)| idx + ch.len_utf8())
                    .unwrap_or(0);
                out.push_str(&combined[..safe_end]);
                Self::append_truncation(&mut out);
                return Ok(ContextMap { map_text: out });
            }
            out.push_str(&combined);
        }

        Ok(ContextMap { map_text: out })
    }

    fn collect_all_files(
        &self,
        root: &Path,
        file_contents: &mut ContentMap,
        file_definitions: &mut SymbolMap,
    ) {
        let skip = ["target", "node_modules", ".git", "vendor", ".venv"];
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(root.to_path_buf());

        while let Some(dir) = queue.pop_front() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(_) => continue,
            };

            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                if path.is_dir() {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    if !skip.contains(&name.as_ref()) {
                        queue.push_back(path);
                    }
                } else if path.is_file() {
                    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

                    let is_allowed_ext = [
                        "rs", "ts", "js", "jsx", "tsx", "toml", "json", "yaml", "yml", "md", "sh",
                        "py", "go", "c", "cpp", "h", "txt",
                    ]
                    .contains(&ext);

                    let is_allowed_name = [
                        "Makefile",
                        "Dockerfile",
                        ".gitignore",
                        ".dockerignore",
                        "LICENSE",
                    ]
                    .contains(&file_name);

                    if is_allowed_ext || is_allowed_name {
                        if let Ok(source) = std::fs::read_to_string(&path) {
                            file_definitions.insert(
                                path.clone(),
                                self.processor.extract_definitions(&source, &path),
                            );
                            file_contents.insert(path, source);
                        }
                    }
                }
            }
        }
    }

    fn append_truncation(out: &mut String) {
        let marker = "\n\n... [REPO MAP TRUNCATED DUE TO BUDGET] ...\n";
        if !out.ends_with(marker) {
            out.push_str(marker);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn build_context_map_respects_hard_budget() {
        let dir = tempdir().unwrap();
        let file1 = dir.path().join("a.rs");
        let file2 = dir.path().join("b.rs");
        let file3 = dir.path().join("c.rs");

        std::fs::write(&file1, "fn a() { println!(\"a\"); }").unwrap();
        std::fs::write(
            &file2,
            "fn b() { println!(\"long padding string here \"); }",
        )
        .unwrap();
        std::fs::write(&file3, "fn c() { println!(\"c\"); }").unwrap(); // Should not fit

        // Very tight budget: 100 bytes
        let walker = RepoWalker::new().with_max_bytes(100);
        let map = walker.build_context_map(dir.path()).unwrap();

        // Assert length won't exceed budget + truncation message
        let truncate_msg_len = "\n\n... [REPO MAP TRUNCATED DUE TO BUDGET] ...\n".len();
        assert!(
            map.map_text.len() <= 100 + truncate_msg_len,
            "Map text exceeded hard budget limits!"
        );
        assert!(map.map_text.contains("TRUNCATED DUE TO"));
    }
}
