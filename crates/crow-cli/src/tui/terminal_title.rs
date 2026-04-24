//! Terminal-title management (Codex pattern).
//!
//! Sets the terminal window/tab title to reflect the current workspace and
//! agent state. This is sanitized before writing to prevent control character
//! injection from untrusted text sources.

/// Set the terminal title using OSC escape sequence.
/// Returns Ok(true) if title was written, Ok(false) if no visible content.
pub fn set_terminal_title(title: &str) -> std::io::Result<bool> {
    use std::io::{IsTerminal, Write};
    if !std::io::stdout().is_terminal() {
        return Ok(true);
    }
    let sanitized = sanitize_title(title);
    if sanitized.is_empty() {
        return Ok(false);
    }
    // OSC 0 = set window title, terminated by BEL
    write!(std::io::stdout(), "\x1b]0;{sanitized}\x07")?;
    std::io::stdout().flush()?;
    Ok(true)
}

/// Clear the terminal title.
pub fn clear_terminal_title() -> std::io::Result<()> {
    use std::io::{IsTerminal, Write};
    if !std::io::stdout().is_terminal() {
        return Ok(());
    }
    write!(std::io::stdout(), "\x1b]0;\x07")?;
    std::io::stdout().flush()
}

/// Maximum title length in chars.
const MAX_TITLE_CHARS: usize = 240;

/// Sanitize untrusted title text: strip control chars, bidi overrides,
/// collapse whitespace, and truncate.
fn sanitize_title(title: &str) -> String {
    let mut out = String::new();
    let mut chars_written = 0;
    let mut pending_space = false;

    for ch in title.chars() {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if ch.is_control() || is_disallowed_char(ch) {
            continue;
        }
        if pending_space && chars_written < MAX_TITLE_CHARS.saturating_sub(1) {
            out.push(' ');
            chars_written += 1;
            pending_space = false;
        }
        if chars_written >= MAX_TITLE_CHARS {
            break;
        }
        out.push(ch);
        chars_written += 1;
    }
    out
}

/// Disallowed chars: bidi overrides, invisible formatting, variation selectors.
fn is_disallowed_char(ch: char) -> bool {
    matches!(
        ch,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_control_chars() {
        let result = sanitize_title("Project\t|\nWorking\x1b\x07 | Thread");
        assert_eq!(result, "Project | Working | Thread");
    }

    #[test]
    fn strips_bidi_overrides() {
        let result = sanitize_title("Pro\u{202E}ject\u{200B} Title");
        assert_eq!(result, "Project Title");
    }

    #[test]
    fn truncates_long_titles() {
        let input = "a".repeat(MAX_TITLE_CHARS + 10);
        let result = sanitize_title(&input);
        assert_eq!(result.len(), MAX_TITLE_CHARS);
    }
}
