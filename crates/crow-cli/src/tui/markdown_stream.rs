//! Streaming markdown renderer with delta updates.
//!
//! Ported from yomi's `markdown_stream.rs` — optimized for streaming content,
//! tracking state and only re-rendering when necessary.

use pulldown_cmark::{CodeBlockKind, Event as MdEvent, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use super::theme::{chars, colors, Styles};

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
            current_style: Style::default().fg(colors::text_primary()),
        }
    }
}

/// Streaming markdown renderer that supports incremental updates.
#[derive(Debug, Default)]
pub struct StreamingMarkdownRenderer {
    content: String,
    lines: Vec<Line<'static>>,
    state: ParseState,
    dirty: bool,
}

impl StreamingMarkdownRenderer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append new text and re-render.
    pub fn append(&mut self, text: &str) -> &[Line<'static>] {
        if text.is_empty() {
            return &self.lines;
        }
        self.content.push_str(text);
        self.dirty = true;
        self.render()
    }

    /// Set content and re-render from scratch.
    pub fn set_content(&mut self, content: String) -> &[Line<'static>] {
        self.content = content;
        self.lines.clear();
        self.state = ParseState::default();
        self.dirty = true;
        self.render()
    }

    /// Get current raw content.
    #[allow(dead_code)]
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Get rendered lines (re-render if dirty).
    pub fn lines(&mut self) -> &[Line<'static>] {
        if self.dirty {
            self.render();
        }
        &self.lines
    }

    /// Force re-render.
    fn render(&mut self) -> &[Line<'static>] {
        self.lines.clear();

        let options =
            Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH;

        let parser = Parser::new_ext(&self.content, options);

        let mut current_line: Vec<Span> = Vec::new();
        let mut in_code_block = self.state.in_code_block;
        let mut code_language = self.state.code_language.clone();
        let mut list_stack: Vec<ListState> = self.state.list_stack.clone();
        let mut current_style = self.state.current_style;

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
                            self.lines.push(Line::from(current_line));
                            current_line = Vec::new();
                        }
                        if let CodeBlockKind::Fenced(lang) = kind {
                            code_language = Some(lang.to_string());
                        }
                    }
                    Tag::List(start_num) => {
                        list_stack.push(
                            start_num.map_or(ListState::Unordered, |n| ListState::Ordered(n, n)),
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
                        current_line.push(Span::styled(
                            prefix,
                            Style::default().fg(colors::accent_user()),
                        ));
                    }
                    Tag::Heading { level, .. } => {
                        if !current_line.is_empty() {
                            self.lines.push(Line::from(current_line));
                            current_line = Vec::new();
                        }
                        self.lines.push(Line::from(""));
                        let prefix = match level {
                            pulldown_cmark::HeadingLevel::H1 => "# ",
                            pulldown_cmark::HeadingLevel::H2 => "## ",
                            pulldown_cmark::HeadingLevel::H3 => "### ",
                            pulldown_cmark::HeadingLevel::H4 => "#### ",
                            pulldown_cmark::HeadingLevel::H5 => "##### ",
                            pulldown_cmark::HeadingLevel::H6 => "###### ",
                        };
                        current_line.push(Span::styled(
                            prefix,
                            Style::default()
                                .fg(colors::text_primary())
                                .add_modifier(Modifier::BOLD),
                        ));
                        current_style = Style::default()
                            .fg(colors::text_primary())
                            .add_modifier(Modifier::BOLD);
                    }
                    Tag::BlockQuote(_) => {
                        current_line.push(Span::styled(
                            format!("{} ", chars::USER_BAR),
                            Style::default().fg(colors::border()),
                        ));
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
                            self.lines.push(Line::from(
                                current_line
                                    .into_iter()
                                    .map(|s| Span::styled(s.content, Styles::code_block()))
                                    .collect::<Vec<_>>(),
                            ));
                            current_line = Vec::new();
                        }
                        self.lines.push(Line::from(Span::styled(
                            format!(
                                "{}{}",
                                chars::CODE_BOTTOM_LEFT,
                                chars::CODE_HORIZONTAL.repeat(40),
                            ),
                            Style::default().fg(colors::code_border()),
                        )));
                        code_language = None;
                    }
                    TagEnd::Item => {
                        if !current_line.is_empty() {
                            self.lines.push(Line::from(current_line));
                            current_line = Vec::new();
                        }
                    }
                    TagEnd::List(_) => {
                        list_stack.pop();
                        if !self.lines.is_empty() {
                            self.lines.push(Line::from(""));
                        }
                    }
                    TagEnd::Heading(_) => {
                        if !current_line.is_empty() {
                            self.lines.push(Line::from(current_line));
                            current_line = Vec::new();
                        }
                        self.lines.push(Line::from(""));
                        current_style = Style::default().fg(colors::text_primary());
                    }
                    TagEnd::Paragraph => {
                        if !current_line.is_empty() {
                            self.lines.push(Line::from(current_line));
                            current_line = Vec::new();
                        }
                        self.lines.push(Line::from(""));
                    }
                    _ => {}
                },
                MdEvent::Text(text) => {
                    if in_code_block {
                        for line in text.lines() {
                            if current_line.is_empty() && code_language.is_some() {
                                let lang = code_language.take().unwrap_or_default();
                                self.lines.push(Line::from(vec![
                                    Span::styled(
                                        format!(
                                            "{}{} ",
                                            chars::CODE_TOP_LEFT,
                                            chars::CODE_HORIZONTAL.repeat(2),
                                        ),
                                        Style::default().fg(colors::code_border()),
                                    ),
                                    Span::styled(lang, Styles::code_lang()),
                                ]));
                            }
                            if !current_line.is_empty() {
                                self.lines.push(Line::from(
                                    current_line
                                        .into_iter()
                                        .map(|s| Span::styled(s.content, Styles::code_block()))
                                        .collect::<Vec<_>>(),
                                ));
                                current_line = Vec::new();
                            }
                            let expanded = line.replace('\t', "  ");
                            self.lines.push(Line::from(vec![
                                Span::styled(
                                    format!("{} ", chars::CODE_VERTICAL),
                                    Style::default().fg(colors::code_border()),
                                ),
                                Span::styled(expanded, Styles::code_block()),
                            ]));
                        }
                    } else {
                        current_line.push(Span::styled(text.to_string(), current_style));
                    }
                }
                MdEvent::Code(code) => {
                    let style = Styles::inline_code().patch(current_style);
                    current_line.push(Span::styled(format!("`{code}`"), style));
                }
                MdEvent::TaskListMarker(checked) => {
                    let checkbox = if checked { "[x]" } else { "[ ]" };
                    current_line.push(Span::styled(
                        format!("{checkbox} "),
                        Style::default().fg(if checked {
                            colors::accent_success()
                        } else {
                            colors::text_secondary()
                        }),
                    ));
                }
                MdEvent::SoftBreak | MdEvent::HardBreak => {
                    if in_code_block {
                        if !current_line.is_empty() {
                            self.lines.push(Line::from(
                                current_line
                                    .into_iter()
                                    .map(|s| Span::styled(s.content, Styles::code_block()))
                                    .collect::<Vec<_>>(),
                            ));
                            current_line = Vec::new();
                        }
                    } else if !current_line.is_empty() {
                        self.lines.push(Line::from(current_line));
                        current_line = Vec::new();
                    }
                }
                MdEvent::Rule => {
                    self.lines.push(Line::from(Span::styled(
                        "─".repeat(40),
                        Style::default().fg(colors::divider()),
                    )));
                }
                _ => {}
            }
        }

        // Flush remaining content
        if !current_line.is_empty() {
            if in_code_block {
                self.lines.push(Line::from(
                    current_line
                        .into_iter()
                        .map(|s| Span::styled(s.content, Styles::code_block()))
                        .collect::<Vec<_>>(),
                ));
            } else {
                self.lines.push(Line::from(current_line));
            }
        }

        // Remove trailing empty lines
        while self
            .lines
            .last()
            .is_some_and(|l| l.to_string().trim().is_empty())
        {
            self.lines.pop();
        }

        // Update state
        self.state = ParseState {
            in_code_block,
            code_language,
            list_stack,
            current_style,
        };
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
            .map(|l| l.to_string())
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
            .map(|l| l.to_string())
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
}
