//! Application state and the state transitions behind every key.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::config::{self, LoadedConfig, Severity};
use crate::engine::{Control, RunSpec};
use crate::git;
use crate::model::{Config, Ctx, EngineEvent, GitFacts, NoteLevel, RunStatus, StepStatus, Stream};
use crate::plan::{self, PlanRow};
use crate::tui::theme::Theme;
use crate::tui::{Event, render};

const MAX_LOG_LINES: usize = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Pipelines,
    Log,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suspend {
    /// Hand the terminal to an interactive step.
    Interactive,
    /// Hand the terminal to `$EDITOR`.
    Editor,
}

#[derive(Debug, Clone)]
pub struct PipelineRow {
    pub name: String,
    pub description: String,
    pub steps: usize,
    pub gate: Option<String>,
    pub status: Option<StepStatus>,
}

#[derive(Debug, Clone)]
pub struct StepRow {
    pub label: String,
    pub detail: String,
    pub command: Option<String>,
    pub status: StepStatus,
    pub duration_ms: u64,
    pub summary: String,
}

pub struct RunState {
    pub pipeline: String,
    pub dry_run: bool,
    pub steps: Vec<StepRow>,
    pub status: Option<RunStatus>,
    pub current: Option<usize>,
    pub started: Instant,
    pub elapsed_ms: u64,
    pub control: Arc<Control>,
    pub error: Option<String>,
}

impl RunState {
    pub fn is_running(&self) -> bool {
        self.status.is_none()
    }

    pub fn done(&self) -> usize {
        self.steps
            .iter()
            .filter(|step| step.status.is_terminal())
            .count()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogKind {
    Command,
    Out,
    Err,
    Note,
    Warn,
    Error,
    Ok,
    Fail,
    Cancel,
    System,
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub kind: LogKind,
    pub text: String,
}

#[derive(Debug, Default)]
pub struct LogView {
    pub lines: VecDeque<LogLine>,
    /// Lines scrolled up from the newest line.
    pub scroll: usize,
    pub follow: bool,
}

impl LogView {
    pub fn clear(&mut self) {
        self.lines.clear();
        self.scroll = 0;
        self.follow = true;
    }

    pub fn push(&mut self, kind: LogKind, text: impl Into<String>) {
        if self.lines.len() >= MAX_LOG_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(LogLine {
            kind,
            text: text.into(),
        });
        if self.follow {
            self.scroll = 0;
        }
    }

    pub fn max_scroll(&self, viewport: usize) -> usize {
        self.lines.len().saturating_sub(viewport.max(1))
    }
}

#[derive(Debug, Clone)]
pub enum Modal {
    Plan {
        pipeline: String,
        rows: Vec<PlanRow>,
        dry_run: bool,
        scroll: usize,
    },
    Confirm {
        prompt: String,
    },
    Help,
    Info,
    Quit,
    Error {
        title: String,
        message: String,
    },
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub message: String,
    pub ok: bool,
    pub at: Instant,
}

pub struct App {
    pub loaded: LoadedConfig,
    pub root: PathBuf,
    pub facts: GitFacts,
    pub profile: Option<String>,
    pub pipelines: Vec<PipelineRow>,
    pub selected: usize,
    pub focus: Focus,
    pub filter: String,
    pub filtering: bool,
    pub modal: Option<Modal>,
    pub run: Option<RunState>,
    pub log: LogView,
    pub tick: u64,
    pub theme: Theme,
    pub should_quit: bool,
    pub suspend: Option<Suspend>,
    pub show_log: bool,
    pub toast: Option<Toast>,
    pub size: Rect,
    pub tx: Sender<Event>,
    /// The `--config` argument, kept so reload uses the same layers.
    pub config_arg: Option<PathBuf>,
    /// `--set` overrides from the command line, highest precedence.
    pub overrides: BTreeMap<String, String>,
    /// `--dry-run` turns every plan into a dry run by default.
    pub default_dry_run: bool,
}

/// The blocking-error modal for a config that cannot run, if it has errors.
fn config_error_modal(loaded: &LoadedConfig) -> Option<Modal> {
    if !loaded.has_errors() {
        return None;
    }
    Some(Modal::Error {
        title: "configuration".into(),
        message: loaded
            .problems
            .iter()
            .filter(|problem| problem.severity == Severity::Error)
            .map(|problem| {
                if problem.path.is_empty() {
                    problem.message.clone()
                } else {
                    format!("{}: {}", problem.path, problem.message)
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
    })
}

impl App {
    pub fn new(
        loaded: LoadedConfig,
        root: PathBuf,
        facts: GitFacts,
        initial: Option<String>,
        config_arg: Option<PathBuf>,
        tx: Sender<Event>,
    ) -> Self {
        let theme = Theme::load(&loaded.config.ui.theme);
        let follow = loaded.config.ui.follow_log.unwrap_or(true);
        let profile = None;

        let mut app = Self {
            loaded,
            root,
            facts,
            profile,
            pipelines: Vec::new(),
            selected: 0,
            focus: Focus::Pipelines,
            filter: String::new(),
            filtering: false,
            modal: None,
            run: None,
            log: LogView {
                follow,
                ..LogView::default()
            },
            tick: 0,
            theme,
            should_quit: false,
            suspend: None,
            show_log: true,
            toast: None,
            size: Rect::new(0, 0, 120, 40),
            tx,
            config_arg,
            overrides: BTreeMap::new(),
            default_dry_run: false,
        };

        app.rebuild_pipelines();
        if let Some(name) = initial {
            if let Some(index) = app.pipelines.iter().position(|row| row.name == name) {
                app.selected = index;
            } else {
                app.toast = Some(Toast {
                    message: format!("no pipeline named `{name}`"),
                    ok: false,
                    at: Instant::now(),
                });
            }
        }
        app.modal = config_error_modal(&app.loaded);
        app
    }

    // -----------------------------------------------------------------
    // Derived state
    // -----------------------------------------------------------------

    pub fn ctx(&self, pipeline: Option<&str>, dry_run: bool) -> Ctx {
        config::context_with(
            &self.loaded,
            &self.facts,
            self.profile.as_deref(),
            dry_run,
            pipeline,
            &self.overrides,
        )
    }

    /// Apply the command-line profile and rebuild gated state.
    pub fn set_profile(&mut self, profile: Option<String>) {
        self.profile = profile;
        self.rebuild_pipelines();
    }

    pub fn running(&self) -> bool {
        self.run.as_ref().is_some_and(RunState::is_running)
    }

    pub fn active_run(&self) -> Option<&RunState> {
        self.run.as_ref()
    }

    pub fn visible(&self) -> Vec<usize> {
        let filter = self.filter.to_lowercase();
        self.pipelines
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                filter.is_empty()
                    || row.name.to_lowercase().contains(&filter)
                    || row.description.to_lowercase().contains(&filter)
            })
            .map(|(index, _)| index)
            .collect()
    }

    pub fn selected_pipeline(&self) -> Option<&PipelineRow> {
        let visible = self.visible();
        visible
            .get(self.selected)
            .and_then(|index| self.pipelines.get(*index))
    }

    pub fn log_viewport(&self) -> usize {
        render::log_inner_height(self.size, self.show_log)
    }

    // -----------------------------------------------------------------
    // Navigation
    // -----------------------------------------------------------------

    fn clamp_selection(&mut self) {
        let count = self.visible().len();
        if count == 0 {
            self.selected = 0;
        } else if self.selected >= count {
            self.selected = count - 1;
        }
    }

    pub fn cycle_pipeline(&mut self, delta: isize) {
        let count = self.visible().len();
        if count == 0 {
            return;
        }
        let current = self.selected as isize;
        self.selected = ((current + delta).rem_euclid(count as isize)) as usize;
    }

    fn move_up(&mut self) {
        if self.focus == Focus::Log {
            self.log.follow = false;
            let max = self.log.max_scroll(self.log_viewport());
            self.log.scroll = (self.log.scroll + 1).min(max);
        } else {
            self.cycle_pipeline(-1);
        }
    }

    fn move_down(&mut self) {
        if self.focus == Focus::Log {
            self.log.scroll = self.log.scroll.saturating_sub(1);
            if self.log.scroll == 0 {
                self.log.follow = true;
            }
        } else {
            self.cycle_pipeline(1);
        }
    }

    fn page_up(&mut self) {
        self.log.follow = false;
        let max = self.log.max_scroll(self.log_viewport());
        self.log.scroll = (self.log.scroll + self.log_viewport()).min(max);
    }

    fn page_down(&mut self) {
        self.log.scroll = self.log.scroll.saturating_sub(self.log_viewport());
        if self.log.scroll == 0 {
            self.log.follow = true;
        }
    }

    fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Pipelines => Focus::Log,
            Focus::Log => Focus::Pipelines,
        };
    }

    // -----------------------------------------------------------------
    // Config reload
    // -----------------------------------------------------------------

    pub fn reload(&mut self) {
        match config::load(self.config_arg.as_deref(), &self.root) {
            Ok(loaded) => {
                self.loaded = loaded;
                self.theme = Theme::load(&self.loaded.config.ui.theme);
                match git::facts(&self.root, &self.loaded.config.defaults.remote) {
                    Ok(facts) => self.facts = facts,
                    Err(err) => self.notify(format!("git facts: {err}"), false),
                }
                let keep = self.selected_pipeline_name();
                self.rebuild_pipelines();
                if let Some(name) = keep {
                    if let Some(index) = self.pipelines.iter().position(|row| row.name == name) {
                        self.selected = index;
                    }
                }
                self.clamp_selection();
                let count = self.loaded.problems.len();
                let message = if count == 0 {
                    "reloaded".to_string()
                } else {
                    format!("reloaded with {count} problem(s)")
                };
                self.notify(message, !self.loaded.has_errors());
                self.modal = config_error_modal(&self.loaded);
            }
            Err(err) => {
                self.notify("reload failed", false);
                self.modal = Some(Modal::Error {
                    title: "reload".into(),
                    message: format!("{err:#}"),
                });
            }
        }
    }

    fn rebuild_pipelines(&mut self) {
        let previous: Vec<(String, Option<StepStatus>)> = self
            .pipelines
            .iter()
            .map(|row| (row.name.clone(), row.status))
            .collect();

        let ordered = crate::engine::run::ordered(&self.loaded.config);
        let contexts: Vec<Ctx> = ordered
            .iter()
            .map(|(name, _)| self.ctx(Some(name), false))
            .collect();

        self.pipelines = ordered
            .into_iter()
            .zip(contexts)
            .map(|((name, pipeline), ctx)| {
                let gate = pipeline.when.as_ref().and_then(|when| {
                    if when.is_empty() {
                        return None;
                    }
                    let verdict = crate::condition::evaluate(when, &ctx, &mut |command| {
                        git::shell_ok(command, &self.root)
                    });
                    (!verdict.passed).then_some(verdict.reason)
                });
                let status = previous
                    .iter()
                    .find(|(previous_name, _)| previous_name == name)
                    .and_then(|(_, status)| *status);
                PipelineRow {
                    name: name.clone(),
                    description: pipeline.description.clone().unwrap_or_default(),
                    steps: pipeline.steps.len(),
                    gate,
                    status,
                }
            })
            .collect();
        self.clamp_selection();
    }

    fn selected_pipeline_name(&self) -> Option<String> {
        self.selected_pipeline().map(|row| row.name.clone())
    }

    // -----------------------------------------------------------------
    // Running
    // -----------------------------------------------------------------

    fn open_plan(&mut self, dry_run: bool) {
        let dry_run = dry_run || self.default_dry_run;
        if self.running() {
            self.notify("a run is already in progress", false);
            return;
        }
        if self.loaded.has_errors() {
            self.modal = config_error_modal(&self.loaded);
            return;
        }
        let Some(row) = self.selected_pipeline() else {
            self.notify("no pipeline selected", false);
            return;
        };
        let name = row.name.clone();
        if let Some(reason) = row.gate.clone() {
            self.notify(format!("`{name}` is gated: {reason}"), false);
            return;
        }
        let ctx = self.ctx(Some(&name), dry_run);
        match plan::build(&self.loaded.config, &ctx, &name) {
            Ok(rows) if rows.is_empty() => self.notify("pipeline has no steps", false),
            Ok(rows) => {
                self.modal = Some(Modal::Plan {
                    pipeline: name,
                    rows,
                    dry_run,
                    scroll: 0,
                });
            }
            Err(err) => self.notify(format!("plan failed: {err}"), false),
        }
    }

    fn start_run(&mut self, pipeline: String, dry_run: bool) {
        if self.running() {
            return;
        }
        let Some(definition) = self.loaded.config.pipelines.get(&pipeline) else {
            self.notify(format!("unknown pipeline `{pipeline}`"), false);
            return;
        };

        let ctx = self.ctx(Some(&pipeline), dry_run);
        let planned = plan::build(&self.loaded.config, &ctx, &pipeline).unwrap_or_default();

        let steps = definition
            .steps
            .iter()
            .enumerate()
            .map(|(index, step)| StepRow {
                label: step.title(),
                detail: planned
                    .get(index)
                    .map(|row| row.detail.clone())
                    .unwrap_or_else(|| step.action.kind().to_string()),
                command: planned.get(index).and_then(|row| row.command.clone()),
                status: StepStatus::Pending,
                duration_ms: 0,
                summary: String::new(),
            })
            .collect::<Vec<_>>();

        let total = steps.len();
        let (control, rx) = Control::new();
        let (engine_tx, engine_rx) = std::sync::mpsc::channel();
        let worker_control = Arc::clone(&control);

        // Reset the view *before* the engine starts, or its first events land in
        // a log that is about to be cleared.
        for row in &mut self.pipelines {
            row.status = None;
        }
        self.log.clear();
        self.show_log = true;
        self.focus = Focus::Pipelines;
        self.run = Some(RunState {
            pipeline: pipeline.clone(),
            dry_run,
            steps,
            status: None,
            current: None,
            started: Instant::now(),
            elapsed_ms: 0,
            control,
            error: None,
        });
        let mode = if dry_run { " (dry run)" } else { "" };
        let noun = if total == 1 { "step" } else { "steps" };
        self.log.push(
            LogKind::System,
            format!("{pipeline}{mode} · {total} {noun}"),
        );

        let forward = self.tx.clone();
        std::thread::spawn(move || {
            for event in engine_rx {
                if forward.send(Event::Engine(event)).is_err() {
                    break;
                }
            }
        });

        let spec = RunSpec {
            pipeline: pipeline.clone(),
            profile: self.profile.clone(),
            overrides: self.overrides.clone(),
            dry_run,
            assume_yes: false,
        };
        let config = self.loaded.config.clone();
        let facts = self.facts.clone();
        let root = self.root.clone();
        std::thread::spawn(move || {
            crate::engine::run(&config, facts, &root, spec, engine_tx, worker_control, rx);
        });
    }

    fn cancel_run(&mut self) {
        if let Some(run) = &self.run {
            if run.is_running() {
                run.control.cancel();
                self.notify("cancelling…", true);
            }
        }
    }

    fn answer_confirm(&mut self, yes: bool) {
        if let Some(run) = &self.run {
            run.control.answer(yes);
        }
    }

    fn notify(&mut self, message: impl Into<String>, ok: bool) {
        self.toast = Some(Toast {
            message: message.into(),
            ok,
            at: Instant::now(),
        });
    }

    // -----------------------------------------------------------------
    // Events
    // -----------------------------------------------------------------

    pub fn on_tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        if let Some(run) = &mut self.run {
            if run.is_running() {
                run.elapsed_ms = run.started.elapsed().as_millis() as u64;
            }
        }
        if let Some(toast) = &self.toast {
            if toast.at.elapsed().as_secs() >= 4 {
                self.toast = None;
            }
        }
        let max = self.log.max_scroll(self.log_viewport());
        self.log.scroll = self.log.scroll.min(max);
    }

    pub fn on_resize(&mut self, width: u16, height: u16) {
        self.size = Rect::new(0, 0, width, height);
        let max = self.log.max_scroll(self.log_viewport());
        self.log.scroll = self.log.scroll.min(max);
    }

    pub fn on_engine(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::RunStarted { steps, .. } => {
                if let Some(run) = &mut self.run {
                    run.steps.truncate(steps);
                    for step in &mut run.steps {
                        step.status = StepStatus::Pending;
                        step.duration_ms = 0;
                        step.summary.clear();
                    }
                    run.status = None;
                    run.current = None;
                    run.started = Instant::now();
                    run.elapsed_ms = 0;
                }
            }
            EngineEvent::StepStarted {
                index,
                label,
                detail,
                command,
            } => {
                if let Some(run) = &mut self.run {
                    if let Some(step) = run.steps.get_mut(index) {
                        step.status = StepStatus::Running;
                        step.label = label;
                        step.detail = detail;
                        if command.is_some() {
                            step.command = command;
                        }
                    }
                    run.current = Some(index);
                    let _ = &run.steps;
                }
                if let Some(name) = self.pipeline_name_of_current() {
                    if let Some(row) = self.pipelines.iter_mut().find(|row| row.name == name) {
                        row.status = Some(StepStatus::Running);
                    }
                }
                self.log.follow = true;
            }
            EngineEvent::Output { stream, line, .. } => {
                let kind = match stream {
                    Stream::Stdout => {
                        if line.starts_with("$ ") {
                            LogKind::Command
                        } else {
                            LogKind::Out
                        }
                    }
                    Stream::Stderr => LogKind::Err,
                };
                self.log.push(kind, line);
            }
            EngineEvent::StepFinished {
                index,
                status,
                duration_ms,
                summary,
            } => {
                if let Some(run) = &mut self.run {
                    if let Some(step) = run.steps.get_mut(index) {
                        step.status = status;
                        step.duration_ms = duration_ms;
                        step.summary = summary;
                    }
                    if run.current == Some(index) {
                        run.current = None;
                    }
                }
            }
            EngineEvent::Note { level, message } => {
                let kind = match level {
                    NoteLevel::Info => LogKind::Note,
                    NoteLevel::Warn => LogKind::Warn,
                    NoteLevel::Error => LogKind::Error,
                };
                self.log.push(kind, message);
            }
            EngineEvent::Confirm { prompt, .. } => {
                self.modal = Some(Modal::Confirm { prompt });
            }
            EngineEvent::InteractiveStart { index } => {
                self.suspend = Some(Suspend::Interactive);
                self.log.follow = true;
                self.log.push(
                    LogKind::System,
                    format!(
                        "step {} is interactive — handing over the terminal",
                        index + 1
                    ),
                );
            }
            EngineEvent::InteractiveEnd { index } => {
                self.log.push(
                    LogKind::System,
                    format!("step {} finished — back in tug", index + 1),
                );
            }
            EngineEvent::RunFinished {
                status,
                duration_ms,
                error,
            } => {
                if let Some(run) = &mut self.run {
                    run.status = Some(status);
                    run.elapsed_ms = duration_ms;
                    run.current = None;
                    run.error = error.clone();
                    let name = run.pipeline.clone();
                    let dry_run = run.dry_run;
                    if let Some(row) = self.pipelines.iter_mut().find(|row| row.name == name) {
                        row.status = Some(match status {
                            RunStatus::Ok if dry_run => StepStatus::Planned,
                            RunStatus::Ok => StepStatus::Ok,
                            RunStatus::Failed => StepStatus::Failed,
                            RunStatus::Cancelled => StepStatus::Cancelled,
                        });
                    }
                }
                let dry_run = self.run.as_ref().is_some_and(|run| run.dry_run);
                let kind = match status {
                    RunStatus::Ok if dry_run => LogKind::System,
                    RunStatus::Ok => LogKind::Ok,
                    RunStatus::Failed => LogKind::Fail,
                    RunStatus::Cancelled => LogKind::Cancel,
                };
                let seconds = duration_ms as f64 / 1000.0;
                let text = match status {
                    RunStatus::Ok if dry_run => format!("planned in {seconds:.1}s"),
                    RunStatus::Ok => format!("shipped in {seconds:.1}s"),
                    RunStatus::Failed => format!("failed after {seconds:.1}s"),
                    RunStatus::Cancelled => format!("cancelled after {seconds:.1}s"),
                };
                self.log.push(kind, text);
                if let Some(error) = error {
                    if status == RunStatus::Failed {
                        self.log.push(LogKind::Error, error);
                    }
                }
                self.refresh_facts();
            }
        }
    }

    fn pipeline_name_of_current(&self) -> Option<String> {
        self.run.as_ref().map(|run| run.pipeline.clone())
    }

    fn refresh_facts(&self) {
        let tx = self.tx.clone();
        let root = self.root.clone();
        let remote = self.loaded.config.defaults.remote.clone();
        std::thread::spawn(move || {
            if let Ok(facts) = git::facts(&root, &remote) {
                let _ = tx.send(Event::Facts(facts));
            }
        });
    }

    pub fn on_facts(&mut self, facts: GitFacts) {
        self.facts = facts;
        self.rebuild_pipelines();
    }

    pub fn on_config(&mut self, loaded: LoadedConfig, facts: GitFacts) {
        self.loaded = loaded;
        self.facts = facts;
        self.theme = Theme::load(&self.loaded.config.ui.theme);
        self.rebuild_pipelines();
    }

    // -----------------------------------------------------------------
    // Keys
    // -----------------------------------------------------------------

    pub fn on_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        if ctrl && key.code == KeyCode::Char('c') {
            if self.running() {
                self.cancel_run();
            } else {
                self.should_quit = true;
            }
            return;
        }

        if self.modal.is_some() {
            self.on_modal_key(key);
            return;
        }

        if self.filtering {
            self.on_filter_key(key);
            return;
        }

        match key.code {
            KeyCode::Char('q') => {
                if self.running() {
                    self.modal = Some(Modal::Quit);
                } else {
                    self.should_quit = true;
                }
            }
            KeyCode::Char('?') => self.modal = Some(Modal::Help),
            KeyCode::Char('c') => self.modal = Some(Modal::Info),
            KeyCode::Char('/') => {
                self.filtering = true;
                self.filter.clear();
                self.selected = 0;
            }
            KeyCode::Char('r') => self.reload(),
            KeyCode::Char('i') if self.loaded.config.pipelines.is_empty() => {
                match crate::config::init::write(&self.root, false) {
                    Ok(path) => {
                        self.reload();
                        self.notify(format!("wrote {}", path.display()), true);
                    }
                    Err(err) => self.notify(format!("{err}"), false),
                }
            }
            KeyCode::Char('e') => self.suspend = Some(Suspend::Editor),
            KeyCode::Char('x') => self.cancel_run(),
            KeyCode::Tab | KeyCode::BackTab => self.cycle_focus(),
            KeyCode::Char('j') | KeyCode::Down => self.move_down(),
            KeyCode::Char('k') | KeyCode::Up => self.move_up(),
            KeyCode::Char('J') => self.page_down(),
            KeyCode::Char('K') => self.page_up(),
            KeyCode::Char('g') => {
                self.log.follow = false;
                self.log.scroll = self.log.max_scroll(self.log_viewport());
            }
            KeyCode::Char('G') => {
                self.log.scroll = 0;
                self.log.follow = true;
            }
            KeyCode::Char('f') => {
                self.log.follow = !self.log.follow;
                if self.log.follow {
                    self.log.scroll = 0;
                }
            }
            KeyCode::Char('[') => self.cycle_pipeline(-1),
            KeyCode::Char(']') => self.cycle_pipeline(1),
            KeyCode::Enter => self.open_plan(false),
            KeyCode::Char('d') => self.open_plan(true),
            KeyCode::Esc => {
                self.filter.clear();
                self.selected = 0;
            }
            _ => {}
        }
    }

    fn on_filter_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => {
                self.filtering = false;
                if self.visible().is_empty() {
                    self.filter.clear();
                    self.notify("no pipelines match", false);
                }
            }
            KeyCode::Esc => {
                self.filtering = false;
                self.filter.clear();
                self.selected = 0;
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.selected = 0;
            }
            KeyCode::Char(ch) => {
                self.filter.push(ch);
                self.selected = 0;
            }
            _ => {}
        }
    }

    fn on_modal_key(&mut self, key: KeyEvent) {
        enum Pending {
            None,
            Run(String, bool),
            Answer(bool),
            Rebuild(String, bool),
        }

        let mut pending = Pending::None;
        let mut close = false;

        match self.modal.as_mut() {
            Some(Modal::Plan {
                pipeline,
                rows,
                dry_run,
                scroll,
            }) => match key.code {
                KeyCode::Enter => {
                    pending = Pending::Run(pipeline.clone(), *dry_run);
                    close = true;
                }
                KeyCode::Char('d') => {
                    *dry_run = !*dry_run;
                    pending = Pending::Rebuild(pipeline.clone(), *dry_run);
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    *scroll = (*scroll + 1).min(rows.len().saturating_sub(1));
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    *scroll = scroll.saturating_sub(1);
                }
                KeyCode::Char('g') => *scroll = 0,
                KeyCode::Char('G') => *scroll = rows.len().saturating_sub(1),
                KeyCode::Esc | KeyCode::Char('q') => close = true,
                _ => {}
            },
            Some(Modal::Confirm { .. }) => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    pending = Pending::Answer(true);
                    close = true;
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    pending = Pending::Answer(false);
                    close = true;
                }
                _ => {}
            },
            Some(Modal::Quit) => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    self.should_quit = true;
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => close = true,
                _ => {}
            },
            Some(Modal::Help) | Some(Modal::Info) | Some(Modal::Error { .. }) => {
                if matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') | KeyCode::Enter
                ) {
                    close = true;
                }
            }
            None => {}
        }

        if close {
            self.modal = None;
        }

        match pending {
            Pending::None => {}
            Pending::Run(pipeline, dry_run) => self.start_run(pipeline, dry_run),
            Pending::Answer(yes) => self.answer_confirm(yes),
            Pending::Rebuild(pipeline, dry_run) => {
                let ctx = self.ctx(Some(&pipeline), dry_run);
                match plan::build(&self.loaded.config, &ctx, &pipeline) {
                    Ok(rows) => {
                        let scroll = match &self.modal {
                            Some(Modal::Plan { scroll, .. }) => *scroll,
                            _ => 0,
                        };
                        self.modal = Some(Modal::Plan {
                            pipeline,
                            rows,
                            dry_run,
                            scroll,
                        });
                    }
                    Err(err) => self.notify(format!("plan failed: {err}"), false),
                }
            }
        }
    }
}

/// Header facts used by the renderer and tests.
pub fn pipeline_context(
    config: &Config,
    facts: &GitFacts,
    profile: Option<&str>,
    dry_run: bool,
) -> Ctx {
    let loaded = LoadedConfig {
        config: config.clone(),
        ..LoadedConfig::empty()
    };
    config::context(&loaded, facts, profile, dry_run)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Action, Config, Pipeline, Step, When};

    fn step(action: Action) -> Step {
        Step {
            action,
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

    fn app(pipeline: Pipeline, facts: GitFacts) -> App {
        let mut config = Config::default();
        config.pipelines.insert("main".into(), pipeline);
        let loaded = LoadedConfig {
            config,
            ..LoadedConfig::empty()
        };
        let (tx, _rx) = std::sync::mpsc::channel();
        App::new(
            loaded,
            std::path::PathBuf::from("/tmp/tug"),
            facts,
            None,
            None,
            tx,
        )
    }

    #[test]
    fn gated_pipelines_do_not_open_a_plan() {
        let mut app = app(
            Pipeline {
                when: Some(When {
                    branch: Some("release/*".into()),
                    ..When::default()
                }),
                steps: vec![step(Action::Sleep { ms: 1 })],
                ..Pipeline::default()
            },
            GitFacts {
                in_repo: true,
                branch: Some("main".into()),
                ..GitFacts::default()
            },
        );

        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));

        assert!(app.modal.is_none());
        let toast = app.toast.as_ref().expect("toast");
        assert!(toast.message.contains("gated"), "{:?}", toast.message);
    }

    #[test]
    fn a_dry_run_plan_carries_earlier_variables() {
        let mut app = app(
            Pipeline {
                steps: vec![
                    step(Action::Set {
                        key: "name".into(),
                        value: "world".into(),
                    }),
                    step(Action::Shell {
                        run: "echo {{ name }}".into(),
                        interactive: false,
                        print_command: true,
                    }),
                ],
                ..Pipeline::default()
            },
            GitFacts::default(),
        );

        app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));

        let Some(Modal::Plan { rows, .. }) = &app.modal else {
            panic!("expected a plan modal, got {:?}", app.modal.is_some());
        };
        assert_eq!(rows[1].detail, "echo world");
    }
}
