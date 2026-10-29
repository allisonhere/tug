use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Instant;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tug::config::LoadedConfig;
use tug::engine::Control;
use tug::model::{Action, Config, Force, GitFacts, Pipeline, RunStatus, Step, StepStatus};
use tug::tui::app::{App, LogKind, RunState, StepRow};
use tug::tui::render;

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

fn main() {
    let mut config = Config::default();
    config.pipelines.insert(
        "push".into(),
        Pipeline {
            description: Some("Stage everything, commit, and push".into()),
            order: Some(10),
            steps: vec![
                step(Action::Stage {
                    paths: vec![],
                    all: true,
                    update: false,
                }),
                step(Action::Commit {
                    message: "feat: add the thing".into(),
                    all: false,
                    allow_empty: false,
                    amend: false,
                    signoff: false,
                    no_verify: false,
                }),
                step(Action::Push {
                    remote: None,
                    branch: None,
                    set_upstream: true,
                    force: Force::No,
                    tags: false,
                }),
            ],
            ..Pipeline::default()
        },
    );
    config.pipelines.insert(
        "pr".into(),
        Pipeline {
            description: Some("Push, open or update a pull request, then watch CI".into()),
            order: Some(20),
            steps: vec![
                step(Action::Push {
                    remote: None,
                    branch: None,
                    set_upstream: true,
                    force: Force::No,
                    tags: false,
                }),
                step(Action::Pr {
                    title: None,
                    body: None,
                    base: Some("main".into()),
                    draft: false,
                    update: true,
                    reviewers: vec![],
                    labels: vec![],
                }),
                step(Action::Watch {
                    timeout_secs: None,
                    interval_secs: Some(10),
                }),
            ],
            ..Pipeline::default()
        },
    );
    config.pipelines.insert(
        "release".into(),
        Pipeline {
            description: Some(
                "Bump the version, tag it, push, and publish a GitHub release".into(),
            ),
            order: Some(30),
            steps: vec![step(Action::Tag {
                name: "{{ version_tag }}".into(),
                message: None,
                force: false,
                sign: false,
                push: true,
                remote: None,
            })],
            ..Pipeline::default()
        },
    );

    let loaded = LoadedConfig {
        config,
        path: Some(PathBuf::from("/home/allie/Projects/tug/tug.toml")),
        sources: vec![PathBuf::from("/home/allie/Projects/tug/tug.toml")],
        problems: vec![],
    };
    let facts = GitFacts {
        root: PathBuf::from("/home/allie/Projects/tug"),
        in_repo: true,
        branch: Some("feat/eyecandy".into()),
        sha: "9f2c1ab".into(),
        short_sha: "9f2c1ab".into(),
        repo: Some("allisonhere/tug".into()),
        remote: Some("origin".into()),
        upstream: Some("origin/feat/eyecandy".into()),
        ahead: 2,
        behind: 0,
        dirty: true,
        staged: 1,
        unstaged: 2,
        untracked: 1,
        latest_tag: Some("v0.3.1".into()),
        base_branch: Some("main".into()),
        ..GitFacts::default()
    };
    let (tx, _rx) = mpsc::channel();
    let mut app = App::new(
        loaded,
        PathBuf::from("/home/allie/Projects/tug"),
        facts,
        Some("push".into()),
        None,
        tx,
    );

    let (control, _crx) = Control::new();
    app.run = Some(RunState {
        pipeline: "push".into(),
        dry_run: false,
        steps: vec![
            StepRow {
                label: "git.stage".into(),
                detail: "git add -A".into(),
                command: Some("git add -A".into()),
                status: StepStatus::Ok,
                duration_ms: 32,
                summary: "4 files staged".into(),
            },
            StepRow {
                label: "git.commit".into(),
                detail: "feat: add the thing".into(),
                command: Some("git commit -m 'feat: add the thing'".into()),
                status: StepStatus::Running,
                duration_ms: 0,
                summary: String::new(),
            },
            StepRow {
                label: "git.push".into(),
                detail: "origin/feat/eyecandy".into(),
                command: None,
                status: StepStatus::Pending,
                duration_ms: 0,
                summary: String::new(),
            },
        ],
        status: None,
        current: Some(1),
        started: Instant::now(),
        elapsed_ms: 1240,
        control,
        error: None,
    });
    app.pipelines[0].status = Some(StepStatus::Running);
    for (kind, text) in [
        (LogKind::System, "push · 3 steps"),
        (LogKind::Command, "$ git add -A"),
        (LogKind::Out, ""),
        (LogKind::System, "4 files staged"),
        (LogKind::Command, "$ git commit -m 'feat: add the thing'"),
        (LogKind::Out, "[feat/eyecandy 9f2c1ab] feat: add the thing"),
        (
            LogKind::Out,
            " 3 files changed, 121 insertions(+), 4 deletions(-)",
        ),
    ] {
        app.log.push(kind, text);
    }

    let mode = std::env::args().nth(1).unwrap_or_else(|| "run".into());
    match mode.as_str() {
        "idle" => app.run = None,
        "plan" => {
            app.run = None;
            app.modal = Some(tug::tui::app::Modal::Plan {
                pipeline: "push".into(),
                rows: vec![
                    tug::plan::PlanRow {
                        index: 1,
                        label: "git.stage".into(),
                        detail: "git add -A".into(),
                        command: Some("git add -A".into()),
                        mutating: true,
                        gate: None,
                    },
                    tug::plan::PlanRow {
                        index: 2,
                        label: "git.commit".into(),
                        detail: "feat: add the thing".into(),
                        command: Some("git commit -m 'feat: add the thing'".into()),
                        mutating: true,
                        gate: None,
                    },
                    tug::plan::PlanRow {
                        index: 3,
                        label: "git.push".into(),
                        detail: "origin/feat/eyecandy".into(),
                        command: Some("git push --set-upstream origin feat/eyecandy".into()),
                        mutating: true,
                        gate: None,
                    },
                ],
                dry_run: true,
                scroll: 0,
            });
        }
        "help" => {
            app.run = None;
            app.modal = Some(tug::tui::app::Modal::Help);
        }
        "info" => {
            app.run = None;
            app.modal = Some(tug::tui::app::Modal::Info);
        }
        "confirm" => {
            app.modal = Some(tug::tui::app::Modal::Confirm {
                prompt: "Run step 2 — git.commit?".into(),
            });
        }
        "error" => {
            app.run = None;
            app.modal = Some(tug::tui::app::Modal::Error {
                title: "configuration".into(),
                message: "pipelines.push.steps[1].mesage: unknown key `mesage`\npipelines.push.steps[2].action: unknown action `git.teleport`".into(),
            });
        }
        "done" => {
            if let Some(run) = &mut app.run {
                run.status = Some(RunStatus::Ok);
                run.current = None;
                run.elapsed_ms = 3410;
                for step in &mut run.steps {
                    step.status = StepStatus::Ok;
                    step.duration_ms = 120;
                }
                run.steps[1].summary = "9f2c1ab feat: add the thing".into();
            }
            app.pipelines[0].status = Some(StepStatus::Ok);
        }
        "tiny" => {
            app.on_resize(58, 12);
            dump(&app, 58, 12, true);
            return;
        }
        _ => {}
    }

    dump(&app, 100, 30, false);
}

fn dump(app: &App, width: u16, height: u16, tiny: bool) {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| render::draw(frame, app)).unwrap();

    let buffer = terminal.backend().buffer().clone();
    for y in 0..height {
        let mut line = String::new();
        for x in 0..width {
            line.push_str(buffer[(x, y)].symbol());
        }
        let line = line.trim_end();
        if tiny {
            println!("|{line}|");
        } else {
            println!("{line}");
        }
    }
}
