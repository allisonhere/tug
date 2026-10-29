//! Command-line entry points.

use std::collections::BTreeMap;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use crate::config::{self, LoadedConfig};
use crate::engine::{self, Control, RunSpec};
use crate::git;
use crate::model::{ConfirmPolicy, Ctx, GitFacts, NoteLevel, RunStatus, StepStatus};
use crate::plan;

#[derive(Debug, Parser)]
#[command(
    name = "tug",
    version,
    about = "Config-driven ship pipelines for git and GitHub",
    long_about = "tug reads a tug.toml file and runs a pipeline of git and GitHub steps: \
                  stage, commit, push, tag, pull request, release, and CI watching.\n\n\
                  Run without a subcommand for the interactive TUI."
)]
struct Cli {
    /// Path to a config file. Replaces config discovery.
    #[arg(long, global = true, env = "TUG_CONFIG", value_name = "PATH")]
    config: Option<PathBuf>,

    /// Named profile from [profiles.*].
    #[arg(long, global = true, value_name = "NAME")]
    profile: Option<String>,

    /// Override a variable: --set summary="fix the thing".
    #[arg(long = "set", global = true, value_name = "KEY=VALUE", value_parser = parse_kv)]
    overrides: Vec<(String, String)>,

    /// Describe mutating steps instead of performing them.
    #[arg(long, global = true)]
    dry_run: bool,

    /// Skip every confirmation.
    #[arg(long, short = 'y', global = true)]
    yes: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run a pipeline to completion.
    Run {
        /// Pipeline name from tug.toml.
        pipeline: String,
    },
    /// Print the resolved plan without running anything.
    Plan { pipeline: String },
    /// List pipelines.
    List,
    /// Validate the configuration.
    Validate,
    /// Write a starter tug.toml.
    Init {
        /// Overwrite an existing config.
        #[arg(long)]
        force: bool,
        /// Directory to write into (default: repository root).
        #[arg(long, value_name = "DIR")]
        path: Option<PathBuf>,
    },
}

fn parse_kv(raw: &str) -> std::result::Result<(String, String), String> {
    match raw.split_once('=') {
        Some((key, value)) if !key.is_empty() => Ok((key.to_string(), value.to_string())),
        _ => Err(format!("expected KEY=VALUE, got {raw:?}")),
    }
}

pub fn entry() -> Result<()> {
    let cli = Cli::parse();
    let cwd = std::env::current_dir().context("reading current directory")?;

    match cli.command {
        None => {
            let overrides: BTreeMap<String, String> = cli.overrides.iter().cloned().collect();
            return crate::tui::run(cli.config, None, cli.profile, overrides, cli.dry_run);
        }
        Some(Command::Init { force, path }) => {
            let dir = path.unwrap_or_else(|| git::discover_root(&cwd).unwrap_or(cwd));
            std::fs::create_dir_all(&dir).ok();
            let written = config::init::write(&dir, force)?;
            println!("wrote {}", written.display());
            println!("next: tug validate, then tug plan push");
            return Ok(());
        }
        Some(command) => {
            let root = git::discover_root(&cwd).unwrap_or(cwd);
            let loaded = config::load(cli.config.as_deref(), &root)?;
            let overrides: BTreeMap<String, String> = cli.overrides.iter().cloned().collect();
            let session = Session::new(&loaded, &root, cli.profile.clone(), overrides.clone());
            if !matches!(command, Command::Validate) {
                session.report_problems(&loaded);
            }

            match command {
                Command::List => list(&loaded, &session),
                Command::Validate => validate(&loaded),
                Command::Plan { pipeline } => {
                    plan_cmd(&loaded, &session, &pipeline)?;
                }
                Command::Run { pipeline } => {
                    let spec = RunSpec {
                        pipeline,
                        profile: cli.profile.clone(),
                        overrides,
                        dry_run: cli.dry_run,
                        assume_yes: cli.yes,
                    };
                    let code = run_headless(&loaded, &session, spec)?;
                    std::process::exit(code);
                }
                Command::Init { .. } => unreachable!("handled above"),
            }
        }
    }

    Ok(())
}

struct Session {
    root: PathBuf,
    facts: GitFacts,
    profile: Option<String>,
    overrides: BTreeMap<String, String>,
    dry_run: bool,
    color: bool,
}

impl Session {
    fn new(
        loaded: &LoadedConfig,
        root: &Path,
        profile: Option<String>,
        overrides: BTreeMap<String, String>,
    ) -> Self {
        let facts = git::facts(root, &loaded.config.defaults.remote).unwrap_or(GitFacts {
            root: root.to_path_buf(),
            ..GitFacts::default()
        });
        Self {
            root: root.to_path_buf(),
            facts,
            profile,
            overrides,
            dry_run: false,
            color: std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
        }
    }

    fn ctx(&self, loaded: &LoadedConfig, pipeline: Option<&str>, dry_run: bool) -> Ctx {
        config::context_with(
            loaded,
            &self.facts,
            self.profile.as_deref(),
            dry_run,
            pipeline,
            &self.overrides,
        )
    }

    fn report_problems(&self, loaded: &LoadedConfig) {
        if loaded.problems.is_empty() {
            return;
        }
        for problem in &loaded.problems {
            let prefix = match problem.severity {
                config::Severity::Error => self.paint("1;31", "error"),
                config::Severity::Warning => self.paint("1;33", "warning"),
            };
            eprintln!("{prefix} {}: {}", problem.path, problem.message);
        }
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }
}

fn list(loaded: &LoadedConfig, session: &Session) {
    if loaded.config.pipelines.is_empty() {
        println!("no pipelines defined — run `tug init`");
        return;
    }
    for (name, pipeline) in engine::run::ordered(&loaded.config) {
        let ctx = session.ctx(loaded, Some(name), false);
        let gate = pipeline.when.as_ref().map(|when| {
            crate::condition::evaluate(when, &ctx, &mut |command| {
                git::shell_ok(command, &session.root)
            })
        });
        let status = match gate {
            Some(verdict) if !verdict.passed => format!("gated: {}", verdict.reason),
            _ => "ready".to_string(),
        };
        println!(
            "{:<14} {:<44} {} step(s)  {}",
            name,
            pipeline.description.clone().unwrap_or_default(),
            pipeline.steps.len(),
            status
        );
    }
}

fn validate(loaded: &LoadedConfig) {
    if loaded.problems.is_empty() {
        println!("ok — {} pipeline(s)", loaded.config.pipelines.len());
        return;
    }
    for problem in &loaded.problems {
        println!("{problem}");
    }
    if loaded.has_errors() {
        std::process::exit(1);
    }
}

fn plan_cmd(loaded: &LoadedConfig, session: &Session, pipeline: &str) -> Result<()> {
    let ctx = session.ctx(loaded, Some(pipeline), session.dry_run);
    print!("{}", plan::render(&loaded.config, &ctx, pipeline)?);
    Ok(())
}

fn run_headless(loaded: &LoadedConfig, session: &Session, spec: RunSpec) -> Result<i32> {
    if loaded.config.pipelines.is_empty() {
        bail!(
            "no pipelines defined in {} — run `tug init`",
            display_path(loaded)
        );
    }
    if loaded.has_errors() {
        bail!("configuration has errors — run `tug validate`");
    }

    let ctx = session.ctx(loaded, Some(&spec.pipeline), spec.dry_run);

    if let Ok(text) = plan::render(&loaded.config, &ctx, &spec.pipeline) {
        println!("{text}");
    }

    let policy = loaded
        .config
        .pipelines
        .get(&spec.pipeline)
        .and_then(|pipeline| pipeline.confirm)
        .unwrap_or(loaded.config.defaults.confirm);

    if !spec.assume_yes
        && !spec.dry_run
        && policy != ConfirmPolicy::Never
        && !confirm("run this pipeline?")?
    {
        println!("aborted");
        return Ok(1);
    }

    let (control, control_rx) = Control::new();
    let (tx, events) = mpsc::channel();

    let config = loaded.config.clone();
    let facts = session.facts.clone();
    let root = session.root.clone();
    let worker_control = Arc::clone(&control);
    let handle = std::thread::spawn(move || {
        engine::run(&config, facts, &root, spec, tx, worker_control, control_rx)
    });

    let mut status = RunStatus::Failed;
    let mut dry_run = false;

    for event in events {
        match event {
            crate::model::EngineEvent::RunStarted {
                pipeline,
                dry_run: dry_run_flag,
                steps,
            } => {
                dry_run = dry_run_flag;
                let mode = if dry_run { " (dry run)" } else { "" };
                println!(
                    "\n{} {pipeline}{mode} · {steps} steps",
                    session.paint("1;36", "tug")
                );
            }
            crate::model::EngineEvent::StepStarted {
                index,
                label,
                detail,
                ..
            } => {
                println!(
                    "\n{}  {}  {}",
                    session.paint("1;36", &format!("{:>2}", index + 1)),
                    session.paint("1", &label),
                    session.paint("2", &detail)
                );
            }
            crate::model::EngineEvent::Output { stream, line, .. } => match stream {
                crate::model::Stream::Stdout => println!("    {line}"),
                crate::model::Stream::Stderr => {
                    println!("    {}", session.paint("33", &format!("! {line}")))
                }
            },
            crate::model::EngineEvent::StepFinished {
                status: step_status,
                duration_ms,
                summary,
                ..
            } => {
                let (glyph, paint) = match step_status {
                    StepStatus::Ok => ("✓", "1;32"),
                    StepStatus::Planned => ("◇", "1;34"),
                    StepStatus::Failed => ("✗", "1;31"),
                    StepStatus::Skipped => ("–", "2"),
                    StepStatus::Cancelled => ("⊘", "1;33"),
                    StepStatus::Running | StepStatus::Pending => ("·", "2"),
                };
                println!(
                    "    {} {} {}",
                    session.paint(paint, glyph),
                    summary,
                    session.paint("2", &format!("({:.1}s)", duration_ms as f64 / 1000.0))
                );
            }
            crate::model::EngineEvent::Note { level, message } => {
                let (glyph, paint) = match level {
                    NoteLevel::Info => ("·", "2"),
                    NoteLevel::Warn => ("!", "1;33"),
                    NoteLevel::Error => ("✗", "1;31"),
                };
                println!("    {} {}", session.paint(paint, glyph), message);
            }
            crate::model::EngineEvent::Confirm { prompt, .. } => {
                let yes = confirm(&prompt)?;
                control.answer(yes);
            }
            crate::model::EngineEvent::InteractiveStart { .. } => {
                // Nothing to release in a headless run; the child inherits stdio.
                control.release_terminal();
            }
            crate::model::EngineEvent::RunFinished {
                status: finished,
                duration_ms,
                error,
            } => {
                status = finished;
                let (glyph, paint, word) = match (finished, dry_run) {
                    (RunStatus::Ok, true) => ("✓", "1;34", "planned"),
                    (RunStatus::Ok, false) => ("✓", "1;32", "passed"),
                    (RunStatus::Failed, _) => ("✗", "1;31", "failed"),
                    (RunStatus::Cancelled, _) => ("⊘", "1;33", "cancelled"),
                };
                println!(
                    "\n{} {} {}",
                    session.paint(paint, glyph),
                    word,
                    session.paint("2", &format!("in {:.1}s", duration_ms as f64 / 1000.0))
                );
                if let Some(error) = error {
                    if finished != RunStatus::Ok {
                        println!("{}", session.paint("31", &error));
                    }
                }
            }
            crate::model::EngineEvent::InteractiveEnd { .. } => {}
        }
        let _ = std::io::stdout().flush();
    }

    let status = handle.join().unwrap_or(status);
    Ok(match status {
        RunStatus::Ok => 0,
        RunStatus::Failed => 1,
        RunStatus::Cancelled => 130,
    })
}

fn confirm(prompt: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        return Ok(false);
    }
    print!("\n{prompt} [y/N] ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn display_path(loaded: &LoadedConfig) -> String {
    loaded
        .path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "tug.toml".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kv_pairs() {
        assert_eq!(
            parse_kv("summary=fix: thing").unwrap(),
            ("summary".to_string(), "fix: thing".to_string())
        );
    }

    #[test]
    fn rejects_missing_value() {
        assert!(parse_kv("summary").is_err());
        assert!(parse_kv("=x").is_err());
    }
}
