//! Small drawing helpers shared by the screens.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::model::StepStatus;
use crate::tui::theme::Theme;

/// Truncate to `max` display columns, appending an ellipsis when cut.
pub fn truncate(text: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if text.width() <= max {
        return text.to_string();
    }
    let mut out = String::new();
    let mut width = 0;
    for ch in text.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if width + ch_width > max.saturating_sub(1) {
            break;
        }
        out.push(ch);
        width += ch_width;
    }
    out.push('…');
    out
}

pub fn pad(text: &str, width: usize) -> String {
    let text = truncate(text, width);
    let mut out = text;
    let current = out.width();
    for _ in current..width {
        out.push(' ');
    }
    out
}

/// A segmented meter, e.g. `▰▰▰▱▱▱▱`.
pub fn meter(filled: usize, total: usize, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if total == 0 {
        return "▱".repeat(width);
    }
    let ratio = (filled as f32 / total as f32).clamp(0.0, 1.0);
    let on = ((ratio * width as f32).round() as usize).min(width);
    format!("{}{}", "▰".repeat(on), "▱".repeat(width - on))
}

pub fn pill(
    text: &str,
    background: ratatui::style::Color,
    foreground: ratatui::style::Color,
) -> Span<'static> {
    Span::styled(
        format!(" {text} "),
        Style::default()
            .bg(background)
            .fg(foreground)
            .add_modifier(Modifier::BOLD),
    )
}

/// A full-width hairline with a soft gradient.
pub fn rule(theme: &Theme, width: usize, tick: u64) -> Line<'static> {
    if width == 0 {
        return Line::default();
    }
    let mut spans = Vec::with_capacity(width);
    for index in 0..width {
        let position = index as f32 / width as f32;
        let shift = (tick / 6) as f32 * 0.02;
        let color = crate::tui::theme::blend(
            theme.border,
            theme.accent,
            ((position + shift) % 1.0) * 0.12,
        );
        spans.push(Span::styled("─", Style::default().fg(color)));
    }
    Line::from(spans)
}

pub fn status_glyph(theme: &Theme, status: StepStatus, tick: u64) -> (&'static str, Style) {
    if status == StepStatus::Running {
        (theme.spinner(tick), theme.status_style(status))
    } else {
        (theme.glyph(status), theme.status_style(status))
    }
}

/// Centre a fixed-size box inside `area`, shrinking when the terminal is small.
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2)).max(1);
    let height = height.min(area.height.saturating_sub(1)).max(1);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width, height)
}

/// Wrap plain text to `width`, preserving explicit newlines.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            let candidate_width = current.width() + usize::from(!current.is_empty()) + word.width();
            if candidate_width > width && !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(&truncate(word, width));
        }
        lines.push(current);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_wide_text() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
        assert_eq!(truncate("", 4), "");
    }

    #[test]
    fn truncates_by_display_width() {
        // Each CJK character occupies two columns.
        assert_eq!(truncate("日本語です", 5), "日本…");
    }

    #[test]
    fn meter_is_exact_width() {
        assert_eq!(meter(0, 4, 4), "▱▱▱▱");
        assert_eq!(meter(2, 4, 4), "▰▰▱▱");
        assert_eq!(meter(4, 4, 4), "▰▰▰▰");
        assert_eq!(meter(9, 4, 4), "▰▰▰▰");
        assert_eq!(meter(0, 0, 3), "▱▱▱");
    }

    #[test]
    fn pads_to_width() {
        assert_eq!(pad("ab", 4), "ab  ");
        assert_eq!(pad("abcdef", 3), "ab…");
    }

    #[test]
    fn centers_within_bounds() {
        let area = Rect::new(0, 0, 100, 40);
        let rect = centered(area, 60, 20);
        assert_eq!(rect.x, 20);
        assert_eq!(rect.y, 10);

        let tiny = Rect::new(0, 0, 10, 4);
        let rect = centered(tiny, 60, 20);
        assert!(rect.width <= tiny.width);
        assert!(rect.height <= tiny.height);
    }

    #[test]
    fn wraps_words() {
        assert_eq!(wrap("one two three", 7), vec!["one two", "three"]);
    }
}
