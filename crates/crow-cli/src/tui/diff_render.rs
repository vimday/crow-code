//! Theme-aware diff rendering with line numbers and gutter signs (Codex pattern).
//!
//! Parses unified diffs via `diffy` and renders each hunk with:
//! - Right-aligned line numbers
//! - Gutter signs (`+` / `-` / ` `)
//! - Theme-aware colors: green for inserts, red for deletes
//!
//! Falls back to plain dim text when a diff cannot be parsed.

use ratatui::style::{Color, Stylize};
use ratatui::text::{Line, Span};

use super::theme::colors;

/// The maximum display width reserved for line numbers (4 digits + space + sign + space).
const LINE_NUM_PAD: usize = 4;

/// Classify a diff line for styling.
#[derive(Clone, Copy)]
enum DiffLineKind {
    Insert,
    Delete,
    Context,
}

/// Render a full diff string (potentially containing multiple files) into styled `Line`s.
///
/// Attempts to parse as a unified diff. If parsing fails, falls back to a
/// line-by-line coloring heuristic based on `+`/`-` prefixes.
pub fn render_diff_lines(diff_text: &str, _width: u16) -> Vec<Line<'static>> {
    if diff_text.trim().is_empty() {
        return vec![Line::from("  Working tree is clean.".dim())];
    }

    let mut out = Vec::new();

    // Try to parse with diffy
    if let Ok(patch) = diffy::Patch::from_str(diff_text) {
        for hunk in patch.hunks() {
            // Hunk header
            let hunk_header = format!(
                "@@ -{},{} +{},{} @@",
                hunk.old_range().start(),
                hunk.old_range().len(),
                hunk.new_range().start(),
                hunk.new_range().len(),
            );
            out.push(Line::from(vec![
                "  ".into(),
                hunk_header.fg(colors::text_muted()).dim(),
            ]));

            let mut old_ln = hunk.old_range().start();
            let mut new_ln = hunk.new_range().start();

            for line in hunk.lines() {
                match line {
                    diffy::Line::Insert(text) => {
                        let s = text.trim_end_matches('\n');
                        out.push(render_diff_line(new_ln, DiffLineKind::Insert, s));
                        new_ln += 1;
                    }
                    diffy::Line::Delete(text) => {
                        let s = text.trim_end_matches('\n');
                        out.push(render_diff_line(old_ln, DiffLineKind::Delete, s));
                        old_ln += 1;
                    }
                    diffy::Line::Context(text) => {
                        let s = text.trim_end_matches('\n');
                        out.push(render_diff_line(new_ln, DiffLineKind::Context, s));
                        old_ln += 1;
                        new_ln += 1;
                    }
                }
            }

            // Hunk separator
            out.push(Line::from(""));
        }
    } else {
        // Fallback: heuristic line-by-line coloring for raw diff output
        out.extend(render_raw_diff_lines(diff_text));
    }

    out
}

/// Render a single diff line with line number, gutter sign, and colored content.
fn render_diff_line(line_num: usize, kind: DiffLineKind, text: &str) -> Line<'static> {
    let (sign, fg_color) = match kind {
        DiffLineKind::Insert => ("+", Color::Green),
        DiffLineKind::Delete => ("-", Color::Red),
        DiffLineKind::Context => (" ", colors::text_muted()),
    };

    let num_str = format!("{line_num:>LINE_NUM_PAD$}");

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(4);

    // Line number (dim for context, colored for add/delete)
    let num_style = match kind {
        DiffLineKind::Context => ratatui::style::Style::new().fg(colors::text_muted()).dim(),
        _ => ratatui::style::Style::new().fg(fg_color).dim(),
    };
    spans.push(Span::styled(num_str, num_style));
    spans.push(Span::styled(
        format!(" {sign} "),
        ratatui::style::Style::new().fg(fg_color),
    ));
    spans.push(Span::styled(
        text.to_string(),
        ratatui::style::Style::new().fg(fg_color),
    ));

    Line::from(spans)
}

/// Fallback renderer for unparseable diffs: colors lines by `+`/`-`/`@@` prefix.
fn render_raw_diff_lines(text: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    for raw_line in text.lines() {
        let line = if raw_line.starts_with("+++") || raw_line.starts_with("---") {
            // File header lines
            Line::from(vec!["  ".into(), raw_line.to_string().bold().dim()])
        } else if raw_line.starts_with('+') {
            Line::from(vec!["  ".into(), raw_line.to_string().fg(Color::Green)])
        } else if raw_line.starts_with('-') {
            Line::from(vec!["  ".into(), raw_line.to_string().fg(Color::Red)])
        } else if raw_line.starts_with("@@") {
            Line::from(vec![
                "  ".into(),
                raw_line.to_string().fg(colors::text_muted()).dim(),
            ])
        } else if raw_line.starts_with("diff ") {
            // diff --git a/... b/...
            Line::from(vec!["  ".into(), raw_line.to_string().bold()])
        } else if raw_line.starts_with("── ") {
            // Section headers (our own: "── Staged ──", "── Untracked ──")
            Line::from(vec![
                "  ".into(),
                raw_line.to_string().bold().fg(colors::accent_system()),
            ])
        } else {
            Line::from(vec!["  ".into(), raw_line.to_string().dim()])
        };
        lines.push(line);
    }

    lines
}
