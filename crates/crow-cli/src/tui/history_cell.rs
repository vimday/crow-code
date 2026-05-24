//! Polymorphic history cell types (Codex `HistoryCell` pattern).
//!
//! Each cell knows how to render itself at a given width and report its
//! desired height. This replaces the flat `Cell { kind, payload }` model
//! with a trait-based system where each cell type controls its own rendering.

use ratatui::style::{Color, Styled, Stylize};
use ratatui::text::Line;
use std::fmt;

use super::theme::{chars, colors, Styles};
use super::markdown_stream;

/// Left gutter matching Codex's LIVE_PREFIX_COLS.
const GUTTER: &str = "  ";

// ── Trait ────────────────────────────────────────────────────────────────────

/// A polymorphic display unit in the conversation history.
///
/// Concrete implementations render themselves differently (user messages
/// get a tinted background, agent messages use markdown, evidence cells
/// are dim, etc.) but all satisfy this trait so the history pane can
/// render them uniformly via dynamic dispatch.
pub trait HistoryCell: fmt::Debug + Send + Sync {
    /// Render this cell as styled ratatui `Line`s at the given viewport width.
    fn display_lines(&self, width: u16) -> Vec<Line<'static>>;

    /// Report how many terminal rows this cell occupies at the given width.
    /// Used for scroll-offset calculations and viewport layout.
    fn desired_height(&self, width: u16) -> u16 {
        self.display_lines(width).len() as u16
    }

    /// Whether this cell is a continuation of a prior streaming chunk
    /// (used to suppress duplicate headers during streaming).
    fn is_stream_continuation(&self) -> bool {
        false
    }

    /// A short label for the cell kind (used in transcript overlays / debugging).
    fn kind_label(&self) -> &'static str;

    /// Raw text content (for search, copy, etc.).
    fn raw_text(&self) -> &str;
}

// ── Concrete Cell Types ──────────────────────────────────────────────────────

/// User-authored prompt. Rendered with `› ` prefix and subtle tinted background.
#[derive(Debug, Clone)]
pub struct UserMessageCell {
    pub payload: String,
}

impl HistoryCell for UserMessageCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let wrap_width = width.saturating_sub(4).max(1) as usize;
        let wrapped = textwrap::wrap(&self.payload, wrap_width);

        let style = Styles::user_content().bg(colors::user_msg_bg());
        let prefix_style = Styles::user_header().bg(colors::user_msg_bg());

        // Top padding (tinted)
        lines.push(Line::from("").style(style));

        for (i, line) in wrapped.iter().enumerate() {
            let prefix = if i == 0 {
                "› ".set_style(prefix_style)
            } else {
                "  ".set_style(prefix_style)
            };
            lines.push(
                Line::from(vec![prefix, line.to_string().set_style(style)]).style(style),
            );
        }

        // Bottom padding (tinted) + untinted spacer
        lines.push(Line::from("").style(style));
        lines.push(Line::from(""));

        lines
    }

    fn kind_label(&self) -> &'static str {
        "User"
    }

    fn raw_text(&self) -> &str {
        &self.payload
    }
}

/// Agent markdown response. Rendered with `• ` prefix and full markdown styling.
#[derive(Debug, Clone)]
pub struct AgentMessageCell {
    pub payload: String,
    pub is_continuation: bool,
}

impl HistoryCell for AgentMessageCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        let mut renderer = markdown_stream::StreamingMarkdownRenderer::new();
        let md_lines = renderer.set_content(self.payload.clone());
        let mut out = Vec::new();
        for (i, line) in md_lines.iter().enumerate() {
            let prefix = if i == 0 && !self.is_continuation {
                "• ".set_style(Styles::assistant_content().dim())
            } else {
                "  ".set_style(Styles::assistant_content())
            };

            let mut new_spans = vec![prefix];
            for span in line.spans.iter() {
                new_spans.push(span.clone());
            }
            out.push(Line::from(new_spans));
        }
        if out.is_empty() {
            out.push(Line::from(vec![
                "• ".set_style(Styles::assistant_content().dim()),
                self.payload.clone().set_style(Styles::assistant_content()),
            ]));
        }
        out
    }

    fn is_stream_continuation(&self) -> bool {
        self.is_continuation
    }

    fn kind_label(&self) -> &'static str {
        "Agent"
    }

    fn raw_text(&self) -> &str {
        &self.payload
    }
}

/// Evidence trace (file reads, recon). Dim, secondary information.
#[derive(Debug, Clone)]
pub struct EvidenceCell {
    pub payload: String,
}

impl HistoryCell for EvidenceCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let wrap_width = width.saturating_sub(6).max(1) as usize;
        let wrapped = textwrap::wrap(&self.payload, wrap_width);
        for (i, line) in wrapped.iter().enumerate() {
            let prefix = if i == 0 {
                format!("{GUTTER}{} ", chars::BULLET)
            } else {
                format!("{GUTTER}  ")
            };
            lines.push(Line::from(vec![
                prefix.set_style(Styles::evidence()),
                line.to_string().set_style(Styles::evidence()),
            ]));
        }
        lines
    }

    fn kind_label(&self) -> &'static str {
        "Evidence"
    }

    fn raw_text(&self) -> &str {
        &self.payload
    }
}

/// Tool/action execution trace.
#[derive(Debug, Clone)]
pub struct ActionCell {
    pub payload: String,
}

impl HistoryCell for ActionCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let wrap_width = width.saturating_sub(6).max(1) as usize;
        let wrapped = textwrap::wrap(&self.payload, wrap_width);
        for (i, line) in wrapped.iter().enumerate() {
            let prefix = if i == 0 {
                format!("{GUTTER}↳ ")
            } else {
                format!("{GUTTER}  ")
            };
            lines.push(Line::from(vec![
                prefix.set_style(Styles::success()),
                line.to_string().set_style(Styles::success()),
            ]));
        }
        lines
    }

    fn kind_label(&self) -> &'static str {
        "Action"
    }

    fn raw_text(&self) -> &str {
        &self.payload
    }
}

/// Final verdict for a turn.
#[derive(Debug, Clone)]
pub struct ResultCell {
    pub payload: String,
}

impl HistoryCell for ResultCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let wrap_width = width.saturating_sub(6).max(1) as usize;
        let wrapped = textwrap::wrap(&self.payload, wrap_width);
        for (i, line) in wrapped.iter().enumerate() {
            let prefix = if i == 0 {
                format!("{GUTTER}✓ ")
            } else {
                format!("{GUTTER}  ")
            };
            lines.push(Line::from(vec![
                prefix.set_style(Styles::tool_header()),
                line.to_string().set_style(Styles::tool_header()),
            ]));
        }
        lines
    }

    fn kind_label(&self) -> &'static str {
        "Result"
    }

    fn raw_text(&self) -> &str {
        &self.payload
    }
}

/// System-level informational log.
#[derive(Debug, Clone)]
pub struct LogCell {
    pub payload: String,
}

impl HistoryCell for LogCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let wrap_width = width.saturating_sub(6).max(1) as usize;
        let wrapped = textwrap::wrap(&self.payload, wrap_width);
        for (i, line) in wrapped.iter().enumerate() {
            let prefix = if i == 0 {
                format!("{GUTTER}· ")
            } else {
                format!("{GUTTER}  ")
            };
            lines.push(Line::from(vec![
                prefix.set_style(Styles::evidence()),
                line.to_string().set_style(Styles::evidence()),
            ]));
        }
        lines
    }

    fn kind_label(&self) -> &'static str {
        "Log"
    }

    fn raw_text(&self) -> &str {
        &self.payload
    }
}

/// Error cell.
#[derive(Debug, Clone)]
pub struct ErrorCell {
    pub payload: String,
}

impl HistoryCell for ErrorCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let wrap_width = width.saturating_sub(6).max(1) as usize;
        let wrapped = textwrap::wrap(&self.payload, wrap_width);
        for (i, line) in wrapped.iter().enumerate() {
            let prefix = if i == 0 {
                format!("{GUTTER}✘ ")
            } else {
                format!("{GUTTER}  ")
            };
            lines.push(Line::from(vec![
                prefix.set_style(Styles::error()),
                line.to_string().set_style(Styles::error()),
            ]));
        }
        lines
    }

    fn kind_label(&self) -> &'static str {
        "Error"
    }

    fn raw_text(&self) -> &str {
        &self.payload
    }
}

/// Multi-agent debate convergence trace.
#[derive(Debug, Clone)]
pub struct DebateCell {
    pub payload: String,
}

impl HistoryCell for DebateCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let wrap_width = width.saturating_sub(6).max(1) as usize;
        let wrapped = textwrap::wrap(&self.payload, wrap_width);
        for (i, line) in wrapped.iter().enumerate() {
            let prefix = if i == 0 {
                format!("{GUTTER}⚖ ")
            } else {
                format!("{GUTTER}  ")
            };
            lines.push(Line::from(vec![
                prefix.fg(Color::Magenta),
                line.to_string().fg(Color::Magenta),
            ]));
        }
        lines
    }

    fn kind_label(&self) -> &'static str {
        "Debate"
    }

    fn raw_text(&self) -> &str {
        &self.payload
    }
}

/// Diff cell with syntax-highlighted line numbers and gutter signs.
/// Uses `diffy` for parsing unified diffs and renders colored add/delete lines.
#[derive(Debug, Clone)]
pub struct DiffCell {
    pub payload: String,
}

impl HistoryCell for DiffCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        use super::diff_render::render_diff_lines;
        render_diff_lines(&self.payload, width)
    }

    fn kind_label(&self) -> &'static str {
        "Diff"
    }

    fn raw_text(&self) -> &str {
        &self.payload
    }
}

/// Status of a tool call card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCardStatus {
    Ok,
    Error,
    Cached,
    Retried,
}

/// A polished tool-call card with status icon, tool name, duration, output
/// size, and a one-line preview. Replaces the flat ActionCell for tool
/// completions so multi-step turns are visually scannable.
#[derive(Debug, Clone)]
pub struct ToolCallCell {
    pub tool_name: String,
    pub status: ToolCardStatus,
    pub duration_ms: u64,
    pub output_bytes: usize,
    pub preview: String,
    pub retry_count: u32,
    pub is_read_only: bool,
}

impl ToolCallCell {
    pub fn from_completed(
        tool_name: String,
        duration_ms: u64,
        output_bytes: usize,
        is_error: bool,
        preview: String,
        retry_count: u32,
        from_cache: bool,
    ) -> Self {
        let status = if is_error {
            ToolCardStatus::Error
        } else if from_cache {
            ToolCardStatus::Cached
        } else if retry_count > 0 {
            ToolCardStatus::Retried
        } else {
            ToolCardStatus::Ok
        };
        Self {
            tool_name,
            status,
            duration_ms,
            output_bytes,
            preview,
            retry_count,
            is_read_only: false,
        }
    }

    fn icon(&self) -> &'static str {
        match self.status {
            ToolCardStatus::Ok => "✓",
            ToolCardStatus::Error => "✘",
            ToolCardStatus::Cached => "💾",
            ToolCardStatus::Retried => "🔁",
        }
    }

    fn icon_color(&self) -> Color {
        match self.status {
            ToolCardStatus::Ok => Color::Green,
            ToolCardStatus::Error => Color::Red,
            ToolCardStatus::Cached => Color::Cyan,
            ToolCardStatus::Retried => Color::Yellow,
        }
    }

    fn format_size(&self) -> String {
        if self.output_bytes >= 1024 * 1024 {
            format!("{:.1}MB", self.output_bytes as f64 / (1024.0 * 1024.0))
        } else if self.output_bytes >= 1024 {
            format!("{:.1}KB", self.output_bytes as f64 / 1024.0)
        } else {
            format!("{}B", self.output_bytes)
        }
    }

    fn format_duration(&self) -> String {
        if self.duration_ms >= 60_000 {
            let secs = self.duration_ms / 1000;
            format!("{}m{:02}s", secs / 60, secs % 60)
        } else if self.duration_ms >= 1_000 {
            format!("{:.1}s", self.duration_ms as f64 / 1000.0)
        } else {
            format!("{}ms", self.duration_ms)
        }
    }
}

impl HistoryCell for ToolCallCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        // Header line: icon + tool name + duration + size
        let meta = if self.output_bytes > 0 {
            format!("({} · {})", self.format_duration(), self.format_size())
        } else {
            format!("({})", self.format_duration())
        };
        let mut header_spans = vec![
            GUTTER.set_style(ratatui::style::Style::new()),
            self.icon().to_string().fg(self.icon_color()).bold(),
            " ".set_style(ratatui::style::Style::new()),
            self.tool_name.clone().bold(),
            " ".set_style(ratatui::style::Style::new()),
            meta.fg(Color::DarkGray),
        ];
        if self.retry_count > 0 {
            header_spans.push(
                format!(" ↻{}", self.retry_count).fg(Color::Yellow),
            );
        }
        if matches!(self.status, ToolCardStatus::Cached) {
            header_spans.push(" cached".fg(Color::Cyan).dim());
        }
        lines.push(Line::from(header_spans));

        // Preview (single line, wrapped, dim)
        if !self.preview.trim().is_empty() {
            let wrap_width = width.saturating_sub(6).max(1) as usize;
            let preview_first = self
                .preview
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or(self.preview.as_str())
                .to_string();
            let wrapped = textwrap::wrap(&preview_first, wrap_width);
            for line in wrapped.iter().take(2) {
                lines.push(Line::from(vec![
                    format!("{GUTTER}  ").fg(Color::DarkGray),
                    line.to_string().fg(Color::DarkGray),
                ]));
            }
        }

        lines
    }

    fn kind_label(&self) -> &'static str {
        "ToolCall"
    }

    fn raw_text(&self) -> &str {
        &self.preview
    }
}
