//! Streaming markdown renderer with newline-gated commit pattern.
//!
//! Inspired by Codex's `MarkdownStreamCollector::commit_complete_lines` —
//! only re-parses content after the last committed newline, avoiding O(n²)
//! re-rendering as content accumulates during streaming.

use ratatui::style::Styled;

use pulldown_cmark::{CodeBlockKind, Event as MdEvent, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use super::theme::{chars, colors, Styles};

lazy_static::lazy_static! {
    static ref SYNTAX_SET: syntect::parsing::SyntaxSet = syntect::parsing::SyntaxSet::load_defaults_newlines();
    static ref THEME_SET: syntect::highlighting::ThemeSet = syntect::highlighting::ThemeSet::load_defaults();
}

fn translate_syn_style(style: syntect::highlighting::Style) -> Style {
    ratatui::style::Style::new().fg(ratatui::style::Color::Rgb(
        style.foreground.r,
        style.foreground.g,
        style.foreground.b,
    ))
}

/// Tracks the state of markdown parsing for incremental rendering.
#[derive(Debug, Clone, Copy)]
enum ListState {
    Ordered(u64, u64),
    Unordered,
}

#[derive(Debug)]
struct ParseState {
    in_code_block: bool,
    code_language: Option<String>,
    list_stack: Vec<ListState>,
    current_style: Style,
}

impl Default for ParseState {
    fn default() -> Self {
        Self {
            in_code_block: false,
            code_language: None,
            list_stack: Vec::new(),
            current_style: ratatui::style::Style::new().fg(colors::text_primary()),
        }
    }
}

/// Streaming markdown renderer that supports incremental updates.
///
/// Uses a newline-gated commit pattern (from Codex's `MarkdownStreamCollector`):
/// complete lines are "committed" and cached, so only the trailing partial line
/// needs re-rendering on each `append()` call. This reduces streaming overhead
/// from O(n²) to O(n) over the total content length.
#[derive(Debug, Default)]
pub struct StreamingMarkdownRenderer {
    content: String,
    /// Byte offset into `content` up to which lines have been committed.
    committed_offset: usize,
    /// Cached rendered lines from committed content.
    committed_lines: Vec<Line<'static>>,
    /// Full rendered output (committed + trailing partial).
    lines: Vec<Line<'static>>,
    state: ParseState,
    dirty: bool,
}

impl StreamingMarkdownRenderer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append new text and re-render only the uncommitted tail.
    pub fn append(&mut self, text: &str) -> &[Line<'static>] {
        if text.is_empty() {
            return &self.lines;
        }
        self.content.push_str(text);
        self.dirty = true;

        // Commit complete lines: find the last newline in content after committed_offset
        let tail = &self.content[self.committed_offset..];
        if let Some(last_nl) = tail.rfind('\n') {
            let new_committed_end = self.committed_offset + last_nl + 1;
            let chunk_to_commit = self.content[self.committed_offset..new_committed_end].to_string();

            if !chunk_to_commit.is_empty() {
                let new_lines = self.render_chunk(&chunk_to_commit);
                self.committed_lines.extend(new_lines);
                self.committed_offset = new_committed_end;
            }
        }

        // Now render the trailing partial (uncommitted) content
        let trailing = self.content[self.committed_offset..].to_string();
        let trailing_lines = if trailing.is_empty() {
            Vec::new()
        } else {
            self.render_chunk(&trailing)
        };

        // Combine committed + trailing
        self.lines.clone_from(&self.committed_lines);
        self.lines.extend(trailing_lines);

        // Trim trailing empty lines
        while self
            .lines
            .last()
            .is_some_and(|l| l.to_string().trim().is_empty())
        {
            self.lines.pop();
        }

        self.dirty = false;
        &self.lines
    }

    /// Set content and re-render from scratch.
    pub fn set_content(&mut self, content: String) -> &[Line<'static>] {
        self.content = content;
        self.committed_offset = 0;
        self.committed_lines.clear();
        self.lines.clear();
        self.state = ParseState::default();
        self.dirty = true;
        self.render_full()
    }

    /// Get current raw content.
    #[allow(dead_code)]
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Get rendered lines (re-render if dirty).
    pub fn lines(&mut self) -> &[Line<'static>] {
        if self.dirty {
            self.render_full();
        }
        &self.lines
    }

    /// Render a chunk of markdown text into Lines.
    fn render_chunk(&mut self, text: &str) -> Vec<Line<'static>> {
        let mut result = Vec::new();
        let options =
            Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH;

        let parser = Parser::new_ext(text, options);

        let mut current_line: Vec<Span> = Vec::new();
        let mut in_code_block = self.state.in_code_block;
        let mut code_language = self.state.code_language.clone();
        let mut list_stack: Vec<ListState> = self.state.list_stack.clone();
        let mut current_style = self.state.current_style;
        let mut highlighter: Option<syntect::easy::HighlightLines<'_>> = None;

        // Re-init highlighter if we're already inside a code block
        if in_code_block {
            let lang = code_language.as_deref().unwrap_or("");
            let syntax = SYNTAX_SET
                .find_syntax_by_token(lang)
                .unwrap_or_else(|| SYNTAX_SET.find_syntax_plain_text());
            let theme = &THEME_SET.themes["base16-ocean.dark"];
            highlighter = Some(syntect::easy::HighlightLines::new(syntax, theme));
        }

        for event in parser {
            match event {
                MdEvent::Start(tag) => match tag {
                    Tag::Strong => {
                        current_style = current_style.add_modifier(Modifier::BOLD);
                    }
                    Tag::Strikethrough => {
                        current_style = current_style.add_modifier(Modifier::CROSSED_OUT);
                    }
                    Tag::Emphasis => {
                        current_style = current_style.add_modifier(Modifier::ITALIC);
                    }
                    Tag::CodeBlock(kind) => {
                        in_code_block = true;
                        if !current_line.is_empty() {
                            result.push(Line::from(current_line));
                            current_line = Vec::new();
                        }
                        if let CodeBlockKind::Fenced(lang) = kind {
                            let lang_str = lang.to_string();
                            code_language = Some(lang_str.clone());
                            let syntax = SYNTAX_SET
                                .find_syntax_by_token(&lang_str)
                                .unwrap_or_else(|| SYNTAX_SET.find_syntax_plain_text());
                            let theme = &THEME_SET.themes["base16-ocean.dark"];
                            highlighter =
                                Some(syntect::easy::HighlightLines::new(syntax, theme));
                        } else {
                            let syntax = SYNTAX_SET.find_syntax_plain_text();
                            let theme = &THEME_SET.themes["base16-ocean.dark"];
                            highlighter =
                                Some(syntect::easy::HighlightLines::new(syntax, theme));
                        }
                    }
                    Tag::List(start_num) => {
                        list_stack.push(
                            start_num
                                .map_or(ListState::Unordered, |n| ListState::Ordered(n, n)),
                        );
                    }
                    Tag::Item => {
                        let indent = "  ".repeat(list_stack.len().saturating_sub(1));
                        let prefix = match list_stack.last_mut() {
                            Some(ListState::Ordered(_start, current)) => {
                                let num = *current;
                                *current += 1;
                                format!("{indent}{num}. ")
                            }
                            Some(ListState::Unordered) => {
                                format!("{indent}{} ", chars::BULLET)
                            }
                            None => format!("{} ", chars::BULLET),
                        };
                        current_line.push((prefix).set_style(ratatui::style::Style::new().fg(colors::accent_user()),));
                    }
                    Tag::Heading { level, .. } => {
                        if !current_line.is_empty() {
                            result.push(Line::from(current_line));
                            current_line = Vec::new();
                        }
                        result.push(Line::from(""));
                        let prefix = match level {
                            pulldown_cmark::HeadingLevel::H1 => "# ",
                            pulldown_cmark::HeadingLevel::H2 => "## ",
                            pulldown_cmark::HeadingLevel::H3 => "### ",
                            pulldown_cmark::HeadingLevel::H4 => "#### ",
                            pulldown_cmark::HeadingLevel::H5 => "##### ",
                            pulldown_cmark::HeadingLevel::H6 => "###### ",
                        };
                        current_line.push((prefix).set_style(ratatui::style::Style::new()
                                .fg(colors::text_primary())
                                .add_modifier(Modifier::BOLD),));
                        current_style = ratatui::style::Style::new()
                            .fg(colors::text_primary())
                            .add_modifier(Modifier::BOLD);
                    }
                    Tag::BlockQuote(_) => {
                        current_line.push((format!("{} ", chars::USER_BAR)).set_style(ratatui::style::Style::new().fg(colors::border()),));
                    }
                    _ => {}
                },
                MdEvent::End(tag_end) => match tag_end {
                    TagEnd::Strong => {
                        current_style = current_style.remove_modifier(Modifier::BOLD);
                    }
                    TagEnd::Strikethrough => {
                        current_style = current_style.remove_modifier(Modifier::CROSSED_OUT);
                    }
                    TagEnd::Emphasis => {
                        current_style = current_style.remove_modifier(Modifier::ITALIC);
                    }
                    TagEnd::CodeBlock => {
                        in_code_block = false;
                        if !current_line.is_empty() {
                            result.push(Line::from(
                                current_line
                                    .into_iter()
                                    .map(|s| s.set_style(Styles::code_block()))
                                    .collect::<Vec<_>>(),
                            ));
                            current_line = Vec::new();
                        }
                        result.push(Line::from((format!(
                                "{}{}",
                                chars::CODE_BOTTOM_LEFT,
                                chars::CODE_HORIZONTAL.repeat(40),
                            )).set_style(ratatui::style::Style::new().fg(colors::code_border()),)));
                        code_language = None;
                    }
                    TagEnd::Item if !current_line.is_empty() => {
                        result.push(Line::from(current_line));
                        current_line = Vec::new();
                    }
                    TagEnd::List(_) => {
                        list_stack.pop();
                        if !result.is_empty() {
                            result.push(Line::from(""));
                        }
                    }
                    TagEnd::Heading(_) => {
                        if !current_line.is_empty() {
                            result.push(Line::from(current_line));
                            current_line = Vec::new();
                        }
                        result.push(Line::from(""));
                        current_style = ratatui::style::Style::new().fg(colors::text_primary());
                    }
                    TagEnd::Paragraph => {
                        if !current_line.is_empty() {
                            result.push(Line::from(current_line));
                            current_line = Vec::new();
                        }
                        result.push(Line::from(""));
                    }
                    _ => {}
                },
                MdEvent::Text(text) => {
                    if in_code_block {
                        for line in text.lines() {
                            if current_line.is_empty() && code_language.is_some() {
                                let lang = code_language.take().unwrap_or_default();
                                result.push(Line::from(vec![
                                    (format!(
                                            "{}{} ",
                                            chars::CODE_TOP_LEFT,
                                            chars::CODE_HORIZONTAL.repeat(2),
                                        )).set_style(ratatui::style::Style::new().fg(colors::code_border()),),
                                    (lang).set_style(Styles::code_lang()),
                                ]));
                            }
                            if !current_line.is_empty() {
                                result.push(Line::from(
                                    current_line
                                        .into_iter()
                                        .map(|s| {
                                            s.set_style(Styles::code_block())
                                        })
                                        .collect::<Vec<_>>(),
                                ));
                                current_line = Vec::new();
                            }
                            let expanded = line.replace('\t', "  ");
                            let mut spans = vec![(format!("{} ", chars::CODE_VERTICAL)).set_style(ratatui::style::Style::new().fg(colors::code_border()),)];

                            if let Some(hl) = highlighter.as_mut() {
                                match hl.highlight_line(&expanded, &SYNTAX_SET) {
                                    Ok(ranges) => {
                                        for (style, s) in ranges {
                                            spans.push((s.to_string()).set_style(translate_syn_style(style),));
                                        }
                                    }
                                    Err(_) => {
                                        spans.push((expanded).set_style(Styles::code_block(),));
                                    }
                                }
                            } else {
                                spans.push((expanded).set_style(Styles::code_block()));
                            }

                            result.push(Line::from(spans));
                        }
                    } else {
                        current_line.push((text.to_string()).set_style(current_style));
                    }
                }
                MdEvent::Code(code) => {
                    let style = Styles::inline_code().patch(current_style);
                    current_line.push((format!("`{code}`")).set_style(style));
                }
                MdEvent::TaskListMarker(checked) => {
                    let checkbox = if checked { "[x]" } else { "[ ]" };
                    current_line.push((format!("{checkbox} ")).set_style(ratatui::style::Style::new().fg(if checked {
                            colors::accent_success()
                        } else {
                            colors::text_secondary()
                        }),));
                }
                MdEvent::SoftBreak | MdEvent::HardBreak => {
                    if in_code_block {
                        if !current_line.is_empty() {
                            result.push(Line::from(
                                current_line
                                    .into_iter()
                                    .map(|s| s.set_style(Styles::code_block()))
                                    .collect::<Vec<_>>(),
                            ));
                            current_line = Vec::new();
                        }
                    } else if !current_line.is_empty() {
                        result.push(Line::from(current_line));
                        current_line = Vec::new();
                    }
                }
                MdEvent::Rule => {
                    result.push(Line::from(("─".repeat(40)).set_style(ratatui::style::Style::new().fg(colors::divider()),)));
                }
                _ => {}
            }
        }

        // Flush remaining content
        if !current_line.is_empty() {
            if in_code_block {
                result.push(Line::from(
                    current_line
                        .into_iter()
                        .map(|s| s.set_style(Styles::code_block()))
                        .collect::<Vec<_>>(),
                ));
            } else {
                result.push(Line::from(current_line));
            }
        }

        // Update state for next chunk
        self.state = ParseState {
            in_code_block,
            code_language,
            list_stack,
            current_style,
        };

        result
    }

    /// Full re-render from scratch (used by set_content and initial load).
    fn render_full(&mut self) -> &[Line<'static>] {
        self.committed_offset = 0;
        self.committed_lines.clear();
        self.state = ParseState::default();

        let content = self.content.clone();
        let rendered = self.render_chunk(&content);
        self.lines = rendered;

        // Commit everything
        self.committed_offset = self.content.len();
        self.committed_lines.clone_from(&self.lines);

        // Trim trailing empty lines
        while self
            .lines
            .last()
            .is_some_and(|l| l.to_string().trim().is_empty())
        {
            self.lines.pop();
        }

        self.dirty = false;
        &self.lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_text() {
        let mut r = StreamingMarkdownRenderer::new();
        let lines = r.set_content("Hello world".to_string());
        assert!(!lines.is_empty());
    }

    #[test]
    fn test_code_block() {
        let mut r = StreamingMarkdownRenderer::new();
        let lines = r.set_content("```rust\nfn main() {}\n```".to_string());
        let output: String = lines
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(output.contains("fn main()"));
    }

    #[test]
    fn test_streaming_append() {
        let mut r = StreamingMarkdownRenderer::new();
        r.append("Hello ");
        let lines = r.append("world");
        let output: String = lines
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(output.contains("Hello world"));
    }

    #[test]
    fn test_heading() {
        let mut r = StreamingMarkdownRenderer::new();
        let lines = r.set_content("# Title\n\nBody text".to_string());
        assert!(lines.len() >= 2);
    }

    #[test]
    fn test_list() {
        let mut r = StreamingMarkdownRenderer::new();
        let lines = r.set_content("- item 1\n- item 2\n- item 3".to_string());
        assert!(lines.len() >= 3);
    }

    #[test]
    fn test_newline_gated_commit() {
        let mut r = StreamingMarkdownRenderer::new();
        // First append doesn't have a newline — no commit
        r.append("line one");
        assert_eq!(r.committed_offset, 0);
        // Now append with newline — triggers commit
        r.append("\nline two");
        assert!(r.committed_offset > 0);
    }

    #[test]
    fn test_incremental_contains_key_content() {
        let full_content = "# Hello\n\nSome **bold** text.\n\n- item a\n- item b\n";

        // Full render
        let mut full = StreamingMarkdownRenderer::new();
        let full_lines = full.set_content(full_content.to_string());
        let full_output: String = full_lines
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        // Incremental render (line by line — the realistic streaming unit)
        let mut inc = StreamingMarkdownRenderer::new();
        let mut last_output = String::new();
        for line in full_content.lines() {
            let rendered = inc.append(&format!("{line}\n"));
            last_output = rendered
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
        }

        // Both should contain the key content elements
        assert!(full_output.contains("Hello"), "full missing Hello");
        assert!(last_output.contains("Hello"), "inc missing Hello");
        assert!(full_output.contains("bold"), "full missing bold");
        assert!(last_output.contains("bold"), "inc missing bold");
        assert!(full_output.contains("item a"), "full missing item a");
        assert!(last_output.contains("item a"), "inc missing item a");
    }
}
