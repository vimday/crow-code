//! Theme and styling configuration for the Crow TUI.
//!
//! Inspired by yomi's runtime-configurable semantic color system.
//! All colors use true-color hex RGB for modern terminal rendering.

use ratatui::style::{Color, Modifier, Style};
use std::sync::{LazyLock, RwLock};

/// Semantic color configuration — modify these to customize the theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeConfig {
    // Core backgrounds
    /// Main background color (transparent by default)
    pub background: Color,
    /// Elevated surface / input area
    pub surface: Color,

    // Text hierarchy
    /// Primary text (main content)
    pub text_primary: Color,
    /// Secondary text (descriptions, metadata)
    pub text_secondary: Color,
    /// Muted text (placeholders, disabled)
    pub text_muted: Color,

    // Accent colors
    /// User message accent
    pub accent_user: Color,
    /// User message background tint
    pub user_msg_bg: Color,
    /// System / tool accent (tool calls, system info)
    pub accent_system: Color,
    /// Success states
    pub accent_success: Color,
    /// Warning states
    pub accent_warning: Color,
    /// Error states
    pub accent_error: Color,

    // Code block colors
    /// Code block background
    pub code_bg: Color,
    /// Code text color
    pub code_fg: Color,
    /// Code block border
    pub code_border: Color,

    // UI chrome
    /// Border color
    pub border: Color,
    /// Active / focused border
    pub border_active: Color,
    /// Divider lines
    pub divider: Color,
}

impl Default for ThemeConfig {
    /// Default crow dark theme with transparent backgrounds.
    fn default() -> Self {
        Self {
            background: Color::Reset,
            surface: Color::Reset,

            text_primary: hex("#F5F5FA"),
            text_secondary: hex("#90909F"),
            text_muted: hex("#808090"),

            accent_user: hex("#C4C6CF"),
            user_msg_bg: hex("#2A2A35"),
            accent_system: hex("#64C8FF"),
            accent_success: hex("#64DC8C"),
            accent_warning: hex("#FFC864"),
            accent_error: hex("#FF6464"),

            code_bg: hex("#23232D"),
            code_fg: hex("#8CDCF0"),
            code_border: hex("#707080"),

            border: hex("#707080"),
            border_active: hex("#A0A0AF"),
            divider: hex("#707080"),
        }
    }
}

// ── Global thread-safe theme ─────────────────────────────────────────────────

static THEME_CONFIG: LazyLock<RwLock<ThemeConfig>> =
    LazyLock::new(|| RwLock::new(ThemeConfig::default()));

/// Get the current theme configuration.
pub fn current_theme() -> ThemeConfig {
    *THEME_CONFIG.read().expect("theme lock poisoned")
}

/// Set the global theme configuration.
pub fn set_theme(config: ThemeConfig) {
    if let Ok(mut theme) = THEME_CONFIG.write() {
        *theme = config;
    }
}

/// Reset to default theme.
#[allow(dead_code)]
pub fn reset_theme() {
    set_theme(ThemeConfig::default());
}

// ── Color accessors ──────────────────────────────────────────────────────────

pub mod colors {
    use super::current_theme;
    use ratatui::style::Color;

    pub fn text_primary() -> Color {
        current_theme().text_primary
    }
    pub fn text_secondary() -> Color {
        current_theme().text_secondary
    }
    pub fn text_muted() -> Color {
        current_theme().text_muted
    }

    pub fn accent_user() -> Color {
        current_theme().accent_user
    }
    pub fn accent_system() -> Color {
        current_theme().accent_system
    }
    pub fn accent_success() -> Color {
        current_theme().accent_success
    }
    pub fn accent_warning() -> Color {
        current_theme().accent_warning
    }
    pub fn accent_error() -> Color {
        current_theme().accent_error
    }

    pub fn code_fg() -> Color {
        current_theme().code_fg
    }
    pub fn code_border() -> Color {
        current_theme().code_border
    }

    pub fn border() -> Color {
        current_theme().border
    }
    pub fn divider() -> Color {
        current_theme().divider
    }
}

// ── Semantic style presets ───────────────────────────────────────────────────

pub struct Styles;

impl Styles {
    /// User message header style.
    pub fn user_header() -> Style {
        Style::default()
            .fg(colors::accent_user())
            .add_modifier(Modifier::BOLD)
    }

    /// User message content style.
    pub fn user_content() -> Style {
        Style::default().fg(colors::text_primary())
    }

    /// Assistant message content style.
    pub fn assistant_content() -> Style {
        Style::default().fg(colors::text_primary())
    }

    /// Evidence / recon line style.
    pub fn evidence() -> Style {
        Style::default().fg(colors::text_secondary())
    }

    /// System / tool header style.
    pub fn tool_header() -> Style {
        Style::default()
            .fg(colors::accent_system())
            .add_modifier(Modifier::BOLD)
    }

    /// Tool content style.
    pub fn tool_content() -> Style {
        Style::default().fg(colors::text_secondary())
    }

    /// Success style.
    pub fn success() -> Style {
        Style::default().fg(colors::accent_success())
    }

    /// Warning style.
    pub fn warning() -> Style {
        Style::default().fg(colors::accent_warning())
    }

    /// Error style.
    pub fn error() -> Style {
        Style::default()
            .fg(colors::accent_error())
            .add_modifier(Modifier::BOLD)
    }

    /// Spinner style.
    pub fn spinner() -> Style {
        Style::default()
            .fg(colors::accent_system())
            .add_modifier(Modifier::BOLD)
    }

    /// Code block style.
    pub fn code_block() -> Style {
        Style::default().fg(colors::code_fg())
    }

    /// Code language tag.
    pub fn code_lang() -> Style {
        Style::default()
            .fg(colors::text_secondary())
            .add_modifier(Modifier::BOLD)
    }

    /// Inline code.
    pub fn inline_code() -> Style {
        Style::default()
            .fg(colors::code_fg())
            .add_modifier(Modifier::BOLD)
    }

    /// Input prompt style.
    pub fn input_prompt() -> Style {
        Style::default()
            .fg(colors::accent_user())
            .add_modifier(Modifier::BOLD)
    }

    /// Placeholder style.
    pub fn placeholder() -> Style {
        Style::default().fg(colors::text_muted())
    }

    /// Thinking / reasoning header.
    pub fn thinking() -> Style {
        Style::default()
            .fg(colors::text_secondary())
            .add_modifier(Modifier::ITALIC)
    }
}

// ── Box-drawing characters ──────────────────────────────────────────────────

pub mod chars {
    /// Vertical bar for message blocks.
    pub const USER_BAR: &str = "│";

    /// Section indicators.
    pub const BULLET: &str = "•";

    /// Input prompt.
    pub const INPUT_PROMPT: &str = "❯";
    pub const INPUT_PROMPT_MULTI: &str = "│";

    /// Code block borders.
    pub const CODE_TOP_LEFT: &str = "╭";
    pub const CODE_TOP_RIGHT: &str = "╮";
    pub const CODE_BOTTOM_LEFT: &str = "╰";
    #[allow(dead_code)]
    pub const CODE_BOTTOM_RIGHT: &str = "╯";
    pub const CODE_HORIZONTAL: &str = "─";
    pub const CODE_VERTICAL: &str = "│";

    /// Spinner frames (braille dots).
    pub const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
}

/// Get spinner character for a given frame index.
pub fn spinner_char(frame: usize) -> &'static str {
    chars::SPINNER[frame % chars::SPINNER.len()]
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Parse a hex color string (e.g. `"#FF5733"`) into a ratatui `Color`.
pub fn hex(color_hex: &str) -> Color {
    let h = color_hex.trim_start_matches('#');
    if h.len() == 6 {
        if let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&h[0..2], 16),
            u8::from_str_radix(&h[2..4], 16),
            u8::from_str_radix(&h[4..6], 16),
        ) {
            return Color::Rgb(r, g, b);
        }
    }
    Color::White // fallback
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hex_color() {
        assert_eq!(hex("#FF5733"), Color::Rgb(255, 87, 51));
        assert_eq!(hex("#000000"), Color::Rgb(0, 0, 0));
        assert_eq!(hex("#FFFFFF"), Color::Rgb(255, 255, 255));
    }

    #[test]
    fn test_default_theme_round_trip() {
        let original = current_theme();
        set_theme(ThemeConfig::default());
        let restored = current_theme();
        assert_eq!(original, restored);
    }
}
