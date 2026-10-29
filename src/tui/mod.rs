//! Ratatui front end.
//!
//! The UI thread owns the terminal and [`App`] state. The engine runs on a
//! worker thread and reports progress through [`Event`]; when a step needs the
//! real terminal (an interactive shell, or `$EDITOR`) the UI steps out of the
//! alternate screen and waits for the step to finish before resuming.

pub mod app;
pub mod render;
pub mod theme;
pub mod widgets;

use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{Event as CrosstermEvent, KeyEvent, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::config::{self, LoadedConfig};
use crate::git;
use crate::model::{EngineEvent, GitFacts};
use crate::tui::app::{App, Suspend};

/// How long to wait for input before advancing animation and timers.
const TICK: Duration = Duration::from_millis(80);

/// Everything the event loop can receive.
#[derive(Debug)]
pub enum Event {
    Key(KeyEvent),
    Resize(u16, u16),
    Tick,
    Engine(EngineEvent),
    Facts(GitFacts),
    Config(Box<LoadedConfig>, GitFacts),
}

/// Run the interactive UI until the user quits.
pub fn run(
    config_arg: Option<PathBuf>,
    pipeline: Option<String>,
    profile: Option<String>,
    overrides: std::collections::BTreeMap<String, String>,
    dry_run: bool,
) -> Result<()> {
    let cwd = std::env::current_dir().context("reading current directory")?;
    let root = git::discover_root(&cwd).unwrap_or(cwd);

    let loaded = config::load(config_arg.as_deref(), &root)?;
    let facts = git::facts(&root, &loaded.config.defaults.remote).unwrap_or_else(|_| GitFacts {
        root: root.clone(),
        ..GitFacts::default()
    });

    let (tx, rx) = mpsc::channel();
    let mut app = App::new(loaded, root, facts, pipeline, config_arg, tx.clone());
    app.overrides = overrides;
    app.default_dry_run = dry_run;
    app.set_profile(profile);

    let mut guard = TerminalGuard::enter()?;
    if let Ok(size) = guard.terminal.size() {
        app.on_resize(size.width, size.height);
    }

    spawn_input(tx);

    let result = event_loop(&mut guard, &mut app, &rx);

    if app.running() {
        if let Some(run) = &app.run {
            run.control.cancel();
        }
    }

    guard.leave();
    result
}

fn event_loop(guard: &mut TerminalGuard, app: &mut App, rx: &mpsc::Receiver<Event>) -> Result<()> {
    loop {
        if let Some(suspend) = app.suspend.take() {
            match suspend {
                Suspend::Interactive => {
                    guard.suspend()?;
                    if let Some(run) = &app.run {
                        run.control.release_terminal();
                    }
                    wait_for_interactive_end(app, rx);
                    guard.resume()?;
                }
                Suspend::Editor => {
                    guard.suspend()?;
                    let edit = edit_config(app);
                    guard.resume()?;
                    if let Err(err) = edit {
                        eprintln!("tug: editor failed: {err:#}");
                    }
                    app.reload();
                }
            }
            continue;
        }

        guard.terminal.draw(|frame| render::draw(frame, app))?;

        if app.should_quit {
            return Ok(());
        }

        match rx.recv_timeout(TICK) {
            Ok(event) => dispatch(app, event),
            Err(RecvTimeoutError::Timeout) => app.on_tick(),
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }

        if app.should_quit {
            return Ok(());
        }
    }
}

fn dispatch(app: &mut App, event: Event) {
    match event {
        Event::Key(key) => {
            if key.kind == KeyEventKind::Press {
                app.on_key(key);
            }
        }
        Event::Resize(width, height) => app.on_resize(width, height),
        Event::Tick => app.on_tick(),
        Event::Engine(event) => app.on_engine(event),
        Event::Facts(facts) => app.on_facts(facts),
        Event::Config(loaded, facts) => app.on_config(*loaded, facts),
    }
}

/// Drain events until the interactive child hands the terminal back.
fn wait_for_interactive_end(app: &mut App, rx: &mpsc::Receiver<Event>) {
    loop {
        match rx.recv_timeout(TICK) {
            Ok(Event::Engine(event)) => {
                let finished = matches!(event, EngineEvent::InteractiveEnd { .. });
                app.on_engine(event);
                if finished {
                    return;
                }
            }
            Ok(Event::Resize(width, height)) => app.on_resize(width, height),
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => app.on_tick(),
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn edit_config(app: &App) -> Result<()> {
    let path = app
        .loaded
        .path
        .clone()
        .unwrap_or_else(|| app.root.join("tug.toml"));

    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&path, config::init::template(&app.root))
            .with_context(|| format!("writing {}", path.display()))?;
    }

    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());
    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("vi");

    let status = std::process::Command::new(program)
        .args(parts)
        .arg(&path)
        .status()
        .with_context(|| format!("launching {editor}"))?;

    if !status.success() {
        anyhow::bail!("{editor} exited with {:?}", status.code());
    }
    Ok(())
}

fn spawn_input(tx: Sender<Event>) {
    std::thread::spawn(move || {
        loop {
            match crossterm::event::poll(Duration::from_millis(200)) {
                Ok(true) => match crossterm::event::read() {
                    Ok(CrosstermEvent::Key(key)) => {
                        if tx.send(Event::Key(key)).is_err() {
                            return;
                        }
                    }
                    Ok(CrosstermEvent::Resize(width, height)) => {
                        if tx.send(Event::Resize(width, height)).is_err() {
                            return;
                        }
                    }
                    Ok(CrosstermEvent::Paste(_)) => {}
                    Ok(CrosstermEvent::Mouse(_)) => {}
                    Ok(CrosstermEvent::FocusGained | CrosstermEvent::FocusLost) => {}
                    Err(_) => return,
                },
                Ok(false) => {}
                Err(_) => return,
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Terminal lifecycle
// ---------------------------------------------------------------------------

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    suspended: bool,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        install_panic_hook();

        enable_raw_mode().context("enabling raw mode")?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen).context("entering the alternate screen")?;

        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).context("creating the terminal")?;

        Ok(Self {
            terminal,
            suspended: false,
        })
    }

    /// Step out of the alternate screen so a child process can own the TTY.
    fn suspend(&mut self) -> Result<()> {
        if self.suspended {
            return Ok(());
        }
        self.suspended = true;
        let _ = self.terminal.show_cursor();
        disable_raw_mode().ok();
        let mut stdout = io::stdout();
        execute!(stdout, LeaveAlternateScreen).context("leaving the alternate screen")?;
        Ok(())
    }

    /// Take the terminal back and repaint from scratch.
    fn resume(&mut self) -> Result<()> {
        if !self.suspended {
            return Ok(());
        }
        self.suspended = false;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen).context("entering the alternate screen")?;
        enable_raw_mode().context("enabling raw mode")?;
        self.terminal.clear().ok();
        let _ = self.terminal.autoresize();
        Ok(())
    }

    fn leave(&mut self) {
        if self.suspended {
            let _ = self.resume();
        }
        restore_terminal();
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn restore_terminal() {
    disable_raw_mode().ok();
    let mut stdout = io::stdout();
    let _ = execute!(
        stdout,
        ratatui::crossterm::cursor::Show,
        LeaveAlternateScreen
    );
}

/// Restore the terminal before printing a panic, so the message is readable.
fn install_panic_hook() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            disable_raw_mode().ok();
            let mut stdout = io::stdout();
            let _ = execute!(
                stdout,
                ratatui::crossterm::cursor::Show,
                LeaveAlternateScreen
            );
            previous(info);
        }));
    });
}

/// The config path the editor keys act on, for tests.
pub fn config_path(app: &App) -> PathBuf {
    app.loaded
        .path
        .clone()
        .unwrap_or_else(|| Path::new(&app.root).join("tug.toml"))
}
