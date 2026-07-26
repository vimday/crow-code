use super::super::component::{Component, TuiAction};
use super::super::state::AppState;
use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyModifiers};
use ratatui::{
    layout::Rect,
    widgets::{Block, Borders},
    Frame,
};
use tui_textarea::TextArea;

/// Maximum number of command palette items visible at once.
const PALETTE_MAX_VISIBLE: usize = 8;
/// How many file picker matches to show.
const FILE_PICKER_MAX_VISIBLE: usize = 8;
/// Maximum number of files to scan for the @ picker (perf cap).
const FILE_PICKER_SCAN_LIMIT: usize = 20_000;

pub enum ActivePopup {
    None,
    CommandPalette {
        query: String,
        selected_idx: usize,
        /// Scroll offset: index of the first visible item.
        scroll_offset: usize,
        options: Vec<(String, String)>,
    },
    /// File picker triggered by typing `@<query>` in the composer.
    /// `start_byte` is the byte offset of the `@` in the input buffer
    /// (used to splice the chosen path back into place on Enter).
    FilePicker {
        query: String,
        selected_idx: usize,
        scroll_offset: usize,
        candidates: Vec<String>,
        start_byte: usize,
    },
}

/// Threshold: pastes with more lines than this get collapsed into a
/// placeholder in the composer (Claude Code behavior). The full content
/// is stored in `paste_attachments` and expanded on submit.
const PASTE_COLLAPSE_LINES: usize = 5;

pub struct ComposerComponent<'a> {
    pub textarea: TextArea<'a>,
    pub active_popup: ActivePopup,
    /// Paste attachments: when a large paste is collapsed into a
    /// placeholder, the full text is stored here keyed by a monotonic
    /// index. On submit, placeholders are expanded back.
    pub paste_attachments: Vec<String>,
    paste_counter: usize,
}

impl<'a> Default for ComposerComponent<'a> {
    fn default() -> Self {
        Self::new()
    }
}

/// Create a fresh textarea with standard styling.
/// Extracted to eliminate 4x duplication of textarea reset logic.
fn make_textarea<'a>() -> TextArea<'a> {
    let mut textarea = TextArea::default();
    textarea.set_block(Block::default().borders(Borders::NONE));
    textarea.set_cursor_line_style(ratatui::style::Style::new());
    // Hide the textarea's internal block cursor — we use frame.set_cursor()
    // to place a single terminal blinking cursor (Codex/Claude Code pattern).
    textarea.set_cursor_style(ratatui::style::Style::new());
    let placeholder = ratatui::style::Style::new().fg(ratatui::style::Color::DarkGray);
    textarea.set_placeholder_text("Ask Crow anything · / for commands · @ for files");
    textarea.set_placeholder_style(placeholder);
    // NOTE: Do NOT call set_line_number_style() — it enables line numbers.
    // A chat input box should never show line numbers.
    textarea
}

impl<'a> ComposerComponent<'a> {
    pub fn new() -> Self {
        Self {
            textarea: make_textarea(),
            active_popup: ActivePopup::None,
            paste_attachments: Vec::new(),
            paste_counter: 0,
        }
    }

    /// Reset textarea to a clean state. Used after submission.
    fn reset_textarea(&mut self) {
        self.textarea = make_textarea();
        self.active_popup = ActivePopup::None;
        self.paste_attachments.clear();
        self.paste_counter = 0;
    }

    /// Replace `[Pasted text #N: ...]` placeholders with the original
    /// content stored in `paste_attachments`. Called just before submit.
    fn expand_paste_placeholders(&self, text: &str) -> String {
        if self.paste_attachments.is_empty() {
            return text.to_string();
        }
        let mut result = text.to_string();
        for (i, content) in self.paste_attachments.iter().enumerate() {
            let idx = i + 1; // 1-indexed
                             // Match the placeholder pattern we insert. Because the
                             // format_bytes output varies, match up to the `]`.
            let prefix = format!("[Pasted text #{idx}:");
            if let Some(start) = result.find(&prefix) {
                if let Some(end_offset) = result[start..].find(']') {
                    let end = start + end_offset + 1;
                    result.replace_range(start..end, content);
                }
            }
        }
        result
    }

    pub fn get_popup_height(&self, state: &AppState) -> u16 {
        if let crate::tui::state::ApprovalState::PendingCommand(..) = state.approval_state {
            return 5;
        }
        match &self.active_popup {
            ActivePopup::CommandPalette { options, .. } => {
                (options.len() as u16).min(PALETTE_MAX_VISIBLE as u16) + 2
            }
            ActivePopup::FilePicker { candidates, .. } => {
                (candidates.len() as u16).min(FILE_PICKER_MAX_VISIBLE as u16) + 2
            }
            ActivePopup::None => 0,
        }
    }

    /// Desired composer height in terminal rows. Grows with the
    /// textarea's content (e.g. after a multi-line paste) so the user
    /// can see all lines and navigate with arrow keys, but caps at 12
    /// rows to keep the conversation pane readable.
    ///
    /// Layout breakdown: 1 row for top border + N content rows.
    pub fn desired_height(&self) -> u16 {
        const MIN_LINES: u16 = 1;
        const MAX_LINES: u16 = 12;
        let content_lines = self.textarea.lines().len() as u16;
        let lines = content_lines.clamp(MIN_LINES, MAX_LINES);
        lines + 1 // +1 for top border
    }
}

impl<'a> Component for ComposerComponent<'a> {
    fn handle_event(&mut self, event: &Event, state: &mut AppState) -> Result<Option<TuiAction>> {
        // Handle bracketed paste events (Ctrl+V / terminal paste)
        if let Event::Paste(ref text) = event {
            let bytes = text.len();
            let line_count = text.lines().count();

            // ── Claude Code behavior: collapse large pastes ─────────
            // Short pastes (<= PASTE_COLLAPSE_LINES lines AND < 4KB):
            //   insert verbatim with proper newlines.
            // Large pastes:
            //   store the full content as an attachment and insert a
            //   compact placeholder like `[Pasted text #1: 245 lines]`
            //   so the composer stays readable. On submit, the
            //   placeholder is expanded back to full content.
            const SOFT_THRESHOLD: usize = 4 * 1024;

            if line_count > PASTE_COLLAPSE_LINES || bytes >= SOFT_THRESHOLD {
                // Collapse into placeholder
                self.paste_counter += 1;
                let idx = self.paste_counter;
                self.paste_attachments.push(text.clone());
                let placeholder = format!(
                    "[Pasted text #{idx}: {line_count} lines, {}]",
                    format_bytes(bytes)
                );
                self.textarea.insert_str(&placeholder);
                state.show_status(
                    crate::tui::state::StatusMessage::info(format!(
                        "Attached paste #{idx} ({line_count} lines) — will expand on submit"
                    )),
                    4000,
                );
            } else {
                // Short paste: insert_str handles '\n' natively — it
                // splits on newlines and inserts as a multi-line chunk.
                self.textarea.insert_str(text);
            }
            return Ok(None);
        }

        if let Event::Key(key) = event {
            // Check if we are in overlay mode
            if let ActivePopup::CommandPalette {
                query: _,
                ref mut selected_idx,
                ref mut scroll_offset,
                ref options,
            } = self.active_popup
            {
                if key.code == KeyCode::Esc {
                    self.active_popup = ActivePopup::None;
                    return Ok(None);
                }

                // Intercept navigation with auto-scroll
                if key.code == KeyCode::Up {
                    if *selected_idx > 0 {
                        *selected_idx -= 1;
                        // Scroll up if cursor moved above visible window
                        if *selected_idx < *scroll_offset {
                            *scroll_offset = *selected_idx;
                        }
                    }
                    return Ok(None);
                }
                if key.code == KeyCode::Down {
                    if *selected_idx < options.len().saturating_sub(1) {
                        *selected_idx += 1;
                        // Scroll down if cursor moved below visible window
                        if *selected_idx >= *scroll_offset + PALETTE_MAX_VISIBLE {
                            *scroll_offset = selected_idx.saturating_sub(PALETTE_MAX_VISIBLE - 1);
                        }
                    }
                    return Ok(None);
                }

                // Intercept autocomplete Enter
                if key.code == KeyCode::Enter && !key.modifiers.contains(KeyModifiers::SHIFT) {
                    if let Some((cmd, _)) = options.get(*selected_idx) {
                        let text = cmd.clone();
                        self.reset_textarea();
                        return Ok(Some(TuiAction::SubmitCommand(text)));
                    }
                }
            }

            // ── File picker overlay ─────────────────────────────────
            // Allows the user to splice a workspace file path into the
            // input by typing `@<query>` and selecting from the picker.
            if let ActivePopup::FilePicker {
                ref mut selected_idx,
                ref mut scroll_offset,
                ref candidates,
                start_byte,
                ..
            } = self.active_popup
            {
                if key.code == KeyCode::Esc {
                    self.active_popup = ActivePopup::None;
                    return Ok(None);
                }
                if key.code == KeyCode::Up {
                    if *selected_idx > 0 {
                        *selected_idx -= 1;
                        if *selected_idx < *scroll_offset {
                            *scroll_offset = *selected_idx;
                        }
                    }
                    return Ok(None);
                }
                if key.code == KeyCode::Down {
                    if *selected_idx < candidates.len().saturating_sub(1) {
                        *selected_idx += 1;
                        if *selected_idx >= *scroll_offset + FILE_PICKER_MAX_VISIBLE {
                            *scroll_offset =
                                selected_idx.saturating_sub(FILE_PICKER_MAX_VISIBLE - 1);
                        }
                    }
                    return Ok(None);
                }
                if (key.code == KeyCode::Tab || key.code == KeyCode::Enter)
                    && !key.modifiers.contains(KeyModifiers::SHIFT)
                {
                    if let Some(path) = candidates.get(*selected_idx).cloned() {
                        let line = self.textarea.lines().join("\n");
                        let after = if start_byte <= line.len() {
                            extract_after_at_token(&line[start_byte..])
                        } else {
                            ""
                        };
                        let mut new_line = String::new();
                        new_line.push_str(&line[..start_byte]);
                        new_line.push_str(&path);
                        new_line.push(' ');
                        new_line.push_str(after);

                        let _ = after; // shadow to silence unused
                        self.textarea = make_textarea();
                        self.textarea.insert_str(&new_line);
                        self.active_popup = ActivePopup::None;
                        return Ok(None);
                    }
                }
                // Fall through for typing — let the textarea consume it
                // and the post-mutation block below will refresh the picker.
            }

            // ── Ctrl+U: clear current line (Unix convention) ──────────
            if key.code == KeyCode::Char('u') && key.modifiers.contains(KeyModifiers::CONTROL) {
                self.reset_textarea();
                return Ok(None);
            }

            // ── Input history navigation: ↑/↓ ──────────────────────────
            // Up works when composer is empty OR when already browsing history.
            // This matches shell behavior: once you start cycling, you can
            // keep going without clearing the input first.
            if key.code == KeyCode::Up
                && !state.input_history.is_empty()
                && (state.input_history_idx.is_some()
                    || self.textarea.lines().join("").trim().is_empty())
            {
                let idx = state
                    .input_history_idx
                    .map(|i| i.saturating_sub(1))
                    .unwrap_or(state.input_history.len().saturating_sub(1));
                state.input_history_idx = Some(idx);
                self.reset_textarea();
                self.textarea.insert_str(&state.input_history[idx]);
                return Ok(None);
            }
            if key.code == KeyCode::Down && state.input_history_idx.is_some() {
                let idx = state.input_history_idx.unwrap_or(0) + 1;
                if idx < state.input_history.len() {
                    state.input_history_idx = Some(idx);
                    self.reset_textarea();
                    self.textarea.insert_str(&state.input_history[idx]);
                } else {
                    state.input_history_idx = None;
                    self.reset_textarea();
                }
                return Ok(None);
            }

            // Normal textarea handling — Enter submits (Shift+Enter = newline)
            if key.code == KeyCode::Enter && !key.modifiers.contains(KeyModifiers::SHIFT) {
                let lines = self.textarea.lines().to_vec();
                let mut text = lines.join("\n");
                // Expand paste placeholders back to full content.
                text = self.expand_paste_placeholders(&text);
                self.reset_textarea();
                return Ok(Some(TuiAction::SubmitCommand(text)));
            }

            self.textarea.input(*key);

            // Post-mutation text analysis for the popup logic.
            // Order: command palette (slash/bang prefix) → file picker (@token).
            let lines = self.textarea.lines();
            let single_line = lines.len() == 1;
            let line0 = lines.first().map(String::as_str).unwrap_or("");
            if single_line && (line0.starts_with('/') || line0.starts_with('!')) {
                let query = line0.to_string();
                let options = crow_commands::get_palette_commands(&query);

                if !options.is_empty() {
                    self.active_popup = ActivePopup::CommandPalette {
                        query,
                        selected_idx: 0,
                        scroll_offset: 0,
                        options,
                    };
                } else {
                    self.active_popup = ActivePopup::None;
                }
            } else if let Some((start, query)) = current_at_token(lines, self.textarea.cursor()) {
                // @ picker: scan workspace lazily and rank by simple
                // contains/prefix match. Capped at FILE_PICKER_SCAN_LIMIT
                // entries so this stays snappy even on huge repos.
                let candidates =
                    rank_workspace_files(&state.workspace_name, &query, FILE_PICKER_SCAN_LIMIT);
                if candidates.is_empty() {
                    self.active_popup = ActivePopup::None;
                } else {
                    self.active_popup = ActivePopup::FilePicker {
                        query,
                        selected_idx: 0,
                        scroll_offset: 0,
                        candidates,
                        start_byte: start,
                    };
                }
            } else {
                self.active_popup = ActivePopup::None;
            }
        }
        Ok(None)
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, state: &AppState) {
        // If there is a pending command approval, render the security prompt
        if let crate::tui::state::ApprovalState::PendingCommand(ref cmd, selected_idx) =
            state.approval_state
        {
            render_approval_popup(frame, area, cmd, selected_idx);
            return;
        }

        // Ensure block remains NONE
        self.textarea
            .set_block(Block::default().borders(Borders::NONE));

        let popup_h = self.get_popup_height(state);
        let split = ratatui::layout::Layout::default()
            .direction(ratatui::layout::Direction::Vertical)
            .constraints([
                ratatui::layout::Constraint::Length(popup_h),
                ratatui::layout::Constraint::Min(0),
            ])
            .split(area);

        let popup_area = split[0];
        let composer_area = split[1];

        // Give the entire composer area a top border to separate it from history/status
        let composer_block = Block::default()
            .borders(Borders::TOP)
            .border_style(ratatui::style::Style::new().fg(ratatui::style::Color::DarkGray));

        let inner_composer_area = composer_block.inner(composer_area);
        frame.render_widget(composer_block, composer_area);

        let composer_split = ratatui::layout::Layout::default()
            .direction(ratatui::layout::Direction::Horizontal)
            .constraints([
                ratatui::layout::Constraint::Length(2),
                ratatui::layout::Constraint::Min(0),
            ])
            .split(inner_composer_area);

        use crate::tui::theme::{chars, spinner_char, Styles};

        let prompt_text = if state.is_task_running() {
            format!("{} ", spinner_char(state.spinner_idx))
        } else {
            format!("{} ", chars::INPUT_PROMPT)
        };

        let prompt_style = if state.is_task_running() {
            Styles::spinner()
        } else {
            Styles::input_prompt()
        };

        let prompt_widget = ratatui::widgets::Paragraph::new(prompt_text).style(prompt_style);

        frame.render_widget(prompt_widget, composer_split[0]);
        frame.render_widget(self.textarea.widget(), composer_split[1]);

        // Always set the terminal cursor at the text insertion point so the
        // user can see where they are typing (Codex/Claude Code UX pattern).
        // Without this, the cursor is invisible and users must "blind-type."
        if state.focus == crate::tui::state::Focus::Composer {
            let (cursor_row, cursor_col) = self.textarea.cursor();
            let x = composer_split[1].x + cursor_col as u16;
            let y = composer_split[1].y + cursor_row as u16;
            // Clamp to the composer area to prevent cursor from escaping
            let clamped_x = x.min(composer_split[1].right().saturating_sub(1));
            let clamped_y = y.min(composer_split[1].bottom().saturating_sub(1));
            frame.set_cursor(clamped_x, clamped_y);
        }

        // Draw the floating popup if active
        if let ActivePopup::CommandPalette {
            query: _,
            selected_idx,
            scroll_offset,
            ref options,
        } = self.active_popup
        {
            if popup_h > 0 {
                use ratatui::style::{Color, Stylize};
                use ratatui::widgets::{Block, Borders, Clear, List, ListItem};

                frame.render_widget(Clear, popup_area); // Erase underlying content

                // Only render the visible window of items
                let visible_end = (scroll_offset + PALETTE_MAX_VISIBLE).min(options.len());
                let has_more_above = scroll_offset > 0;
                let has_more_below = visible_end < options.len();

                let list_items: Vec<ListItem> = options[scroll_offset..visible_end]
                    .iter()
                    .enumerate()
                    .map(|(vis_i, (cmd, desc))| {
                        let abs_i = vis_i + scroll_offset;
                        let content = format!(" {cmd:18} {desc}");
                        if abs_i == selected_idx {
                            ListItem::new(content).style(
                                ratatui::style::Style::new()
                                    .bg(Color::Cyan)
                                    .fg(Color::Black)
                                    .bold(),
                            )
                        } else {
                            ListItem::new(content)
                        }
                    })
                    .collect();

                // Build title with scroll indicators
                let title = if has_more_above && has_more_below {
                    " Commands ▲▼ "
                } else if has_more_above {
                    " Commands ▲ "
                } else if has_more_below {
                    " Commands ▼ "
                } else {
                    " Commands "
                };

                let popup_list = List::new(list_items).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(ratatui::style::Style::new().fg(Color::Cyan))
                        .title(title),
                );

                // Dynamic width: 55% of terminal width, clamped to [40, 60]
                let popup_width = (area.width * 55 / 100).clamp(40, 60);

                // Render List on the left side of the popup area
                let popup_horiz = ratatui::layout::Layout::default()
                    .direction(ratatui::layout::Direction::Horizontal)
                    .constraints([
                        ratatui::layout::Constraint::Length(popup_width),
                        ratatui::layout::Constraint::Min(0),
                    ])
                    .split(popup_area);

                frame.render_widget(popup_list, popup_horiz[0]);
            }
        }

        // Draw the file picker overlay (@ trigger)
        if let ActivePopup::FilePicker {
            ref query,
            selected_idx,
            scroll_offset,
            ref candidates,
            ..
        } = self.active_popup
        {
            if popup_h > 0 {
                use ratatui::style::{Color, Stylize};
                use ratatui::widgets::{Block, Borders, Clear, List, ListItem};

                frame.render_widget(Clear, popup_area);

                let visible_end = (scroll_offset + FILE_PICKER_MAX_VISIBLE).min(candidates.len());
                let has_more_above = scroll_offset > 0;
                let has_more_below = visible_end < candidates.len();

                let list_items: Vec<ListItem> = candidates[scroll_offset..visible_end]
                    .iter()
                    .enumerate()
                    .map(|(vis_i, path)| {
                        let abs_i = vis_i + scroll_offset;
                        let line = format!(" {path}");
                        if abs_i == selected_idx {
                            ListItem::new(line).style(
                                ratatui::style::Style::new()
                                    .bg(Color::LightMagenta)
                                    .fg(Color::Black)
                                    .bold(),
                            )
                        } else {
                            ListItem::new(line)
                        }
                    })
                    .collect();

                let title = match (has_more_above, has_more_below) {
                    (true, true) => format!(" @{query}  ▲▼ "),
                    (true, false) => format!(" @{query}  ▲ "),
                    (false, true) => format!(" @{query}  ▼ "),
                    (false, false) => format!(" @{query} "),
                };

                let popup_list = List::new(list_items).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(ratatui::style::Style::new().fg(Color::Magenta))
                        .title(title),
                );

                let popup_width = (area.width * 70 / 100).clamp(45, 80);
                let popup_horiz = ratatui::layout::Layout::default()
                    .direction(ratatui::layout::Direction::Horizontal)
                    .constraints([
                        ratatui::layout::Constraint::Length(popup_width),
                        ratatui::layout::Constraint::Min(0),
                    ])
                    .split(popup_area);
                frame.render_widget(popup_list, popup_horiz[0]);
            }
        }
    }
}

// ── Extracted approval popup renderer ─────────────────────────────────

/// Format a byte count into a short human-readable string (e.g. "1.2KB", "4MB").
fn format_bytes(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes}B")
    }
}

/// Locate the current `@<query>` token under or just before the cursor.
/// Returns `(byte_offset_of_at, query)` if one is active, else None.
///
/// Triggers when the immediately-preceding `@` is at start-of-line or
/// follows whitespace; the query is the run of non-whitespace
/// characters after it. Empty query is fine (just-typed `@`).
fn current_at_token(lines: &[String], cursor: (usize, usize)) -> Option<(usize, String)> {
    let (row, col) = cursor;
    let line = lines.get(row)?;
    let cursor_byte = line
        .char_indices()
        .nth(col)
        .map(|(b, _)| b)
        .unwrap_or(line.len());
    let head = &line[..cursor_byte];
    let at_pos = head.rfind('@')?;
    // Boundary check: at start-of-line or preceded by whitespace.
    if at_pos > 0 {
        let prev = head[..at_pos].chars().last()?;
        if !prev.is_whitespace() {
            return None;
        }
    }
    let query: String = head[at_pos + 1..].to_string();
    if query.contains(char::is_whitespace) {
        return None;
    }
    // Compute the running byte offset across prior lines (include '\n').
    let mut byte_offset = 0;
    for prior in lines.iter().take(row) {
        byte_offset += prior.len() + 1; // +1 for '\n'
    }
    byte_offset += at_pos;
    Some((byte_offset, query))
}

/// Helper: when committing a @-token replacement, return the slice of
/// text after the token within the spliced segment.
fn extract_after_at_token(rest: &str) -> &str {
    // `rest` starts at the `@`; find the first whitespace after it.
    let mut idx = 0;
    let bytes = rest.as_bytes();
    if bytes.first() == Some(&b'@') {
        idx = 1;
    }
    while idx < bytes.len() && !(bytes[idx] as char).is_whitespace() {
        idx += 1;
    }
    &rest[idx..]
}

/// Walk the workspace and rank files by query match. Returns up to
/// `FILE_PICKER_MAX_VISIBLE * 2` candidates so the user can scroll.
/// Skips common heavy directories (target, node_modules, .git).
fn rank_workspace_files(workspace_root: &str, query: &str, scan_limit: usize) -> Vec<String> {
    use std::path::Path;

    let root = Path::new(workspace_root);
    if !root.exists() {
        return Vec::new();
    }

    let mut all = Vec::with_capacity(2048);
    let mut count = 0usize;
    walk_files(root, root, &mut all, &mut count, scan_limit);

    let q_lower = query.to_lowercase();
    let mut ranked: Vec<(i32, String)> = all
        .into_iter()
        .filter_map(|rel| {
            let lower = rel.to_lowercase();
            // Score: 0 if no match, otherwise smaller is better.
            // Tier 1: filename startsWith   (rank 0)
            // Tier 2: any path startsWith   (rank 1)
            // Tier 3: path contains         (rank 2)
            let basename = std::path::Path::new(&rel)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();
            if q_lower.is_empty() {
                Some((3, rel))
            } else if basename.starts_with(&q_lower) {
                Some((0, rel))
            } else if lower.starts_with(&q_lower) {
                Some((1, rel))
            } else if lower.contains(&q_lower) {
                Some((2, rel))
            } else {
                None
            }
        })
        .collect();

    ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.len().cmp(&b.1.len())));
    ranked.truncate(FILE_PICKER_MAX_VISIBLE * 4);
    ranked.into_iter().map(|(_, p)| p).collect()
}

fn walk_files(
    root: &std::path::Path,
    dir: &std::path::Path,
    out: &mut Vec<String>,
    count: &mut usize,
    limit: usize,
) {
    if *count >= limit {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        if *count >= limit {
            return;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') && name_str != "." {
            continue; // skip dotfiles/dirs (.git, .crow, etc.)
        }
        if matches!(
            name_str.as_ref(),
            "target" | "node_modules" | "dist" | "build" | "venv" | "__pycache__"
        ) {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = entry.file_type() else {
            continue;
        };
        if meta.is_dir() {
            walk_files(root, &path, out, count, limit);
        } else if meta.is_file() {
            if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_string_lossy().into_owned());
                *count += 1;
            }
        }
    }
}

/// Render the security approval popup. Extracted from inline render() to
/// reduce complexity and enable dynamic sizing.
fn render_approval_popup(frame: &mut Frame, area: Rect, cmd: &str, selected_idx: usize) {
    use ratatui::style::Stylize;
    use ratatui::text::Line;
    use ratatui::widgets::{List, ListItem, Paragraph};

    let composer_lines = vec![
        Line::from(vec!["⚠️  Security Approval Required".red().bold()]),
        Line::from(vec!["Command: ".dark_gray(), cmd.to_string().into()]),
        Line::from(vec![
            "  (y=Allow  a=Always  n=Reject  Esc=Cancel)".dark_gray()
        ]),
    ];

    let composer_widget =
        Paragraph::new(composer_lines).block(Block::default().borders(Borders::NONE));
    frame.render_widget(composer_widget, area);

    // Render floating interaction popup — dynamically sized to terminal width
    let options = [
        "[✓] Allow Once",
        "[★] Allow Always (Whitelist)",
        "[X] Reject",
    ];
    let list_items: Vec<ListItem> = options
        .iter()
        .enumerate()
        .map(|(i, &opt)| {
            if i == selected_idx {
                ListItem::new(opt).style(
                    ratatui::style::Style::new()
                        .bg(ratatui::style::Color::LightRed)
                        .fg(ratatui::style::Color::Black)
                        .bold(),
                )
            } else {
                ListItem::new(opt)
            }
        })
        .collect();

    let popup_list = List::new(list_items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(ratatui::style::Style::new().fg(ratatui::style::Color::LightRed))
            .title(" Action "),
    );

    // Dynamic sizing: cap popup width to terminal width - 12, minimum 30
    let popup_width = area.width.saturating_sub(12).clamp(30, 40);
    let popup_area = Rect {
        x: area.x.saturating_add(6),
        y: area.y.saturating_sub(5),
        width: popup_width,
        height: 5,
    };
    frame.render_widget(ratatui::widgets::Clear, popup_area);
    frame.render_widget(popup_list, popup_area);
}
