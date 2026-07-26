use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Widget};

use crate::tui::shimmer::shimmer_spans;
use crate::tui::state::AppState;
use crate::tui::theme::{chars, colors};

const DETAILS_PREFIX: &str = "  └ ";

/// Compact elapsed string formatter (Codex style)
pub fn fmt_elapsed_compact(elapsed_secs: u64) -> String {
    if elapsed_secs < 60 {
        return format!("{elapsed_secs}s");
    }
    if elapsed_secs < 3600 {
        let minutes = elapsed_secs / 60;
        let seconds = elapsed_secs % 60;
        return format!("{minutes}m {seconds:02}s");
    }
    let hours = elapsed_secs / 3600;
    let minutes = (elapsed_secs % 3600) / 60;
    let seconds = elapsed_secs % 60;
    format!("{hours}h {minutes:02}m {seconds:02}s")
}

/// Truncate line with ellipsis
fn truncate_line_with_ellipsis(line: Line<'static>, max_width: usize) -> Line<'static> {
    if line.width() <= max_width {
        return line;
    }
    let mut out_spans = Vec::new();
    let mut current_width = 0;
    for span in line.spans {
        let sw = span.width();
        if current_width + sw > max_width {
            let remain = max_width.saturating_sub(current_width);
            if remain > 1 {
                let trunc: String = span.content.chars().take(remain - 1).collect();
                out_spans.push(Span::styled(format!("{trunc}…"), span.style));
            } else if remain == 1 {
                out_spans.push(Span::styled("…", span.style));
            }
            break;
        } else {
            out_spans.push(span);
            current_width += sw;
        }
    }
    Line::from(out_spans)
}

pub struct StatusIndicatorWidget<'a> {
    state: &'a AppState,
}

impl<'a> StatusIndicatorWidget<'a> {
    pub fn new(state: &'a AppState) -> Self {
        Self { state }
    }
}

impl<'a> Widget for StatusIndicatorWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        // ── Quit Hint Override (Codex "press again to quit" pattern) ──
        if let Some(deadline) = self.state.quit_hint_until {
            if std::time::Instant::now() < deadline {
                let hint = Line::from(vec![
                    " ⚠ ".fg(colors::accent_error()).bold(),
                    "Press ".fg(colors::accent_warning()),
                    "Ctrl+C".fg(colors::accent_error()).bold(),
                    " again to quit".fg(colors::accent_warning()),
                ]);
                Paragraph::new(hint).render(area, buf);
                return;
            }
        }

        // ── Right side context ──
        let mut right_parts: Vec<String> = Vec::new();
        right_parts.push(self.state.model_info.clone());

        #[allow(clippy::cast_precision_loss)]
        if let Some((tokens, context_window)) = self.state.ctx_usage {
            if context_window > 0 {
                let pct = tokens as f32 / context_window as f32;
                let cw_k = context_window / 1000;
                right_parts.push(format!("{:.1}% ({cw_k}K)", pct * 100.0));
            }
        }

        // streaming tokens
        if self.state.is_streaming {
            let tokens = self.state.streaming_token_estimate;
            let token_display = if tokens < 1000.0 {
                format!("{tokens:.0} tok")
            } else {
                format!("{:.1}k tok", tokens / 1000.0)
            };
            right_parts.push(token_display);
        }

        right_parts.push(format!("{:?}", self.state.view_mode));
        let right_text = format!(" {} ", right_parts.join(" · "));
        let right_w = right_text.chars().count() as u16;

        let right_color = self
            .state
            .ctx_usage
            .map(|(tokens, cw)| {
                if cw == 0 {
                    return colors::text_muted();
                }
                let pct = tokens as f32 / cw as f32;
                if pct >= 0.9 {
                    colors::accent_error()
                } else if pct >= 0.7 {
                    colors::accent_warning()
                } else {
                    colors::text_muted()
                }
            })
            .unwrap_or(colors::text_muted());

        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(0), Constraint::Length(right_w)])
            .split(Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: 1,
            });

        Paragraph::new(right_text.fg(right_color)).render(chunks[1], buf);

        // ── Left side (Action + Details) ──
        let mut spans = Vec::new();

        let mut has_action = false;

        // 1. Spinner & Header
        if let Some(ref action) = self.state.active_action {
            has_action = true;
            let frame = chars::SPINNER[self.state.spinner_idx % chars::SPINNER.len()];
            spans.push(Span::styled(
                frame,
                ratatui::style::Style::new()
                    .fg(colors::accent_system())
                    .bold(),
            ));
            spans.push(" ".into());
            spans.extend(shimmer_spans(action));
        } else if let Some(ref ind) = self.state.status_indicator {
            has_action = true;
            let frame = chars::SPINNER[self.state.spinner_idx % chars::SPINNER.len()];
            spans.push(Span::styled(
                frame,
                ratatui::style::Style::new()
                    .fg(colors::accent_system())
                    .bold(),
            ));
            spans.push(" ".into());
            if self.state.is_streaming {
                spans.extend(shimmer_spans(&ind.header));
            } else {
                spans.push(Span::raw(ind.header.clone()).fg(colors::accent_system()));
            }
        } else if let Some(ref msg) = self.state.status_message {
            let color = match msg.level {
                crate::tui::state::StatusLevel::Info => colors::accent_system(),
                crate::tui::state::StatusLevel::Warn => colors::accent_warning(),
                crate::tui::state::StatusLevel::Error => colors::accent_error(),
                crate::tui::state::StatusLevel::Tip => colors::text_muted(),
            };
            spans.push(Span::styled(
                msg.content.clone(),
                ratatui::style::Style::new().fg(color),
            ));
        }

        if has_action {
            spans.push(" ".into());

            // 3. Elapsed & Interrupt Hint
            let elapsed_secs = if let Some(start) = self.state.streaming_start_time {
                start.elapsed().as_secs()
            } else {
                0
            };
            let pretty_elapsed = fmt_elapsed_compact(elapsed_secs);

            if self.state.is_task_running() {
                spans.extend(vec![
                    format!("({pretty_elapsed} • ").dim(),
                    "esc".bold().dim(),
                    " to interrupt)".dim(),
                ]);
            } else {
                spans.push(format!("({pretty_elapsed})").dim());
            }
        }

        let mut lines = Vec::new();
        lines.push(truncate_line_with_ellipsis(
            Line::from(spans),
            chunks[0].width as usize,
        ));

        // Progress bar (slim) when the indicator carries a percent.
        if area.height > 1 {
            if let Some(ref ind) = self.state.status_indicator {
                if let Some(pct) = ind.progress_pct {
                    let bar_width = chunks[0].width.saturating_sub(8) as usize;
                    if bar_width >= 8 {
                        let filled = (bar_width * pct as usize / 100).min(bar_width);
                        let empty = bar_width - filled;
                        let bar_str = format!(
                            "  {}{}{} {pct:>3}%",
                            "▰".repeat(filled),
                            "▱".repeat(empty),
                            ""
                        );
                        let _ = bar_str;
                        let bar_line = Line::from(vec![
                            Span::styled(
                                format!("  {}", "▰".repeat(filled)),
                                ratatui::style::Style::new().fg(colors::accent_system()),
                            ),
                            Span::styled(
                                "▱".repeat(empty),
                                ratatui::style::Style::new().fg(colors::text_muted()),
                            ),
                            Span::styled(
                                format!(" {pct:>3}%"),
                                ratatui::style::Style::new().fg(colors::text_muted()),
                            ),
                        ]);
                        lines.push(bar_line);
                    }
                }
            }
        }

        // 4. Details (if any) and enough height
        if area.height > 1 {
            if let Some(ref ind) = self.state.status_indicator {
                if let Some(ref details) = ind.details {
                    // Simple wrap logic
                    let wrap_opts = textwrap::Options::new(area.width.saturating_sub(4) as usize);
                    let wrapped = textwrap::wrap(details, wrap_opts);
                    for (i, wline) in wrapped
                        .iter()
                        .enumerate()
                        .take(area.height.saturating_sub(1) as usize)
                    {
                        if i == 0 {
                            lines.push(Line::from(vec![
                                DETAILS_PREFIX.dim(),
                                wline.to_string().dim(),
                            ]));
                        } else {
                            lines.push(Line::from(vec!["    ".dim(), wline.to_string().dim()]));
                        }
                    }
                }
            }
        }

        Paragraph::new(Text::from(lines)).render(
            Rect {
                x: chunks[0].x,
                y: area.y,
                width: chunks[0].width,
                height: area.height,
            },
            buf,
        );
    }
}
