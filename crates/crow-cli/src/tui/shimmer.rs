//! Shimmer animation for the "Working..." spinner text.
//!
//! Produces a sweep-highlight effect that travels across a text string,
//! creating a polished loading indicator. The animation is time-based,
//! synchronized to process start, so it runs smoothly regardless of
//! render frequency.
//!
//! Inspired by codex's `shimmer.rs` implementation.

use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::Span;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

static PROCESS_START: OnceLock<Instant> = OnceLock::new();

fn elapsed_since_start() -> Duration {
    let start = PROCESS_START.get_or_init(Instant::now);
    start.elapsed()
}

/// Produce a sequence of spans for `text` with a time-based shimmer effect.
///
/// Each character is styled individually: a bright highlight band sweeps
/// across the text with a cosine-smoothed falloff. The sweep repeats every
/// `sweep_seconds` (default 2.0s).
///
/// Uses the theme module's color functions to respect light/dark mode.
pub fn shimmer_spans(text: &str) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }

    // Time-based sweep synchronized to process start
    let padding = 10usize;
    let period = chars.len() + padding * 2;
    let sweep_seconds = 2.0f32;
    let pos_f =
        (elapsed_since_start().as_secs_f32() % sweep_seconds) / sweep_seconds * (period as f32);
    let pos = pos_f as usize;
    let band_half_width = 5.0;

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(chars.len());

    // Use theme-aware colors
    let theme = crate::tui::theme::current_theme();
    let base_color = theme.text_secondary;
    let highlight_color = theme.accent_system;

    for (i, ch) in chars.iter().enumerate() {
        let i_pos = i as isize + padding as isize;
        let pos = pos as isize;
        let dist = (i_pos - pos).abs() as f32;

        // Cosine-smoothed intensity falloff
        let t = if dist <= band_half_width {
            let x = std::f32::consts::PI * (dist / band_half_width);
            0.5 * (1.0 + x.cos())
        } else {
            0.0
        };

        let style = if t > 0.01 {
            // In the highlight band: blend between base and highlight
            let highlight = t.clamp(0.0, 1.0);
            blend_style(base_color, highlight_color, highlight)
        } else {
            // Outside highlight: dim base
            Style::new().fg(base_color).add_modifier(Modifier::DIM)
        };

        spans.push(Span::styled(ch.to_string(), style));
    }
    spans
}

/// Produce shimmer spans with a custom base and highlight color.
pub fn shimmer_spans_colored(
    text: &str,
    base: Color,
    highlight: Color,
) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }

    let padding = 10usize;
    let period = chars.len() + padding * 2;
    let sweep_seconds = 2.0f32;
    let pos_f =
        (elapsed_since_start().as_secs_f32() % sweep_seconds) / sweep_seconds * (period as f32);
    let pos = pos_f as usize;
    let band_half_width = 5.0;

    let mut spans = Vec::with_capacity(chars.len());
    for (i, ch) in chars.iter().enumerate() {
        let i_pos = i as isize + padding as isize;
        let pos = pos as isize;
        let dist = (i_pos - pos).abs() as f32;

        let t = if dist <= band_half_width {
            let x = std::f32::consts::PI * (dist / band_half_width);
            0.5 * (1.0 + x.cos())
        } else {
            0.0
        };

        let style = if t > 0.01 {
            blend_style(base, highlight, t.clamp(0.0, 1.0))
        } else {
            Style::new().fg(base).add_modifier(Modifier::DIM)
        };

        spans.push(Span::styled(ch.to_string(), style));
    }
    spans
}

/// Blend two colors with a given weight (0.0 = pure base, 1.0 = pure highlight).
fn blend_style(base: Color, highlight: Color, weight: f32) -> Style {
    match (base, highlight) {
        (Color::Rgb(br, bg, bb), Color::Rgb(hr, hg, hb)) => {
            let r = lerp_u8(br, hr, weight);
            let g = lerp_u8(bg, hg, weight);
            let b = lerp_u8(bb, hb, weight);
            Style::new().fg(Color::Rgb(r, g, b)).bold()
        }
        _ => {
            // Fallback for non-RGB colors: use modifier-based intensity
            if weight < 0.2 {
                Style::new().fg(base).add_modifier(Modifier::DIM)
            } else if weight < 0.6 {
                Style::new().fg(base)
            } else {
                Style::new().fg(highlight).bold()
            }
        }
    }
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    let a = a as f32;
    let b = b as f32;
    (a + (b - a) * t).round().clamp(0.0, 255.0) as u8
}
