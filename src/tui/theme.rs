//! Semantic theme. Rendering asks for roles, never for raw colours.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use crate::model::StepStatus;

#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    pub fg: Color,
    pub muted: Color,
    pub faint: Color,
    pub accent: Color,
    pub accent2: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    pub border: Color,
    pub focus: Color,
    pub selection: Color,
    pub selection_fg: Color,
    pub panel: Color,
    /// True when colour is unavailable; rely on modifiers and glyphs.
    pub mono: bool,
}

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

impl Theme {
    pub fn load(name: &str) -> Self {
        let mono = matches!(name, "mono")
            || std::env::var_os("NO_COLOR").is_some()
            || matches!(std::env::var("TERM").as_deref(), Ok("dumb"));

        if mono {
            return Self {
                name: "mono".into(),
                fg: Color::Reset,
                muted: Color::Reset,
                faint: Color::Reset,
                accent: Color::Reset,
                accent2: Color::Reset,
                success: Color::Reset,
                warning: Color::Reset,
                danger: Color::Reset,
                border: Color::Reset,
                focus: Color::Reset,
                selection: Color::Reset,
                selection_fg: Color::Reset,
                panel: Color::Reset,
                mono: true,
            };
        }

        match name {
            "light" => Self {
                name: "light".into(),
                fg: Color::Rgb(30, 36, 46),
                muted: Color::Rgb(96, 108, 124),
                faint: Color::Rgb(150, 160, 175),
                accent: Color::Rgb(0, 118, 190),
                accent2: Color::Rgb(120, 78, 200),
                success: Color::Rgb(20, 130, 70),
                warning: Color::Rgb(170, 110, 0),
                danger: Color::Rgb(190, 45, 45),
                border: Color::Rgb(200, 208, 218),
                focus: Color::Rgb(0, 118, 190),
                selection: Color::Rgb(222, 235, 248),
                selection_fg: Color::Rgb(10, 20, 30),
                panel: Color::Rgb(246, 248, 251),
                mono: false,
            },
            _ => Self {
                name: "tide".into(),
                fg: Color::Rgb(216, 224, 236),
                muted: Color::Rgb(122, 136, 156),
                faint: Color::Rgb(66, 78, 94),
                accent: Color::Rgb(96, 200, 246),
                accent2: Color::Rgb(160, 128, 255),
                success: Color::Rgb(104, 222, 148),
                warning: Color::Rgb(246, 200, 96),
                danger: Color::Rgb(246, 116, 116),
                border: Color::Rgb(44, 54, 70),
                focus: Color::Rgb(96, 200, 246),
                selection: Color::Rgb(26, 38, 52),
                selection_fg: Color::Rgb(238, 246, 255),
                panel: Color::Rgb(16, 21, 29),
                mono: false,
            },
        }
    }

    pub fn spinner(&self, tick: u64) -> &'static str {
        SPINNER[(tick as usize / 2) % SPINNER.len()]
    }

    pub fn status(&self, status: StepStatus) -> Color {
        match status {
            StepStatus::Ok => self.success,
            StepStatus::Running => self.accent,
            StepStatus::Failed => self.danger,
            StepStatus::Skipped => self.faint,
            StepStatus::Cancelled => self.warning,
            StepStatus::Planned => self.accent2,
            StepStatus::Pending => self.muted,
        }
    }

    pub fn status_style(&self, status: StepStatus) -> Style {
        if self.mono {
            let modifier = match status {
                StepStatus::Ok | StepStatus::Failed => Modifier::BOLD,
                StepStatus::Skipped | StepStatus::Pending => Modifier::DIM,
                _ => Modifier::empty(),
            };
            return Style::default().add_modifier(modifier);
        }
        Style::default().fg(self.status(status))
    }

    pub fn glyph(&self, status: StepStatus) -> &'static str {
        match status {
            StepStatus::Running => "▶",
            other => other.glyph(),
        }
    }

    /// A left rail colour that cycles while a run is active.
    pub fn pulse(&self, tick: u64) -> Color {
        if self.mono {
            return Color::Reset;
        }
        let mix = (tick % 40) as f32 / 40.0;
        let wave = (mix * std::f32::consts::TAU).sin() * 0.5 + 0.5;
        blend(self.accent, self.accent2, wave * 0.65)
    }

    /// Interpolate a gradient across `text`, one colour per character.
    pub fn gradient(&self, text: &str, tick: u64) -> Vec<Span<'static>> {
        if self.mono {
            return vec![Span::styled(text.to_string(), Style::default())];
        }
        let chars: Vec<char> = text.chars().collect();
        if chars.is_empty() {
            return Vec::new();
        }
        let shift = (tick / 3) as f32 * 0.05;
        chars
            .into_iter()
            .enumerate()
            .map(|(index, ch)| {
                let position = index as f32 / chars_len(text) as f32;
                let position = (position + shift) % 1.0;
                Span::styled(
                    ch.to_string(),
                    Style::default().fg(blend(self.accent, self.accent2, position)),
                )
            })
            .collect()
    }

    pub fn dim(&self) -> Style {
        Style::default().fg(self.muted)
    }

    pub fn faint_style(&self) -> Style {
        Style::default().fg(self.faint)
    }

    pub fn title(&self) -> Style {
        Style::default().fg(self.fg).add_modifier(Modifier::BOLD)
    }
}

fn chars_len(text: &str) -> usize {
    text.chars().count().max(1)
}

/// Linear blend between two RGB colours; returns `from` for non-RGB inputs.
pub fn blend(from: Color, to: Color, position: f32) -> Color {
    let position = position.clamp(0.0, 1.0);
    match (from, to) {
        (Color::Rgb(fr, fg, fb), Color::Rgb(tr, tg, tb)) => Color::Rgb(
            lerp(fr, tr, position),
            lerp(fg, tg, position),
            lerp(fb, tb, position),
        ),
        _ => from,
    }
}

fn lerp(from: u8, to: u8, position: f32) -> u8 {
    (from as f32 + (to as f32 - from as f32) * position).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_is_bounded() {
        assert_eq!(
            blend(Color::Rgb(0, 0, 0), Color::Rgb(255, 255, 255), 0.5),
            Color::Rgb(128, 128, 128)
        );
        assert_eq!(
            blend(Color::Rgb(10, 10, 10), Color::Rgb(20, 20, 20), -1.0),
            Color::Rgb(10, 10, 10)
        );
        assert_eq!(
            blend(Color::Rgb(10, 10, 10), Color::Rgb(20, 20, 20), 5.0),
            Color::Rgb(20, 20, 20)
        );
    }

    #[test]
    fn mono_theme_has_no_colour() {
        let theme = Theme::load("mono");
        assert!(theme.mono);
        assert_eq!(theme.status(StepStatus::Ok), Color::Reset);
    }

    #[test]
    fn spinner_advances_over_time() {
        let theme = Theme::load("tide");
        assert_ne!(theme.spinner(0), theme.spinner(4));
    }

    #[test]
    fn status_glyphs_are_distinct() {
        let statuses = [
            StepStatus::Pending,
            StepStatus::Running,
            StepStatus::Ok,
            StepStatus::Failed,
            StepStatus::Skipped,
            StepStatus::Cancelled,
            StepStatus::Planned,
        ];
        let mut glyphs: Vec<&str> = statuses.iter().map(|s| s.glyph()).collect();
        glyphs.sort_unstable();
        glyphs.dedup();
        assert_eq!(glyphs.len(), statuses.len());
    }
}
