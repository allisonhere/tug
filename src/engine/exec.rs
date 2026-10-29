//! Executor internals: child spawning, streaming, timeouts, cancellation.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::actions::Exec;
use crate::engine::control::Control;
use crate::git::{CmdResult, CmdSpec};
use crate::model::{Ctx, Defaults, EngineEvent, NoteLevel, Stream};

/// How long to wait for the UI to release the terminal before an interactive step.
const GATE_TIMEOUT: Duration = Duration::from_secs(5);
/// Upper bound on captured output used for `capture` and failure messages.
const CAPTURE_LIMIT: usize = 2 * 1024 * 1024;

/// The [`Exec`] implementation used during a run.
pub struct Runner {
    pub ctx: Ctx,
    pub root: PathBuf,
    pub cwd: PathBuf,
    pub defaults: Defaults,
    pub dry_run: bool,
    pub step: usize,
    pub tx: Sender<EngineEvent>,
    pub ctl: Arc<Control>,
    pub assume_yes: bool,
    pub env: BTreeMap<String, String>,
    pub timeout: Option<Duration>,
    pub silent: bool,
    pub held: Vec<String>,
    answer_rx: Receiver<bool>,
    gate_rx: Receiver<()>,
}

impl Runner {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ctx: Ctx,
        root: PathBuf,
        defaults: Defaults,
        dry_run: bool,
        tx: Sender<EngineEvent>,
        ctl: Arc<Control>,
        assume_yes: bool,
        answer_rx: Receiver<bool>,
        gate_rx: Receiver<()>,
    ) -> Self {
        Self {
            cwd: root.clone(),
            env: ctx.env.clone(),
            ctx,
            root,
            defaults,
            dry_run,
            step: 0,
            tx,
            ctl,
            assume_yes,
            timeout: None,
            silent: false,
            held: Vec::new(),
            answer_rx,
            gate_rx,
        }
    }

    /// Emit any output held back by a silent step, e.g. after it failed.
    pub fn flush_held(&mut self) {
        if self.held.is_empty() {
            return;
        }
        let held = std::mem::take(&mut self.held);
        for line in held {
            self.send(Stream::Stdout, &line);
        }
    }

    fn send(&self, stream: Stream, line: &str) {
        let _ = self.tx.send(EngineEvent::Output {
            index: self.step,
            stream,
            line: line.to_string(),
        });
    }

    fn emit(&mut self, stream: Stream, line: &str) {
        if self.silent {
            if self.held.len() < 2000 {
                self.held.push(line.to_string());
            }
        } else {
            self.send(stream, line);
        }
    }

    fn spawn_streaming(&mut self, spec: &CmdSpec) -> Result<CmdResult> {
        if spec.print_command {
            self.emit(Stream::Stdout, &format!("$ {}", spec.display()));
        }

        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .envs(&spec.env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command
            .spawn()
            .with_context(|| format!("starting `{}`", spec.program))?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let (line_tx, line_rx) = mpsc::channel::<(Stream, String)>();
        if let Some(stdout) = stdout {
            spawn_reader(stdout, Stream::Stdout, line_tx.clone());
        }
        if let Some(stderr) = stderr {
            spawn_reader(stderr, Stream::Stderr, line_tx.clone());
        }
        drop(line_tx);

        self.ctl.set_child(Some(child));

        let started = Instant::now();
        let mut timed_out = false;
        let mut cancelled = false;
        let mut captured_stdout = String::new();
        let mut captured_stderr = String::new();
        let code = loop {
            let mut drained = false;
            while let Ok((stream, line)) = line_rx.try_recv() {
                drained = true;
                if !spec.quiet {
                    self.emit(stream, &line);
                }
                capture(stream, &line, &mut captured_stdout, &mut captured_stderr);
            }

            if !cancelled && self.ctl.is_cancelled() {
                cancelled = true;
                self.ctl.kill_child();
            }

            if !timed_out {
                if let Some(limit) = spec.timeout {
                    if started.elapsed() > limit {
                        timed_out = true;
                        self.ctl.kill_child();
                    }
                }
            }

            match self.ctl.try_wait_child() {
                Ok(Some(status)) => break status.code(),
                Ok(None) => {}
                Err(err) => bail!("waiting for `{}` failed: {err}", spec.program),
            }

            if !drained {
                std::thread::sleep(Duration::from_millis(15));
            }
        };

        // Drain whatever the readers buffered before the pipes closed.
        for _ in 0..200 {
            match line_rx.recv_timeout(Duration::from_millis(50)) {
                Ok((stream, line)) => {
                    if !spec.quiet {
                        self.emit(stream, &line);
                    }
                    capture(stream, &line, &mut captured_stdout, &mut captured_stderr);
                }
                Err(_) => break,
            }
        }
        self.ctl.take_child();

        if timed_out {
            self.emit(
                Stream::Stderr,
                &format!("timed out after {}s", started.elapsed().as_secs()),
            );
        }
        if cancelled {
            self.emit(Stream::Stderr, "cancelled");
        }

        cap_output(&mut captured_stdout);
        cap_output(&mut captured_stderr);

        Ok(CmdResult {
            code,
            stdout: captured_stdout,
            stderr: captured_stderr,
            timed_out,
            cancelled,
        })
    }

    fn spawn_inherit(&mut self, spec: &CmdSpec) -> Result<CmdResult> {
        if spec.print_command {
            self.emit(Stream::Stdout, &format!("$ {}", spec.display()));
        }

        let _ = self
            .tx
            .send(EngineEvent::InteractiveStart { index: self.step });
        // The UI releases the terminal and acknowledges; headless CLIs ack at once.
        if self.gate_rx.recv_timeout(GATE_TIMEOUT).is_err() {
            let _ = self.tx.send(EngineEvent::Note {
                level: NoteLevel::Warn,
                message: "terminal was not released in time; continuing anyway".to_string(),
            });
        }

        let status = Command::new(&spec.program)
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .envs(&spec.env)
            .status()
            .with_context(|| format!("starting `{}`", spec.program));

        let _ = self
            .tx
            .send(EngineEvent::InteractiveEnd { index: self.step });

        let status = status?;
        Ok(CmdResult {
            code: status.code(),
            ..CmdResult::default()
        })
    }
}

/// Append a streamed line to its capture buffer while there is room.
fn capture(stream: Stream, line: &str, stdout: &mut String, stderr: &mut String) {
    let buffer = match stream {
        Stream::Stdout => stdout,
        Stream::Stderr => stderr,
    };
    if buffer.len() >= CAPTURE_LIMIT {
        return;
    }
    buffer.push_str(line);
    buffer.push('\n');
}

fn spawn_reader<R: Read + Send + 'static>(reader: R, stream: Stream, tx: Sender<(Stream, String)>) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut buffer = Vec::new();
        loop {
            buffer.clear();
            match reader.read_until(b'\n', &mut buffer) {
                Ok(0) => break,
                Ok(_) => {
                    while matches!(buffer.last(), Some(b'\n' | b'\r')) {
                        buffer.pop();
                    }
                    let line = String::from_utf8_lossy(&buffer).into_owned();
                    if tx.send((stream, line)).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
}

impl Exec for Runner {
    fn ctx(&self) -> &Ctx {
        &self.ctx
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn cwd(&self) -> &Path {
        &self.cwd
    }

    fn defaults(&self) -> &Defaults {
        &self.defaults
    }

    fn dry_run(&self) -> bool {
        self.dry_run
    }

    fn cancelled(&self) -> bool {
        self.ctl.is_cancelled()
    }

    fn run(&mut self, spec: CmdSpec) -> Result<CmdResult> {
        if self.dry_run {
            if spec.print_command {
                self.emit(Stream::Stdout, &format!("$ {}   (dry run)", spec.display()));
            }
            return Ok(CmdResult {
                code: Some(0),
                ..CmdResult::default()
            });
        }
        if self.ctl.is_cancelled() {
            return Ok(CmdResult {
                cancelled: true,
                ..CmdResult::default()
            });
        }

        let mut spec = spec;
        if spec.timeout.is_none() {
            spec.timeout = self.timeout;
        }

        if spec.inherit_stdio {
            self.spawn_inherit(&spec)
        } else {
            self.spawn_streaming(&spec)
        }
    }

    fn run_readonly(&mut self, mut spec: CmdSpec) -> Result<CmdResult> {
        if spec.timeout.is_none() {
            spec.timeout = Some(Duration::from_secs(120));
        }
        spec.print_command = false;
        spec.quiet = true;
        self.spawn_streaming(&spec)
    }

    fn note(&mut self, level: NoteLevel, message: &str) {
        let _ = self.tx.send(EngineEvent::Note {
            level,
            message: message.to_string(),
        });
    }

    fn set_var(&mut self, key: &str, value: String) {
        self.ctx.vars.insert(key.to_string(), value);
    }

    fn confirm(&mut self, prompt: &str) -> Result<bool> {
        if self.assume_yes {
            return Ok(true);
        }
        let _ = self.tx.send(EngineEvent::Confirm {
            index: self.step,
            prompt: prompt.to_string(),
        });
        match self.answer_rx.recv_timeout(Duration::from_secs(600)) {
            Ok(answer) => Ok(answer),
            Err(_) => bail!("timed out waiting for confirmation"),
        }
    }

    fn line(&mut self, text: &str) {
        self.emit(Stream::Stdout, text);
    }
}

/// Trim captured output to the capture limit.
pub fn cap_output(text: &mut String) {
    if text.len() > CAPTURE_LIMIT {
        let mut cut = CAPTURE_LIMIT;
        while !text.is_char_boundary(cut) && cut > 0 {
            cut -= 1;
        }
        text.truncate(cut);
        text.push_str("\n… output truncated");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    use crate::model::GitFacts;

    fn runner() -> (Runner, mpsc::Receiver<EngineEvent>) {
        let (tx, rx) = mpsc::channel();
        let (control, control_rx) = Control::new();
        let ctx = Ctx {
            facts: GitFacts::default(),
            ..Ctx::default()
        };
        let runner = Runner::new(
            ctx,
            std::env::temp_dir(),
            Defaults::default(),
            false,
            tx,
            control,
            true,
            control_rx.answer,
            control_rx.gate,
        );
        (runner, rx)
    }

    #[test]
    fn captures_stdout_and_stderr_while_streaming() {
        let (mut runner, rx) = runner();
        let spec = CmdSpec::new("sh", std::env::temp_dir()).args(["-c", "echo out; echo err >&2"]);

        let result = runner.run(spec).expect("run");

        assert!(result.success());
        assert_eq!(result.stdout, "out\n");
        assert_eq!(result.stderr, "err\n");

        let lines: Vec<String> = rx
            .try_iter()
            .filter_map(|event| match event {
                EngineEvent::Output { line, .. } => Some(line),
                _ => None,
            })
            .collect();
        assert!(lines.iter().any(|line| line == "out"), "{lines:?}");
        assert!(lines.iter().any(|line| line == "err"), "{lines:?}");
    }

    #[test]
    fn readonly_runs_capture_without_logging() {
        let (mut runner, rx) = runner();
        let spec = CmdSpec::new("sh", std::env::temp_dir()).args(["-c", "printf 'a\\nb\\n'"]);

        let result = runner.run_readonly(spec).expect("run");

        assert_eq!(result.stdout, "a\nb\n");
        let logged = rx
            .try_iter()
            .any(|event| matches!(event, EngineEvent::Output { .. }));
        assert!(!logged, "read-only output should stay out of the step log");
    }

    #[test]
    fn failed_commands_still_capture_diagnostics() {
        let (mut runner, _rx) = runner();
        let spec = CmdSpec::new("sh", std::env::temp_dir()).args(["-c", "echo nope >&2; exit 3"]);

        let result = runner.run_readonly(spec).expect("run");

        assert_eq!(result.code, Some(3));
        assert_eq!(result.stderr, "nope\n");
    }
}
