//! Layout and painting.
//!
//! Rendering is pure: it reads [`App`] and writes to the frame. Nothing here
//! spawns processes, locks, or reads files, so a frame can never stall a run.

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::model::{Density, RunStatus, StepStatus};
use crate::tui::app::{App, Focus, LogKind, Modal, RunState};
use crate::tui::theme::Theme;
use crate::tui::widgets::{self, meter, pad, truncate, wrap};

const HEADER_HEIGHT: u16 = 3;
const GUTTER: u16 = 1;
const TINY_WIDTH: u16 = 62;
const TINY_HEIGHT: u16 = 14;

#[derive(Debug, Clone, Copy)]
pub struct Areas {
    pub header: Rect,
    pub sidebar: Rect,
    pub steps: Rect,
    pub log: Rect,
    pub footer: Rect,
}

/// Height of the log *pane*, including its title rule.
fn log_pane_height(area: Rect, show_log: bool) -> u16 {
    if !show_log || area.height < 18 {
        return 0;
    }
    let footer = if area.height >= 8 { 1 } else { 0 };
    let body = area.height.saturating_sub(HEADER_HEIGHT + footer);
    (area.height / 3).clamp(5, 14).min(body.saturating_sub(5))
}

/// Number of text lines the log can show, used for scroll clamping.
pub fn log_inner_height(size: Rect, show_log: bool) -> usize {
    log_pane_height(size, show_log).saturating_sub(1) as usize
}

/// Split `area` into the fixed regions of the main screen.
pub fn layout(area: Rect, app: &App) -> Areas {
    let header_height = HEADER_HEIGHT.min(area.height);
    let footer_height = if area.height >= 8 { 1 } else { 0 };
    let body_height = area.height.saturating_sub(header_height + footer_height);

    let sidebar_width = (area.width / 4).clamp(22, 34);
    let log_height = log_pane_height(area, app.show_log);
    let steps_height = body_height.saturating_sub(log_height);

    let header = Rect::new(area.x, area.y, area.width, header_height);
    let body = Rect::new(area.x, area.y + header_height, area.width, body_height);
    let footer = Rect::new(
        area.x,
        area.y + header_height + body_height,
        area.width,
        footer_height,
    );

    let sidebar = Rect::new(body.x, body.y, sidebar_width.min(body.width), body.height);
    let main_x = body.x + sidebar.width + GUTTER;
    let main_width = body.width.saturating_sub(sidebar.width + GUTTER);
    let steps = Rect::new(main_x, body.y, main_width, steps_height);
    let log = Rect::new(main_x, body.y + steps_height, main_width, log_height);

    Areas {
        header,
        sidebar,
        steps,
        log,
        footer,
    }
}

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    if area.width < TINY_WIDTH || area.height < TINY_HEIGHT {
        draw_tiny(frame, app, area);
        return;
    }

    let areas = layout(area, app);
    draw_header(frame, app, areas.header);
    draw_sidebar(frame, app, areas.sidebar);
    draw_steps(frame, app, areas.steps);
    if areas.log.height > 0 {
        draw_log(frame, app, areas.log);
    }
    draw_footer(frame, app, areas.footer);
    draw_toast(frame, app, areas.footer);

    if let Some(modal) = &app.modal {
        // Blank the stage so the modal never shares a row with background text.
        frame.render_widget(Clear, area);
        frame.render_widget(
            Block::default().style(Style::default().bg(app.theme.panel)),
            area,
        );
        draw_modal(frame, app, modal, area);
    }
}

// ---------------------------------------------------------------------------
// Header
// ---------------------------------------------------------------------------

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    frame.render_widget(
        Block::default().style(Style::default().bg(theme.panel)),
        area,
    );
    if area.height < 1 || area.width < 8 {
        return;
    }

    let facts = &app.facts;
    let repo = facts.display_name();
    let branch = facts.branch.clone().unwrap_or_else(|| "(detached)".into());

    // Row 0: brand on the left, repository on the right.
    let brand = theme.gradient("tug", app.tick);
    let mut left = brand;
    left.push(Span::styled(
        "  ship pipelines",
        Style::default().fg(theme.faint),
    ));

    let mut right: Vec<Span<'static>> = vec![Span::styled(
        repo,
        Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
    )];
    if let Some(remote) = &facts.remote {
        right.push(Span::styled(
            format!("  {remote}"),
            Style::default().fg(theme.faint),
        ));
    }
    if let Some(tag) = &facts.latest_tag {
        right.push(Span::styled(
            format!("  {tag}"),
            Style::default().fg(theme.warning),
        ));
    }

    let row0 = Rect::new(area.x + 1, area.y, area.width.saturating_sub(2), 1);
    let split = (area.width as usize * 3 / 5) as u16;
    let left_width = split.min(row0.width) as usize;
    let right_width = row0.width.saturating_sub(split) as usize;
    frame.render_widget(
        Paragraph::new(Line::from(fit_spans(left, left_width.saturating_sub(1))))
            .style(Style::default().bg(theme.panel)),
        Rect::new(row0.x, row0.y, split.min(row0.width), 1),
    );
    frame.render_widget(
        Paragraph::new(Line::from(fit_spans(right, right_width.saturating_sub(1))))
            .alignment(Alignment::Right)
            .style(Style::default().bg(theme.panel)),
        Rect::new(
            row0.x + split.min(row0.width),
            row0.y,
            row0.width.saturating_sub(split),
            1,
        ),
    );

    // Row 1: branch state and config provenance.
    if area.height >= 2 {
        let mut spans = vec![
            Span::styled(" ", Style::default()),
            widgets::pill(&branch, theme.selection, theme.accent),
        ];
        if facts.dirty {
            spans.push(Span::styled(" ", Style::default()));
            spans.push(widgets::pill("dirty", theme.selection, theme.warning));
        } else if facts.in_repo {
            spans.push(Span::styled(" ", Style::default()));
            spans.push(widgets::pill("clean", theme.selection, theme.success));
        }
        if facts.staged > 0 {
            spans.push(Span::styled(
                format!("  {} staged", facts.staged),
                Style::default().fg(theme.accent2),
            ));
        }
        if facts.unstaged > 0 {
            spans.push(Span::styled(
                format!("  {} unstaged", facts.unstaged),
                Style::default().fg(theme.muted),
            ));
        }
        if facts.untracked > 0 {
            spans.push(Span::styled(
                format!("  {} untracked", facts.untracked),
                Style::default().fg(theme.muted),
            ));
        }
        if facts.ahead > 0 || facts.behind > 0 {
            spans.push(Span::styled(
                format!("  ↑{} ↓{}", facts.ahead, facts.behind),
                Style::default().fg(theme.accent),
            ));
        }
        let config_name = app
            .loaded
            .path
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "no config".to_string());
        let mut meta: Vec<Span<'static>> = Vec::new();
        if let Some(profile) = &app.profile {
            meta.push(Span::styled(
                format!("profile {profile}"),
                Style::default().fg(theme.accent2),
            ));
            meta.push(Span::styled("  ·  ", Style::default().fg(theme.faint)));
        }
        meta.push(Span::styled(
            format!("{} pipeline(s)", app.pipelines.len()),
            Style::default().fg(theme.muted),
        ));
        meta.push(Span::styled("  ·  ", Style::default().fg(theme.faint)));
        meta.push(Span::styled(config_name, Style::default().fg(theme.faint)));

        let row1 = Rect::new(area.x + 1, area.y + 1, area.width.saturating_sub(2), 1);
        let split = (area.width as usize * 3 / 5) as u16;
        let left_width = split.min(row1.width) as usize;
        let right_width = row1.width.saturating_sub(split) as usize;
        frame.render_widget(
            Paragraph::new(Line::from(fit_spans(spans, left_width.saturating_sub(1))))
                .style(Style::default().bg(theme.panel)),
            Rect::new(row1.x, row1.y, split.min(row1.width), 1),
        );
        frame.render_widget(
            Paragraph::new(Line::from(fit_spans(meta, right_width.saturating_sub(1))))
                .alignment(Alignment::Right)
                .style(Style::default().bg(theme.panel)),
            Rect::new(
                row1.x + split.min(row1.width),
                row1.y,
                row1.width.saturating_sub(split),
                1,
            ),
        );
    }

    // Row 2: a soft animated rule.
    if area.height >= 3 {
        frame.render_widget(
            Paragraph::new(widgets::rule(theme, area.width as usize, app.tick))
                .style(Style::default().bg(theme.panel)),
            Rect::new(area.x, area.y + 2, area.width, 1),
        );
    }
}

// ---------------------------------------------------------------------------
// Sidebar
// ---------------------------------------------------------------------------

fn draw_sidebar(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let focused = app.focus == Focus::Pipelines;
    let border = if focused { theme.border } else { theme.faint };

    let count = format!("{}", app.visible().len());
    let block = panel(theme, area, "pipelines", border, &count, theme.accent);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut budget = inner.height as usize;

    if app.filtering || !app.filter.is_empty() {
        let marker = if app.filtering { "_" } else { "" };
        lines.push(Line::from(vec![
            Span::styled(" /", Style::default().fg(theme.accent)),
            Span::styled(
                format!("{}{marker}", app.filter),
                Style::default().fg(theme.fg),
            ),
        ]));
        budget = budget.saturating_sub(1);
    }

    let visible = app.visible();
    if visible.is_empty() || budget == 0 {
        if budget > 0 {
            lines.push(Line::from(Span::styled(
                "  no pipelines",
                Style::default().fg(theme.muted),
            )));
            if budget > 1 {
                lines.push(Line::from(Span::styled(
                    "  run `tug init`",
                    Style::default().fg(theme.faint),
                )));
            }
        }
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    }

    // Comfortable density always shows descriptions and puts air between rows.
    let comfortable = app.loaded.config.ui.density == Density::Comfortable;
    let with_descriptions = comfortable || visible.len() * 2 <= budget;

    // Scroll the list so the selection stays on screen.
    let per_entry = if !with_descriptions {
        1
    } else if comfortable {
        3
    } else {
        2
    };
    let capacity = (budget / per_entry).max(1);
    let offset = if app.selected >= capacity {
        app.selected + 1 - capacity
    } else {
        0
    };

    for (position, index) in visible.iter().enumerate().skip(offset).take(capacity) {
        let Some(row) = app.pipelines.get(*index) else {
            continue;
        };
        let selected = position == app.selected;
        let background = if selected {
            theme.selection
        } else {
            theme.panel
        };

        let mut name_spans = vec![Span::styled(
            if selected { "▌ " } else { "  " },
            Style::default()
                .fg(if selected { theme.accent } else { theme.panel })
                .bg(background),
        )];
        name_spans.push(Span::styled(
            truncate(&row.name, inner.width.saturating_sub(6) as usize),
            Style::default()
                .fg(if selected {
                    theme.selection_fg
                } else {
                    theme.fg
                })
                .bg(background)
                .add_modifier(if selected {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ));

        let mut right: Vec<Span<'static>> = Vec::new();
        if let Some(status) = row.status {
            right.push(Span::styled(
                status.glyph().to_string(),
                theme.status_style(status).bg(background),
            ));
            right.push(Span::raw(" "));
        }
        right.push(Span::styled(
            format!("{} ", row.steps),
            Style::default().fg(theme.faint).bg(background),
        ));

        let used: usize = name_spans
            .iter()
            .map(|s| s.content.width())
            .sum::<usize>()
            .saturating_sub(2);
        let right_width: usize = right.iter().map(|s| s.content.width()).sum();
        let fill = (inner.width as usize)
            .saturating_sub(2)
            .saturating_sub(used + right_width);

        let mut spans: Vec<Span<'static>> = Vec::new();
        spans.extend(name_spans);
        spans.push(Span::styled(
            " ".repeat(fill),
            Style::default().bg(background),
        ));
        spans.extend(right);
        lines.push(Line::from(spans).style(Style::default().bg(background)));

        if with_descriptions {
            let description = if row.description.is_empty() {
                if let Some(gate) = &row.gate {
                    format!("gated: {gate}")
                } else {
                    String::new()
                }
            } else {
                row.description.clone()
            };
            let color = if row.gate.is_some() {
                theme.warning
            } else {
                theme.muted
            };
            lines.push(Line::from(Span::styled(
                format!(
                    "   {}",
                    pad(&description, inner.width.saturating_sub(4) as usize)
                ),
                Style::default().fg(color).bg(background),
            )));
            if comfortable {
                lines.push(Line::from(""));
            }
        }
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

// ---------------------------------------------------------------------------
// Steps
// ---------------------------------------------------------------------------

fn draw_steps(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let focused = app.focus == Focus::Pipelines;
    let border = if focused { theme.border } else { theme.faint };

    let (title, right) = match (&app.run, app.selected_pipeline()) {
        (Some(run), _) => {
            let progress = format!("{}/{}", run.done(), run.steps.len());
            (run.pipeline.clone(), progress)
        }
        (None, Some(row)) => (row.name.clone(), format!("{} steps", row.steps)),
        (None, None) => ("pipeline".to_string(), String::new()),
    };

    let block = panel(theme, area, &title, border, &right, theme.accent2);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    match &app.run {
        Some(run) => draw_run_steps(frame, app, inner, run),
        None => draw_idle_steps(frame, app, inner),
    }
}

fn draw_idle_steps(frame: &mut Frame, app: &App, inner: Rect) {
    let theme = &app.theme;
    let Some(row) = app.selected_pipeline() else {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    "no pipeline selected",
                    Style::default().fg(theme.muted),
                )),
                Line::from(Span::styled(
                    "press i to write a starter tug.toml",
                    Style::default().fg(theme.faint),
                )),
            ])
            .alignment(Alignment::Center),
            inner,
        );
        return;
    };

    let description = if row.description.is_empty() {
        "no description".to_string()
    } else {
        row.description.clone()
    };
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut head = vec![
        Span::styled(description, Style::default().fg(theme.muted)),
        Span::styled("  ·  ", Style::default().fg(theme.faint)),
        Span::styled(
            format!("{} steps", row.steps),
            Style::default().fg(theme.accent),
        ),
    ];
    if let Some(gate) = &row.gate {
        head.push(Span::styled("  ·  ", Style::default().fg(theme.faint)));
        head.push(Span::styled(
            format!("gated: {gate}"),
            Style::default().fg(theme.warning),
        ));
    }
    lines.push(Line::from(head));
    lines.push(Line::from(""));

    let defined = app
        .loaded
        .config
        .pipelines
        .get(&row.name)
        .map(|pipeline| pipeline.steps.as_slice())
        .unwrap_or(&[]);
    let ctx = app.ctx(Some(&row.name), false);

    let budget = inner.height.saturating_sub(3) as usize;
    for (index, step) in defined.iter().take(budget).enumerate() {
        let mutating = step.action.is_mutating();
        let marker = if mutating { "◆" } else { "◇" };
        let marker_color = if mutating { theme.accent2 } else { theme.muted };
        let label = step.title();
        lines.push(Line::from(vec![
            Span::styled(format!("  {marker} "), Style::default().fg(marker_color)),
            Span::styled(
                format!("{:>2}  ", index + 1),
                Style::default().fg(theme.faint),
            ),
            Span::styled(
                pad(&label, 20),
                Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                truncate(
                    &step_hint(step, &ctx),
                    inner.width.saturating_sub(28) as usize,
                ),
                Style::default().fg(theme.muted),
            ),
        ]));
    }

    if inner.height >= 3 {
        lines.push(Line::from(""));
        let hint_y = inner.y + inner.height - 1;
        let hint = Rect::new(inner.x, hint_y, inner.width, 1);
        frame.render_widget(Paragraph::new(lines), inner);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("enter", Style::default().fg(theme.accent)),
                Span::styled(" review plan   ", Style::default().fg(theme.muted)),
                Span::styled("d", Style::default().fg(theme.accent)),
                Span::styled(" dry run ", Style::default().fg(theme.muted)),
            ]))
            .alignment(Alignment::Right),
            hint,
        );
        return;
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_run_steps(frame: &mut Frame, app: &App, inner: Rect, run: &RunState) {
    let theme = &app.theme;
    let total = run.steps.len();
    let done = run.done();

    let status_span = match run.status {
        Some(RunStatus::Ok) if run.dry_run => widgets::pill("planned", theme.accent2, theme.panel),
        Some(RunStatus::Ok) => widgets::pill("shipped", theme.success, theme.panel),
        Some(RunStatus::Failed) => widgets::pill("failed", theme.danger, theme.panel),
        Some(RunStatus::Cancelled) => widgets::pill("cancelled", theme.warning, theme.panel),
        None if run.dry_run => widgets::pill("dry run", theme.accent2, theme.panel),
        None => widgets::pill("running", theme.accent, theme.panel),
    };

    let meter_width = (inner.width as usize).saturating_sub(34).clamp(8, 32);
    let header = vec![
        Span::styled(
            meter(done, total, meter_width),
            Style::default().fg(theme.pulse(app.tick)),
        ),
        Span::styled(
            format!("  {done}/{total}"),
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {:.1}s", run.elapsed_ms as f64 / 1000.0),
            Style::default().fg(theme.muted),
        ),
        Span::styled("  ", Style::default()),
        status_span,
    ];
    let mut lines = vec![Line::from(header), Line::from("")];
    let list_height = inner.height.saturating_sub(2) as usize;

    let focus = run
        .current
        .or_else(|| {
            run.steps
                .iter()
                .position(|step| step.status == StepStatus::Failed)
        })
        .or_else(|| run.steps.iter().rposition(|step| step.status.is_terminal()))
        .unwrap_or(0);
    let offset = if list_height > 0 && run.steps.len() > list_height {
        focus
            .saturating_sub(list_height.saturating_sub(1))
            .min(run.steps.len() - list_height)
    } else {
        0
    };

    for (index, step) in run.steps.iter().enumerate().skip(offset).take(list_height) {
        let current = run.current == Some(index);
        let background = if current {
            theme.selection
        } else {
            theme.panel
        };
        let (glyph, glyph_style) = widgets::status_glyph(theme, step.status, app.tick);

        let label_style = match step.status {
            StepStatus::Running => Style::default()
                .fg(theme.selection_fg)
                .bg(background)
                .add_modifier(Modifier::BOLD),
            StepStatus::Failed => Style::default().fg(theme.danger).bg(background),
            StepStatus::Ok => Style::default().fg(theme.fg).bg(background),
            StepStatus::Skipped | StepStatus::Pending => {
                Style::default().fg(theme.muted).bg(background)
            }
            _ => Style::default().fg(theme.fg).bg(background),
        };

        let label_width = 22usize
            .min((inner.width as usize).saturating_sub(24) / 2)
            .max(10);
        let duration = if step.duration_ms >= 1000 {
            format!("{:.1}s", step.duration_ms as f64 / 1000.0)
        } else if step.duration_ms > 0 {
            format!("{}ms", step.duration_ms)
        } else {
            String::new()
        };
        let detail_width = (inner.width as usize)
            .saturating_sub(label_width + 16)
            .max(4);

        let mut spans = vec![
            Span::styled("  ", Style::default().bg(background)),
            Span::styled(format!("{glyph} "), glyph_style.bg(background)),
            Span::styled(
                format!("{:>2}  ", index + 1),
                Style::default().fg(theme.faint).bg(background),
            ),
            Span::styled(pad(&step.label, label_width), label_style),
            Span::styled(
                pad(&step.detail, detail_width),
                Style::default().fg(theme.muted).bg(background),
            ),
        ];
        let used: usize = spans.iter().map(|span| span.content.width()).sum();
        let fill = (inner.width as usize).saturating_sub(used + duration.width() + 1);
        spans.push(Span::styled(
            " ".repeat(fill),
            Style::default().bg(background),
        ));
        spans.push(Span::styled(
            format!("{duration} "),
            Style::default().fg(theme.faint).bg(background),
        ));

        lines.push(Line::from(spans).style(Style::default().bg(background)));

        // Give failures and the live step their own summary line.
        if (current || step.status == StepStatus::Failed) && !step.summary.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("        ", Style::default().bg(background)),
                Span::styled(
                    truncate(&step.summary, inner.width.saturating_sub(10) as usize),
                    Style::default()
                        .fg(if step.status == StepStatus::Failed {
                            theme.danger
                        } else {
                            theme.faint
                        })
                        .bg(background),
                ),
            ]));
        }
    }

    if let Some(error) = &run.error {
        if run.status == Some(RunStatus::Failed) && !error.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("  {error}"),
                Style::default().fg(theme.danger),
            )));
        }
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

// ---------------------------------------------------------------------------
// Log
// ---------------------------------------------------------------------------

fn draw_log(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    if area.height == 0 {
        return;
    }

    // Title rule: `─ log ─────────── follow · 1.2k lines ─`
    let mut title = vec![
        Span::styled("─ ", Style::default().fg(theme.border)),
        Span::styled(
            "log",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" ", Style::default()),
    ];
    let right = if app.log.follow {
        " follow ".to_string()
    } else {
        format!(" paused · {} lines ", app.log.lines.len())
    };
    let left_width: usize = title.iter().map(|s| s.content.width()).sum();
    let fill = (area.width as usize).saturating_sub(left_width + right.width());
    title.push(Span::styled(
        "─".repeat(fill),
        Style::default().fg(theme.border),
    ));
    title.push(Span::styled(
        right,
        Style::default().fg(if app.log.follow {
            theme.success
        } else {
            theme.warning
        }),
    ));

    frame.render_widget(
        Paragraph::new(Line::from(title)),
        Rect::new(area.x, area.y, area.width, 1),
    );

    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 1);
    if body.height == 0 {
        return;
    }

    if app.log.lines.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "  output appears here while a pipeline runs",
                Style::default().fg(theme.faint),
            )),
            body,
        );
        return;
    }

    let viewport = body.height as usize;
    let total = app.log.lines.len();
    let end = total.saturating_sub(app.log.scroll.min(total));
    let start = end.saturating_sub(viewport);

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(end - start);
    for line in app.log.lines.iter().skip(start).take(end - start) {
        let (color, modifier, marker) = match line.kind {
            LogKind::Command => (theme.accent, Modifier::empty(), "$"),
            LogKind::Out => (theme.fg, Modifier::empty(), " "),
            LogKind::Err => (theme.danger, Modifier::empty(), "!"),
            LogKind::Note => (theme.muted, Modifier::empty(), "·"),
            LogKind::Warn => (theme.warning, Modifier::empty(), "!"),
            LogKind::Error => (theme.danger, Modifier::BOLD, "✗"),
            LogKind::Ok => (theme.success, Modifier::BOLD, "✓"),
            LogKind::Fail => (theme.danger, Modifier::BOLD, "✗"),
            LogKind::Cancel => (theme.warning, Modifier::BOLD, "⊘"),
            LogKind::System => (theme.accent2, Modifier::BOLD, "▸"),
        };
        let text = if marker == " " || (marker == "$" && line.text.starts_with("$ ")) {
            line.text.clone()
        } else {
            format!("{marker} {}", line.text)
        };
        lines.push(Line::from(vec![
            Span::styled("▏", Style::default().fg(theme.faint)),
            Span::styled(
                truncate(&text, body.width.saturating_sub(2) as usize),
                Style::default().fg(color).add_modifier(modifier),
            ),
        ]));
    }

    frame.render_widget(Paragraph::new(lines), body);
}

// ---------------------------------------------------------------------------
// Footer
// ---------------------------------------------------------------------------

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 {
        return;
    }
    let theme = &app.theme;
    frame.render_widget(
        Block::default().style(Style::default().bg(theme.panel)),
        area,
    );

    let mut hints: Vec<Span<'static>> = Vec::new();
    let key = Style::default()
        .fg(theme.accent)
        .add_modifier(Modifier::BOLD);
    let text = Style::default().fg(theme.muted);

    let items: Vec<(&str, &str)> = if app.running() {
        vec![
            ("x", "cancel"),
            ("g/G", "log"),
            ("f", "follow"),
            ("?", "help"),
        ]
    } else if app.modal.is_some() {
        vec![("esc", "close"), ("?", "help")]
    } else {
        let mut items = vec![
            ("↵", "run"),
            ("d", "dry"),
            ("/", "filter"),
            ("tab", "pane"),
            ("r", "reload"),
            ("e", "edit"),
            ("?", "help"),
            ("q", "quit"),
        ];
        if app.pipelines.is_empty() {
            items.insert(1, ("i", "init"));
        }
        items
    };

    for (index, (name, label)) in items.iter().enumerate() {
        if index > 0 {
            hints.push(Span::styled("  ", text));
        }
        hints.push(Span::styled(*name, key));
        hints.push(Span::styled(format!(" {label}"), text));
    }

    let left = Rect::new(area.x + 1, area.y, area.width.saturating_sub(2), 1);
    let right_text = if let Some(run) = &app.run {
        match run.status {
            Some(RunStatus::Ok) if run.dry_run => "planned",
            Some(RunStatus::Ok) => "shipped",
            Some(RunStatus::Failed) => "failed",
            Some(RunStatus::Cancelled) => "cancelled",
            None if run.dry_run => "dry run",
            None => "running",
        }
        .to_string()
    } else {
        format!("{} pipelines ", app.pipelines.len())
    };
    let right = Rect::new(
        area.x + area.width.saturating_sub(right_text.width() as u16 + 2),
        area.y,
        right_text.width() as u16 + 1,
        1,
    );
    let left_width = area.width.saturating_sub(right.width + 2);
    frame.render_widget(
        Paragraph::new(Line::from(hints)),
        Rect::new(left.x, left.y, left_width, 1),
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            right_text,
            Style::default().fg(theme.faint),
        )))
        .alignment(Alignment::Right),
        right,
    );
}

fn draw_toast(frame: &mut Frame, app: &App, footer: Rect) {
    let Some(toast) = &app.toast else {
        return;
    };
    let theme = &app.theme;
    let width = (toast.message.width() as u16 + 4).min(footer.width.saturating_sub(4));
    if width < 8 {
        return;
    }
    let area = Rect::new(
        footer.x + footer.width.saturating_sub(width + 2),
        footer.y.saturating_sub(2),
        width,
        1,
    );
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" {} ", if toast.ok { "✓" } else { "!" }),
                Style::default()
                    .fg(theme.panel)
                    .bg(if toast.ok {
                        theme.success
                    } else {
                        theme.danger
                    })
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                truncate(&toast.message, width.saturating_sub(4) as usize),
                Style::default()
                    .fg(theme.fg)
                    .bg(theme.selection)
                    .add_modifier(Modifier::BOLD),
            ),
        ])),
        area,
    );
}

// ---------------------------------------------------------------------------
// Modals
// ---------------------------------------------------------------------------

fn draw_modal(frame: &mut Frame, app: &App, modal: &Modal, area: Rect) {
    match modal {
        Modal::Plan {
            pipeline,
            rows,
            dry_run,
            scroll,
        } => draw_plan(frame, app, area, pipeline, rows, *dry_run, *scroll),
        Modal::Confirm { prompt } => draw_confirm(frame, app, area, prompt),
        Modal::Help => draw_help(frame, app, area),
        Modal::Info => draw_info(frame, app, area),
        Modal::Quit => draw_quit(frame, app, area),
        Modal::Error { title, message } => draw_error(frame, app, area, title, message),
    }
}

fn backdrop(frame: &mut Frame, area: Rect) {
    frame.render_widget(Clear, area);
}

fn modal_shell(
    frame: &mut Frame,
    theme: &Theme,
    area: Rect,
    width: u16,
    height: u16,
    title: Vec<Span<'static>>,
    accent: ratatui::style::Color,
) -> Rect {
    let rect = widgets::centered(area, width, height);
    backdrop(frame, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(accent))
        .style(Style::default().bg(theme.panel))
        .title(Line::from({
            let mut spans = vec![Span::styled(" ", Style::default())];
            spans.extend(title);
            spans.push(Span::styled(" ", Style::default()));
            spans
        }));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    inner
}

fn draw_plan(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    pipeline: &str,
    rows: &[crate::plan::PlanRow],
    dry_run: bool,
    scroll: usize,
) {
    let theme = &app.theme;
    let height = (rows.len() as u16 + 8).clamp(10, area.height.saturating_sub(2));
    let width = area.width.saturating_sub(8).clamp(50, 96);
    let accent = if dry_run { theme.accent2 } else { theme.accent };

    let mut title = vec![
        Span::styled(
            "plan",
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · ", Style::default().fg(theme.faint)),
        Span::styled(pipeline.to_string(), Style::default().fg(theme.fg)),
        Span::styled("  ", Style::default()),
    ];
    title.push(if dry_run {
        widgets::pill("dry run", theme.selection, theme.accent2)
    } else {
        widgets::pill("live", theme.selection, theme.success)
    });

    let inner = modal_shell(frame, theme, area, width, height, title, accent);
    if inner.height < 3 {
        return;
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    let list_height = inner.height.saturating_sub(2) as usize;
    let offset = scroll.min(rows.len().saturating_sub(list_height.max(1)));

    for row in rows.iter().skip(offset).take(list_height) {
        let (marker, marker_color) = if row.gate.is_some() {
            ("–", theme.faint)
        } else if row.mutating {
            ("◆", theme.accent2)
        } else {
            ("◇", theme.muted)
        };
        let label_color = if row.gate.is_some() {
            theme.faint
        } else {
            theme.fg
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!(" {:>2} ", row.index),
                Style::default().fg(theme.faint),
            ),
            Span::styled(format!("{marker} "), Style::default().fg(marker_color)),
            Span::styled(
                pad(&row.label, 20),
                Style::default()
                    .fg(label_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                truncate(&row.detail, inner.width.saturating_sub(30) as usize),
                Style::default().fg(theme.muted),
            ),
        ]));

        if let Some(gate) = &row.gate {
            lines.push(Line::from(vec![
                Span::styled("      ", Style::default()),
                Span::styled(format!("gated: {gate}"), Style::default().fg(theme.warning)),
            ]));
        } else if let Some(command) = &row.command {
            lines.push(Line::from(vec![
                Span::styled("        $ ", Style::default().fg(theme.faint)),
                Span::styled(
                    truncate(command, inner.width.saturating_sub(12) as usize),
                    Style::default().fg(theme.faint),
                ),
            ]));
        }
    }

    frame.render_widget(Paragraph::new(lines), inner);

    let hint = Rect::new(
        inner.x,
        inner.y + inner.height.saturating_sub(1),
        inner.width,
        1,
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" enter", Style::default().fg(theme.accent)),
            Span::styled(" run   ", Style::default().fg(theme.muted)),
            Span::styled("d", Style::default().fg(theme.accent)),
            Span::styled(
                if dry_run {
                    " go live   "
                } else {
                    " dry run   "
                },
                Style::default().fg(theme.muted),
            ),
            Span::styled("j/k", Style::default().fg(theme.accent)),
            Span::styled(" scroll   ", Style::default().fg(theme.muted)),
            Span::styled("esc", Style::default().fg(theme.accent)),
            Span::styled(" cancel ", Style::default().fg(theme.muted)),
        ])),
        hint,
    );
}

fn draw_confirm(frame: &mut Frame, app: &App, area: Rect, prompt: &str) {
    let theme = &app.theme;
    let inner = modal_shell(
        frame,
        theme,
        area,
        62,
        9,
        vec![Span::styled(
            "confirm",
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        )],
        theme.warning,
    );

    let text: Vec<Line<'static>> = wrap(prompt, inner.width.saturating_sub(2) as usize)
        .into_iter()
        .take(inner.height.saturating_sub(2) as usize)
        .map(|line| Line::from(Span::styled(line, Style::default().fg(theme.fg))))
        .collect();
    frame.render_widget(Paragraph::new(text), inner);

    let hint = Rect::new(
        inner.x,
        inner.y + inner.height.saturating_sub(1),
        inner.width,
        1,
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("y", Style::default().fg(theme.success)),
            Span::styled(" confirm   ", Style::default().fg(theme.muted)),
            Span::styled("n", Style::default().fg(theme.danger)),
            Span::styled(" cancel", Style::default().fg(theme.muted)),
        ]))
        .alignment(Alignment::Center),
        hint,
    );
}

fn draw_help(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let inner = modal_shell(
        frame,
        theme,
        area,
        76,
        22,
        vec![Span::styled(
            "keys",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )],
        theme.accent,
    );

    let key = Style::default().fg(theme.accent);
    let label = Style::default().fg(theme.muted);
    let section = Style::default()
        .fg(theme.accent2)
        .add_modifier(Modifier::BOLD);

    let rows: Vec<(&str, Vec<(&str, &str)>)> = vec![
        (
            "navigate",
            vec![
                ("j / k", "move selection"),
                ("[ / ]", "previous / next pipeline"),
                ("tab", "switch pane"),
                ("/", "filter pipelines"),
            ],
        ),
        (
            "run",
            vec![
                ("enter", "review the plan, then run"),
                ("d", "review in dry-run mode"),
                ("x", "cancel the current run"),
                ("g / G", "oldest / newest log line"),
                ("f", "toggle log follow"),
            ],
        ),
        (
            "workspace",
            vec![
                ("r", "reload tug.toml"),
                ("e", "edit config in $EDITOR"),
                ("i", "write a starter config"),
                ("c", "repository context"),
                ("q", "quit"),
                ("?", "close this"),
            ],
        ),
    ];

    let mut lines: Vec<Line<'static>> = Vec::new();
    for (heading, items) in rows {
        lines.push(Line::from(Span::styled(heading, section)));
        for (keys, description) in items {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<12}", keys), key),
                Span::styled(description, label),
            ]));
        }
        lines.push(Line::from(""));
    }
    lines.truncate(inner.height as usize);
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_info(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let inner = modal_shell(
        frame,
        theme,
        area,
        78,
        16,
        vec![Span::styled(
            "context",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )],
        theme.accent,
    );

    let key = Style::default().fg(theme.faint);
    let value = Style::default().fg(theme.fg);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut row = |name: &str, text: String| {
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<12}", name), key),
            Span::styled(
                truncate(&text, inner.width.saturating_sub(16) as usize),
                value,
            ),
        ]));
    };

    row("root", app.root.display().to_string());
    if app.loaded.sources.is_empty() {
        row("config", "none found — run `tug init`".to_string());
    } else {
        let sources = app
            .loaded
            .sources
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        row("config", sources);
    }
    row(
        "profile",
        app.profile.clone().unwrap_or_else(|| "(none)".into()),
    );
    row(
        "branch",
        format!(
            "{} · {}",
            app.facts
                .branch
                .clone()
                .unwrap_or_else(|| "(detached)".into()),
            if app.facts.dirty { "dirty" } else { "clean" }
        ),
    );
    row(
        "repo",
        app.facts
            .repo
            .clone()
            .unwrap_or_else(|| app.facts.display_name()),
    );
    row(
        "remote",
        app.facts.remote.clone().unwrap_or_else(|| "(none)".into()),
    );
    row(
        "upstream",
        app.facts
            .upstream
            .clone()
            .unwrap_or_else(|| "(none)".into()),
    );
    if let Some(tag) = &app.facts.latest_tag {
        row("latest tag", tag.clone());
    }
    row("theme", app.theme.name.clone());
    row(
        "engine",
        format!(
            "tug {} · {} pipeline(s) · {} problem(s)",
            env!("CARGO_PKG_VERSION"),
            app.pipelines.len(),
            app.loaded.problems.len()
        ),
    );

    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_quit(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let inner = modal_shell(
        frame,
        theme,
        area,
        56,
        8,
        vec![Span::styled(
            "quit",
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        )],
        theme.warning,
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "a run is still in progress",
                Style::default().fg(theme.fg),
            )),
            Line::from(Span::styled(
                "quit anyway and cancel it?",
                Style::default().fg(theme.muted),
            )),
        ])
        .alignment(Alignment::Center),
        inner,
    );
    let hint = Rect::new(
        inner.x,
        inner.y + inner.height.saturating_sub(1),
        inner.width,
        1,
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("y", Style::default().fg(theme.danger)),
            Span::styled(" quit   ", Style::default().fg(theme.muted)),
            Span::styled("n", Style::default().fg(theme.success)),
            Span::styled(" stay", Style::default().fg(theme.muted)),
        ]))
        .alignment(Alignment::Center),
        hint,
    );
}

fn draw_error(frame: &mut Frame, app: &App, area: Rect, title: &str, message: &str) {
    let theme = &app.theme;
    let inner = modal_shell(
        frame,
        theme,
        area,
        82,
        18,
        vec![Span::styled(
            format!("{title} · error"),
            Style::default()
                .fg(theme.danger)
                .add_modifier(Modifier::BOLD),
        )],
        theme.danger,
    );

    let mut lines: Vec<Line<'static>> = Vec::new();
    for line in message.lines() {
        let mut first = true;
        for wrapped in wrap(line, inner.width.saturating_sub(6) as usize) {
            let marker = if first { "  ✗ " } else { "    " };
            lines.push(Line::from(vec![
                Span::styled(marker, Style::default().fg(theme.danger)),
                Span::styled(wrapped, Style::default().fg(theme.fg)),
            ]));
            first = false;
        }
    }
    lines.truncate(inner.height.saturating_sub(1) as usize);
    frame.render_widget(Paragraph::new(lines), inner);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "fix tug.toml, then press r to reload",
            Style::default().fg(theme.muted),
        )))
        .alignment(Alignment::Center),
        Rect::new(
            inner.x,
            inner.y + inner.height.saturating_sub(1),
            inner.width,
            1,
        ),
    );
}

// ---------------------------------------------------------------------------
// Tiny terminals
// ---------------------------------------------------------------------------

fn draw_tiny(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    frame.render_widget(
        Block::default().style(Style::default().bg(theme.panel)),
        area,
    );
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("tug ", Style::default().fg(theme.accent)),
        Span::styled(
            app.facts.display_name(),
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
        ),
    ]));

    let visible = app.visible();
    let budget = area.height.saturating_sub(3) as usize;
    let offset = if app.selected >= budget && budget > 0 {
        app.selected + 1 - budget
    } else {
        0
    };
    for (position, index) in visible.iter().enumerate().skip(offset).take(budget) {
        let Some(row) = app.pipelines.get(*index) else {
            continue;
        };
        let selected = position == app.selected;
        lines.push(Line::from(vec![
            Span::styled(
                if selected { "▌ " } else { "  " },
                Style::default().fg(theme.accent),
            ),
            Span::styled(
                truncate(row.name.as_str(), area.width.saturating_sub(4) as usize),
                Style::default()
                    .fg(if selected { theme.fg } else { theme.muted })
                    .add_modifier(if selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
        ]));
    }

    frame.render_widget(Paragraph::new(lines), area);
    if area.height >= 2 {
        frame.render_widget(
            Paragraph::new(Span::styled(
                " ↵ run · d dry · q quit",
                Style::default().fg(theme.faint),
            )),
            Rect::new(area.x, area.y + area.height - 1, area.width, 1),
        );
    }

    if let Some(modal) = &app.modal {
        frame.render_widget(Clear, area);
        frame.render_widget(
            Block::default().style(Style::default().bg(theme.panel)),
            area,
        );
        draw_modal(frame, app, modal, area);
    }
}

// ---------------------------------------------------------------------------
// Shared chrome
// ---------------------------------------------------------------------------

/// A short, pure description of what a step will do, for the idle preview.
fn step_hint(step: &crate::model::Step, ctx: &crate::model::Ctx) -> String {
    use crate::model::Action::*;

    let render =
        |text: &str| crate::template::render(text, ctx, false).unwrap_or_else(|_| text.to_string());

    if step.label.is_some() {
        return step.action.kind().to_string();
    }
    match &step.action {
        Stage { paths, all, update } => {
            if *update {
                "tracked changes".into()
            } else if *all || paths.is_empty() {
                "all changes".into()
            } else {
                paths.join(" ")
            }
        }
        Unstage { paths, all } => {
            if *all || paths.is_empty() {
                "everything".into()
            } else {
                paths.join(" ")
            }
        }
        Commit { message, .. } => truncate(&render(message), 60),
        Push {
            remote,
            branch,
            force,
            ..
        } => {
            let mut hint = String::new();
            if let Some(remote) = remote {
                hint.push_str(remote);
                hint.push('/');
            }
            hint.push_str(
                &branch
                    .as_deref()
                    .map(render)
                    .unwrap_or_else(|| "upstream".to_string()),
            );
            if *force != crate::model::Force::No {
                hint.push_str(" (forced)");
            }
            hint
        }
        Fetch { remote, prune, .. } => format!(
            "{}{}",
            remote.as_deref().unwrap_or("remote"),
            if *prune { " --prune" } else { "" }
        ),
        Checkout { target, create } => {
            format!("{}{target}", if *create { "new branch " } else { "" })
        }
        Tag { name, .. } => name.clone(),
        Pr {
            title, base, draft, ..
        } => {
            let mut hint = title.clone().unwrap_or_else(|| "pull request".into());
            if let Some(base) = base {
                hint.push_str(&format!(" → {base}"));
            }
            if *draft {
                hint.push_str(" (draft)");
            }
            hint
        }
        Release {
            tag,
            draft,
            prerelease,
            ..
        } => format!(
            "{tag}{}{}",
            if *draft { " (draft)" } else { "" },
            if *prerelease { " (pre)" } else { "" }
        ),
        Watch { interval_secs, .. } => interval_secs
            .map(|interval| format!("CI checks every {interval}s"))
            .unwrap_or_else(|| "CI checks".into()),
        Shell { run, .. } => render(run),
        Set { key, value } => format!("{key} = {}", render(value)),
        Bump { part, .. } => format!("bump {part:?}"),
        Confirm { prompt } => render(prompt),
        Sleep { ms } => format!("wait {ms} ms"),
    }
}

/// Clip styled spans to `width` display columns, ellipsising the last one.
fn fit_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    for span in spans {
        let span_width = span.content.width();
        if used + span_width <= width {
            used += span_width;
            out.push(span);
            continue;
        }
        let room = width.saturating_sub(used);
        if room > 0 {
            out.push(Span::styled(truncate(&span.content, room), span.style));
        }
        break;
    }
    out
}

fn panel(
    theme: &Theme,
    area: Rect,
    title: &str,
    border: ratatui::style::Color,
    right: &str,
    accent: ratatui::style::Color,
) -> Block<'static> {
    let inner_width = area.width.saturating_sub(2) as usize;
    let left = format!(" {title} ");
    let right = format!(" {right} ");
    let gap = inner_width.saturating_sub(left.width() + right.width());
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(theme.panel))
        .title(Line::from(vec![
            Span::styled(
                format!(" {title} "),
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ".repeat(gap), Style::default()),
            Span::styled(format!("{right} "), Style::default().fg(theme.faint)),
        ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::config::LoadedConfig;
    use crate::model::{Action, Config, Ctx, Density, GitFacts, Pipeline, Step};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::mpsc;

    fn app_with(pipelines: usize) -> App {
        let mut config = Config::default();
        for index in 0..pipelines {
            config.pipelines.insert(
                format!("pipe{index}"),
                Pipeline {
                    description: Some(format!("pipeline number {index}")),
                    steps: vec![
                        Step {
                            action: crate::model::Action::Stage {
                                paths: Vec::new(),
                                all: true,
                                update: false,
                            },
                            ..step_defaults()
                        },
                        Step {
                            action: crate::model::Action::Push {
                                remote: None,
                                branch: None,
                                set_upstream: true,
                                force: crate::model::Force::No,
                                tags: false,
                            },
                            ..step_defaults()
                        },
                    ],
                    ..Pipeline::default()
                },
            );
        }
        let loaded = LoadedConfig {
            config,
            ..LoadedConfig::empty()
        };
        let (tx, _rx) = mpsc::channel();
        App::new(
            loaded,
            PathBuf::from("/tmp/tug"),
            GitFacts {
                root: PathBuf::from("/tmp/tug"),
                in_repo: true,
                branch: Some("main".into()),
                ..GitFacts::default()
            },
            None,
            None,
            tx,
        )
    }

    fn step_defaults() -> Step {
        Step {
            action: crate::model::Action::Sleep { ms: 1 },
            label: None,
            when: None,
            env: Default::default(),
            cwd: None,
            timeout: None,
            retries: None,
            continue_on_error: false,
            confirm: false,
            silent: false,
            capture: None,
        }
    }

    fn render(app: &mut App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        app.on_resize(width, height);
        terminal.draw(|frame| draw(frame, app)).expect("draw");
        let buffer = terminal.backend().buffer().clone();
        buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    }

    #[test]
    fn log_height_matches_layout() {
        let app = app_with(2);
        for height in [18u16, 20, 24, 32, 48, 80] {
            let area = Rect::new(0, 0, 120, height);
            let areas = layout(area, &app);
            let expected = areas.log.height.saturating_sub(1) as usize;
            assert_eq!(
                log_inner_height(area, true),
                expected,
                "height {height} layout {}",
                areas.log.height
            );
        }
    }

    #[test]
    fn log_is_hidden_on_short_terminals() {
        assert_eq!(log_inner_height(Rect::new(0, 0, 120, 17), true), 0);
        assert_eq!(log_inner_height(Rect::new(0, 0, 120, 40), false), 0);
    }

    #[test]
    fn renders_the_selected_pipeline() {
        let mut app = app_with(3);
        let screen = render(&mut app, 120, 40);
        assert!(screen.contains("tug"));
        assert!(screen.contains("pipe0"));
        assert!(screen.contains("pipeline number 0"));
        assert!(screen.contains("pipelines"));
    }

    #[test]
    fn renders_the_plan_modal() {
        let mut app = app_with(1);
        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));
        let screen = render(&mut app, 120, 40);
        assert!(screen.contains("plan"));
        assert!(screen.contains("dry"));
        assert!(screen.contains("enter"));
    }

    #[test]
    fn renders_a_tiny_terminal_without_panicking() {
        let mut app = app_with(2);
        let _ = render(&mut app, 40, 10);
        let _ = render(&mut app, 60, 13);
        let _ = render(&mut app, 80, 20);
        let _ = render(&mut app, 200, 60);
    }

    #[test]
    fn idle_hints_render_template_fallbacks() {
        let step = Step {
            action: Action::Commit {
                message: "{{ summary | chore: update }}".into(),
                all: false,
                allow_empty: false,
                amend: false,
                signoff: false,
                no_verify: false,
            },
            ..step_defaults()
        };
        let ctx = Ctx::new(GitFacts::default());
        assert_eq!(step_hint(&step, &ctx), "chore: update");
    }

    #[test]
    fn comfortable_density_keeps_descriptions() {
        let mut app = app_with(8);
        app.loaded.config.ui.density = Density::Comfortable;
        let screen = render(&mut app, 120, 28);
        assert!(screen.contains("pipeline number 0"));
    }

    #[test]
    fn sidebar_tracks_selection() {
        let mut app = app_with(3);
        app.cycle_pipeline(1);
        assert_eq!(app.selected, 1);
        let screen = render(&mut app, 120, 40);
        assert!(screen.contains("pipe1"));
        let _ = Arc::new(());
    }
}
