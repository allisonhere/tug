//! The pipeline runner.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use regex::Regex;

use crate::actions;
use crate::actions::Exec;
use crate::condition;
use crate::engine::control::{Control, ControlRx};
use crate::engine::exec::{Runner, cap_output};
use crate::git;
use crate::model::{Config, EngineEvent, GitFacts, NoteLevel, Pipeline, RunStatus, StepStatus};
use crate::template;

/// What the user asked to run.
#[derive(Debug, Clone, Default)]
pub struct RunSpec {
    pub pipeline: String,
    pub profile: Option<String>,
    /// `key=value` overrides from the command line or the TUI.
    pub overrides: BTreeMap<String, String>,
    pub dry_run: bool,
    /// Skip every confirmation prompt.
    pub assume_yes: bool,
}

/// Execute a pipeline to completion, streaming events to `tx`.
///
/// Returns the final status. Failures are also reported as `RunFinished`.
pub fn run(
    config: &Config,
    facts: GitFacts,
    root: &Path,
    spec: RunSpec,
    tx: Sender<EngineEvent>,
    ctl: Arc<Control>,
    rx: ControlRx,
) -> RunStatus {
    let started = Instant::now();

    let Some(pipeline) = config.pipelines.get(&spec.pipeline) else {
        let message = format!("unknown pipeline `{}`", spec.pipeline);
        emit(
            &tx,
            EngineEvent::Note {
                level: NoteLevel::Error,
                message: message.clone(),
            },
        );
        emit(
            &tx,
            EngineEvent::RunFinished {
                status: RunStatus::Failed,
                duration_ms: 0,
                error: Some(message),
            },
        );
        return RunStatus::Failed;
    };

    let mut vars = config.vars.clone();
    let mut env = config.env.clone();

    if let Some(profile_name) = &spec.profile {
        match config.profiles.get(profile_name) {
            Some(profile) => {
                vars.extend(profile.vars.clone());
                env.extend(profile.env.clone());
            }
            None => emit(
                &tx,
                EngineEvent::Note {
                    level: NoteLevel::Warn,
                    message: format!("unknown profile `{profile_name}`; ignoring"),
                },
            ),
        }
    }

    vars.extend(pipeline.vars.clone());
    vars.extend(spec.overrides.clone());

    // Facts-derived defaults available to every template.
    vars.entry("remote".to_string())
        .or_insert_with(|| config.defaults.remote.clone());
    vars.entry("tag_prefix".to_string())
        .or_insert_with(|| config.defaults.tag_prefix.clone());
    if let Some(base) = config
        .defaults
        .base_branch
        .clone()
        .or_else(|| facts.base_branch.clone())
    {
        vars.entry("base_branch".to_string()).or_insert(base);
    }
    if let Some(tag) = facts.latest_tag.clone() {
        vars.entry("latest_tag".to_string()).or_insert(tag);
    }

    env.extend(pipeline.env.clone());

    let mut ctx = crate::model::Ctx {
        vars,
        env: env.clone(),
        facts,
        profile: spec.profile.clone(),
        dry_run: spec.dry_run,
    };

    emit(
        &tx,
        EngineEvent::RunStarted {
            pipeline: spec.pipeline.clone(),
            dry_run: spec.dry_run,
            steps: pipeline.steps.len(),
        },
    );

    // Pipeline-level gate.
    if let Some(when) = &pipeline.when {
        if !when.is_empty() {
            let verdict =
                condition::evaluate(when, &ctx, &mut |command| git::shell_ok(command, root));
            if !verdict.passed {
                let message = format!("pipeline gate failed: {}", verdict.reason);
                emit(
                    &tx,
                    EngineEvent::Note {
                        level: NoteLevel::Error,
                        message: message.clone(),
                    },
                );
                emit(
                    &tx,
                    EngineEvent::RunFinished {
                        status: RunStatus::Failed,
                        duration_ms: started.elapsed().as_millis() as u64,
                        error: Some(message),
                    },
                );
                return RunStatus::Failed;
            }
        }
    }

    if pipeline.steps.is_empty() {
        let message = "pipeline has no steps".to_string();
        emit(
            &tx,
            EngineEvent::Note {
                level: NoteLevel::Warn,
                message: message.clone(),
            },
        );
        emit(
            &tx,
            EngineEvent::RunFinished {
                status: RunStatus::Failed,
                duration_ms: 0,
                error: Some(message),
            },
        );
        return RunStatus::Failed;
    }

    let mut runner = Runner::new(
        std::mem::take(&mut ctx),
        root.to_path_buf(),
        config.defaults.clone(),
        spec.dry_run,
        tx.clone(),
        Arc::clone(&ctl),
        spec.assume_yes,
        rx.answer,
        rx.gate,
    );

    let mut status = RunStatus::Ok;
    let mut error: Option<String> = None;
    let mut cancelled = false;

    for (index, step) in pipeline.steps.iter().enumerate() {
        if ctl.is_cancelled() {
            cancelled = true;
            break;
        }

        // Per-step gate.
        if let Some(when) = &step.when {
            if !when.is_empty() {
                let verdict = condition::evaluate(when, &runner.ctx, &mut |command| {
                    git::shell_ok(command, root)
                });
                if !verdict.passed {
                    emit(
                        &tx,
                        EngineEvent::StepFinished {
                            index,
                            status: StepStatus::Skipped,
                            duration_ms: 0,
                            summary: verdict.reason,
                        },
                    );
                    continue;
                }
            }
        }

        runner.step = index;
        runner.silent = step.silent;
        runner.held.clear();
        runner.cwd = match &step.cwd {
            Some(cwd) => match template::render(cwd, &runner.ctx, false) {
                Ok(rendered) => {
                    let path = PathBuf::from(&rendered);
                    if path.is_absolute() {
                        path
                    } else {
                        root.join(path)
                    }
                }
                Err(_) => root.to_path_buf(),
            },
            None => root.to_path_buf(),
        };

        runner.env = env.clone();
        for (key, value) in &step.env {
            if let Ok(rendered) =
                template::render(value, &runner.ctx, config.defaults.strict_templates)
            {
                runner.env.insert(key.clone(), rendered);
            }
        }
        runner.timeout = step
            .timeout
            .or(config.defaults.timeout_secs)
            .map(Duration::from_secs);

        let preview = actions::preview(&step.action, &runner.ctx, &config.defaults).ok();
        let detail = preview
            .as_ref()
            .map(|p| p.detail.clone())
            .unwrap_or_else(|| step.action.kind().to_string());
        let command = preview.as_ref().and_then(|p| p.command.clone());

        if step.confirm && !spec.assume_yes && !spec.dry_run {
            let prompt = format!("Run step {} — {}?", index + 1, step.title());
            match runner.confirm(&prompt) {
                Ok(true) => {}
                Ok(false) => {
                    let message = format!("step {} declined", index + 1);
                    emit(
                        &tx,
                        EngineEvent::StepFinished {
                            index,
                            status: StepStatus::Skipped,
                            duration_ms: 0,
                            summary: message.clone(),
                        },
                    );
                    error = Some(message);
                    status = RunStatus::Failed;
                    break;
                }
                Err(err) => {
                    error = Some(format!("confirmation failed: {err}"));
                    status = RunStatus::Failed;
                    break;
                }
            }
        }

        emit(
            &tx,
            EngineEvent::StepStarted {
                index,
                label: step.title(),
                detail,
                command,
            },
        );

        let step_started = Instant::now();
        let retries = step.retries.unwrap_or(0);
        let mut attempt = 0u32;

        let outcome = loop {
            match actions::execute(&step.action, &mut runner) {
                Ok(executed) => break Ok(executed),
                Err(err) => {
                    runner.flush_held();
                    if attempt < retries && !ctl.is_cancelled() {
                        attempt += 1;
                        emit(
                            &tx,
                            EngineEvent::Note {
                                level: NoteLevel::Warn,
                                message: format!(
                                    "step {} attempt {} failed: {err}; retrying",
                                    index + 1,
                                    attempt
                                ),
                            },
                        );
                        continue;
                    }
                    break Err(err);
                }
            }
        };

        let duration_ms = step_started.elapsed().as_millis() as u64;

        match outcome {
            Ok(mut executed) => {
                cap_output(&mut executed.output);

                if let Some(capture) = &step.capture {
                    match Regex::new(&capture.regex) {
                        Ok(regex) => {
                            let group = capture.group.unwrap_or(1);
                            let value = regex.captures(&executed.output).and_then(|caps| {
                                caps.get(group)
                                    .or_else(|| caps.get(0))
                                    .map(|m| m.as_str().to_string())
                            });
                            match value {
                                Some(value) => {
                                    runner.ctx.vars.insert(capture.name.clone(), value.clone());
                                    emit(
                                        &tx,
                                        EngineEvent::Note {
                                            level: NoteLevel::Info,
                                            message: format!("captured {}={}", capture.name, value),
                                        },
                                    );
                                }
                                None => emit(
                                    &tx,
                                    EngineEvent::Note {
                                        level: NoteLevel::Warn,
                                        message: format!(
                                            "capture `{}` matched nothing in step {}",
                                            capture.name,
                                            index + 1
                                        ),
                                    },
                                ),
                            }
                        }
                        Err(err) => emit(
                            &tx,
                            EngineEvent::Note {
                                level: NoteLevel::Warn,
                                message: format!(
                                    "capture `{}` has an invalid regex: {err}",
                                    capture.name
                                ),
                            },
                        ),
                    }
                }

                let step_status = if ctl.is_cancelled() {
                    StepStatus::Cancelled
                } else if spec.dry_run && step.action.is_mutating() {
                    StepStatus::Planned
                } else {
                    StepStatus::Ok
                };

                emit(
                    &tx,
                    EngineEvent::StepFinished {
                        index,
                        status: step_status,
                        duration_ms,
                        summary: executed.summary,
                    },
                );

                if step_status == StepStatus::Cancelled {
                    cancelled = true;
                    break;
                }
            }
            Err(err) => {
                let message = err.to_string();
                let failing = if ctl.is_cancelled() {
                    StepStatus::Cancelled
                } else {
                    StepStatus::Failed
                };
                emit(
                    &tx,
                    EngineEvent::StepFinished {
                        index,
                        status: failing,
                        duration_ms,
                        summary: short(&message, 120),
                    },
                );

                if failing == StepStatus::Cancelled {
                    cancelled = true;
                    break;
                }

                status = RunStatus::Failed;
                error = Some(format!(
                    "step {} ({}) failed: {}",
                    index + 1,
                    step.title(),
                    short(&message, 400)
                ));
                if !step.continue_on_error {
                    break;
                }
            }
        }
    }

    if cancelled {
        status = RunStatus::Cancelled;
        error = Some("cancelled".to_string());
    }

    emit(
        &tx,
        EngineEvent::RunFinished {
            status,
            duration_ms: started.elapsed().as_millis() as u64,
            error,
        },
    );

    status
}

fn emit(tx: &Sender<EngineEvent>, event: EngineEvent) {
    let _ = tx.send(event);
}

fn short(text: &str, max: usize) -> String {
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    actions::truncate(line, max)
}

/// Pipelines ordered for display: explicit `order`, then name.
pub fn ordered(config: &Config) -> Vec<(&String, &Pipeline)> {
    let mut pipelines: Vec<(&String, &Pipeline)> = config.pipelines.iter().collect();
    pipelines.sort_by(|(a_name, a), (b_name, b)| {
        a.order
            .unwrap_or(i32::MAX)
            .cmp(&b.order.unwrap_or(i32::MAX))
            .then_with(|| a_name.cmp(b_name))
    });
    pipelines
}
