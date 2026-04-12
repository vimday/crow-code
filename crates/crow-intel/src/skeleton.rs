use std::path::Path;
use tree_sitter::{Language, Node, Parser};

pub enum SupportedLanguage {
    Rust,
    TypeScript,
    JavaScript,
}

impl SupportedLanguage {
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "rs" => Some(SupportedLanguage::Rust),
            "ts" | "tsx" => Some(SupportedLanguage::TypeScript),
            "js" | "jsx" => Some(SupportedLanguage::JavaScript),
            _ => None,
        }
    }

    pub fn tree_sitter_lang(&self) -> Language {
        match self {
            SupportedLanguage::Rust => tree_sitter_rust::language(),
            SupportedLanguage::TypeScript => tree_sitter_typescript::language_typescript(),
            SupportedLanguage::JavaScript => tree_sitter_typescript::language_tsx(), // TSX handles JS well enough for signatures, or we could use tree-sitter-javascript but let's stick to TS for now.
        }
    }
}

pub struct ASTProcessor;

impl Default for ASTProcessor {
    fn default() -> Self {
        Self
    }
}

impl ASTProcessor {
    pub fn new() -> Self {
        Self
    }

    /// Takes source code and its file path, returns the LOD 1 Skeletal representation.
    /// If the language is unsupported, it currently just returns the original source.
    pub fn generate_skeleton(&self, source: &str, path: &Path) -> Result<String, String> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

        let lang = match SupportedLanguage::from_extension(ext) {
            Some(l) => l,
            None => return Ok(source.to_string()),
        };

        let mut parser = Parser::new();
        parser
            .set_language(&lang.tree_sitter_lang())
            .map_err(|e| e.to_string())?;

        let tree = parser.parse(source, None).ok_or("Failed to parse AST")?;
        let root = tree.root_node();

        // Find all spans we want to replace with "{ ... }"
        let mut replacements = Vec::new();
        Self::collect_body_spans(root, &mut replacements);

        // Sort by start byte so we can process linearly
        replacements.sort_by_key(|span| span.0);

        let mut result = String::with_capacity(source.len());
        let mut last_end = 0;

        for (start, end) in replacements {
            if start >= last_end {
                result.push_str(&source[last_end..start]);
                result.push_str("{ ... }");
                last_end = end;
            }
        }

        if last_end < source.len() {
            result.push_str(&source[last_end..]);
        }

        Ok(result)
    }

    fn collect_body_spans(node: Node<'_>, spans: &mut Vec<(usize, usize)>) {
        let kind = node.kind();

        // Check if this node is a block that implements a function body
        let is_target_block = match kind {
            "block" => {
                // Rust: parent is function_item
                node.parent().is_some_and(|p| p.kind() == "function_item")
            }
            "statement_block" => {
                // TS/JS: parent is function_declaration, method_definition, arrow_function, etc.
                node.parent().is_some_and(|p| {
                    matches!(
                        p.kind(),
                        "function_declaration" | "method_definition" | "arrow_function"
                    )
                })
            }
            _ => false,
        };

        if is_target_block {
            spans.push((node.start_byte(), node.end_byte()));
        } else {
            // Recurse
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                Self::collect_body_spans(child, spans);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeletonize_rust_functions() {
        let source = r#"
/// A person struct
struct Person(String);

impl Person {
    pub fn new() -> Self {
        let mut s = String::new();
        s.push_str("test");
        Self(s)
    }
}

fn global_fn() {
    println!("Hello");
}
"#;

        let processor = ASTProcessor::new();
        let path = Path::new("main.rs");
        let skeleton = processor.generate_skeleton(source, path).unwrap();

        assert!(skeleton.contains("pub fn new() -> Self { ... }"));
        assert!(skeleton.contains("fn global_fn() { ... }"));
        // Make sure inner code is gone
        assert!(!skeleton.contains("println"));
        assert!(!skeleton.contains("push_str"));
        // Structs exist
        assert!(skeleton.contains("struct Person(String);"));
    }

    #[test]
    fn skeletonize_ts_functions() {
        let source = r#"
class Person {
    constructor() {
        this.name = "test";
    }
    
    sayHi() {
        console.log(this.name);
    }
}

function globalFn() {
    console.log("Hello");
}

const arrow = () => {
    return 42;
};
"#;

        let processor = ASTProcessor::new();
        let path = Path::new("main.ts");
        let skeleton = processor.generate_skeleton(source, path).unwrap();

        assert!(skeleton.contains("constructor() { ... }"));
        assert!(skeleton.contains("sayHi() { ... }"));
        assert!(skeleton.contains("function globalFn() { ... }"));
        assert!(skeleton.contains("const arrow = () => { ... }"));
        assert!(!skeleton.contains("console.log"));
    }
}
