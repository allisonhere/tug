//! The shared data contract between config, engine, and UI.
//!
//! Everything in this file is pure data. No I/O, no process spawning, no terminal
//! access. If a type here changes shape, update `docs/architecture.md` and
//! `docs/configuration.md` in the same change.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Config document
// ---------------------------------------------------------------------------

/// Top-level shape of a `tug.toml` document.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Config {
    /// Schema version. Reserved for future migrations.
    #[serde(default)]
    pub version: Option<u32>,
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub ui: UiConfig,
    /// Named pipelines, keyed by pipeline name.
    #[serde(default)]
    pub pipelines: BTreeMap<String, Pipeline>,
    /// Named profiles applied with `--profile`.
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Defaults {
    /// Default git remote for push/fetch/tag operations.
    #[serde(default = "default_remote")]
    pub remote: String,
    /// Base branch for pull requests. `None` means "detect the repo default".
    #[serde(default)]
    pub base_branch: Option<String>,
    /// Prefix used by `bump` and tag suggestions (for example `v`).
    #[serde(default = "default_tag_prefix")]
    pub tag_prefix: String,
    /// Variable name that `bump` writes and templates read (default `version`).
    #[serde(default = "default_version_var")]
    pub version_var: String,
    /// Shell used for `shell` steps. `None` means `sh -c`.
    #[serde(default)]
    pub shell: Option<String>,
    /// Fail on an unknown `{{ variable }}` instead of rendering it empty.
    #[serde(default = "default_true")]
    pub strict_templates: bool,
    /// When a pipeline must ask for confirmation before mutating anything.
    #[serde(default)]
    pub confirm: ConfirmPolicy,
    /// Default per-step timeout.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            remote: default_remote(),
            base_branch: None,
            tag_prefix: default_tag_prefix(),
            version_var: default_version_var(),
            shell: None,
            strict_templates: true,
            confirm: ConfirmPolicy::default(),
            timeout_secs: None,
        }
    }
}

fn default_remote() -> String {
    "origin".to_string()
}

fn default_tag_prefix() -> String {
    "v".to_string()
}

fn default_version_var() -> String {
    "version".to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmPolicy {
    /// Confirm only when the pipeline contains a mutating step.
    #[default]
    Destructive,
    /// Always show the plan before running.
    Always,
    /// Never ask; `--yes` behaviour.
    Never,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UiConfig {
    /// Theme name. `tide` is the default; `mono` is a no-colour fallback.
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Keep the log pane pinned to the newest line while a step runs.
    #[serde(default)]
    pub follow_log: Option<bool>,
    #[serde(default)]
    pub density: Density,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            follow_log: None,
            density: Density::default(),
        }
    }
}

fn default_theme() -> String {
    "tide".to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Density {
    #[default]
    Compact,
    Comfortable,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Profile {
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Overrides applied to `[defaults]` when this profile is active.
    #[serde(default)]
    pub defaults: Option<Defaults>,
}

// ---------------------------------------------------------------------------
// Pipelines and steps
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Pipeline {
    #[serde(default)]
    pub description: Option<String>,
    /// Explicit sort key in the UI. Lower runs first; ties sort by name.
    #[serde(default)]
    pub order: Option<i32>,
    /// Pipeline-level gate. A failing `when` disables the pipeline in the UI.
    #[serde(default)]
    pub when: Option<When>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
    /// Overrides `[defaults].confirm` for this pipeline.
    #[serde(default)]
    pub confirm: Option<ConfirmPolicy>,
    #[serde(default)]
    pub steps: Vec<Step>,
}

/// One step: a common envelope plus an `action` discriminated by its `action` key.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Step {
    #[serde(flatten)]
    pub action: Action,
    /// Display name. Defaults to the action kind.
    ///
    /// Named `label` rather than `name` because `name` belongs to
    /// `Action::Tag` and serde's flatten would otherwise steal it.
    #[serde(default)]
    pub label: Option<String>,
    /// Skip this step unless every condition holds.
    #[serde(default)]
    pub when: Option<When>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Working directory, relative to the repo root by default.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Step timeout in seconds. Overrides `[defaults].timeout_secs`.
    #[serde(default)]
    pub timeout: Option<u64>,
    /// Extra attempts after the first failure. `None` means 0.
    #[serde(default)]
    pub retries: Option<u32>,
    /// Keep going even if this step fails.
    #[serde(default)]
    pub continue_on_error: bool,
    /// Ask before running this step, even when the pipeline was confirmed.
    #[serde(default)]
    pub confirm: bool,
    /// Do not echo the step's output unless it fails.
    #[serde(default)]
    pub silent: bool,
    /// Extract a variable from the step output.
    #[serde(default)]
    pub capture: Option<Capture>,
}

impl Step {
    pub fn title(&self) -> String {
        match &self.label {
            Some(label) => label.clone(),
            None => self.action.kind().to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Capture {
    /// Variable name written into the run context.
    pub name: String,
    /// Regex applied to the step output.
    pub regex: String,
    /// Capture group to take. Defaults to 1, or the whole match when no groups exist.
    #[serde(default)]
    pub group: Option<usize>,
}

/// The action of a step. Serialised as a map with an `action` tag.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "action")]
pub enum Action {
    #[serde(rename = "git.stage", alias = "stage")]
    Stage {
        #[serde(default)]
        paths: Vec<String>,
        #[serde(default)]
        all: bool,
        #[serde(default)]
        update: bool,
    },
    #[serde(rename = "git.unstage", alias = "unstage")]
    Unstage {
        #[serde(default)]
        paths: Vec<String>,
        #[serde(default)]
        all: bool,
    },
    #[serde(rename = "git.commit", alias = "commit")]
    Commit {
        message: String,
        #[serde(default)]
        all: bool,
        #[serde(default)]
        allow_empty: bool,
        #[serde(default)]
        amend: bool,
        #[serde(default)]
        signoff: bool,
        #[serde(default)]
        no_verify: bool,
    },
    #[serde(rename = "git.push", alias = "push")]
    Push {
        #[serde(default)]
        remote: Option<String>,
        #[serde(default)]
        branch: Option<String>,
        #[serde(default)]
        set_upstream: bool,
        #[serde(default)]
        force: Force,
        #[serde(default)]
        tags: bool,
    },
    #[serde(rename = "git.fetch", alias = "fetch")]
    Fetch {
        #[serde(default)]
        remote: Option<String>,
        #[serde(default)]
        prune: bool,
        #[serde(default)]
        tags: bool,
    },
    #[serde(rename = "git.checkout", alias = "checkout")]
    Checkout {
        target: String,
        #[serde(default)]
        create: bool,
    },
    #[serde(rename = "git.tag", alias = "tag")]
    Tag {
        name: String,
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        force: bool,
        #[serde(default)]
        sign: bool,
        #[serde(default)]
        push: bool,
        #[serde(default)]
        remote: Option<String>,
    },
    #[serde(rename = "github.pr", alias = "pr")]
    Pr {
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        body: Option<String>,
        #[serde(default)]
        base: Option<String>,
        #[serde(default)]
        draft: bool,
        /// Update an existing PR for this branch instead of failing.
        #[serde(default = "default_true")]
        update: bool,
        #[serde(default)]
        reviewers: Vec<String>,
        #[serde(default)]
        labels: Vec<String>,
    },
    #[serde(rename = "github.release", alias = "release")]
    Release {
        tag: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        notes: Option<String>,
        #[serde(default)]
        notes_file: Option<String>,
        #[serde(default)]
        draft: bool,
        #[serde(default)]
        prerelease: bool,
        #[serde(default)]
        generate_notes: bool,
        #[serde(default)]
        assets: Vec<String>,
        /// Update the release when the tag already has one.
        #[serde(default = "default_true")]
        update: bool,
    },
    #[serde(rename = "github.watch", alias = "watch")]
    Watch {
        #[serde(default)]
        timeout_secs: Option<u64>,
        #[serde(default)]
        interval_secs: Option<u64>,
    },
    #[serde(rename = "shell", alias = "run")]
    Shell {
        run: String,
        /// Hand the terminal to the child process (needs a TTY).
        #[serde(default)]
        interactive: bool,
        /// Echo the resolved command before running it.
        #[serde(default = "default_true")]
        print_command: bool,
    },
    #[serde(rename = "set")]
    Set { key: String, value: String },
    #[serde(rename = "bump")]
    Bump {
        #[serde(default)]
        part: BumpPart,
        /// Variable to write. Defaults to `[defaults].version_var`.
        #[serde(default)]
        var: Option<String>,
        /// Glob selecting candidate tags. Defaults to `<tag_prefix>*`.
        #[serde(default)]
        tag: Option<String>,
    },
    #[serde(rename = "confirm")]
    Confirm { prompt: String },
    #[serde(rename = "sleep")]
    Sleep {
        #[serde(default = "default_sleep_ms")]
        ms: u64,
    },
}

fn default_sleep_ms() -> u64 {
    500
}

impl Action {
    /// Stable identifier used for display, filtering, and dry-run previews.
    pub fn kind(&self) -> &'static str {
        match self {
            Action::Stage { .. } => "git.stage",
            Action::Unstage { .. } => "git.unstage",
            Action::Commit { .. } => "git.commit",
            Action::Push { .. } => "git.push",
            Action::Fetch { .. } => "git.fetch",
            Action::Checkout { .. } => "git.checkout",
            Action::Tag { .. } => "git.tag",
            Action::Pr { .. } => "github.pr",
            Action::Release { .. } => "github.release",
            Action::Watch { .. } => "github.watch",
            Action::Shell { .. } => "shell",
            Action::Set { .. } => "set",
            Action::Bump { .. } => "bump",
            Action::Confirm { .. } => "confirm",
            Action::Sleep { .. } => "sleep",
        }
    }

    /// Whether this action can change local state, a remote, or a release.
    ///
    /// Shell commands are treated as mutating because tug cannot see inside them.
    pub fn is_mutating(&self) -> bool {
        !matches!(
            self,
            Action::Watch { .. }
                | Action::Set { .. }
                | Action::Bump { .. }
                | Action::Confirm { .. }
                | Action::Sleep { .. }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Force {
    #[default]
    No,
    WithLease,
    Force,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BumpPart {
    Major,
    Minor,
    #[default]
    Patch,
    None,
}

// ---------------------------------------------------------------------------
// Conditions
// ---------------------------------------------------------------------------

/// A structured `when` gate. All present fields must hold (logical AND);
/// `any` adds an OR group and `not` inverts the whole predicate.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct When {
    /// Branch glob, e.g. `main` or `release/*`.
    #[serde(default)]
    pub branch: Option<String>,
    /// Require a clean / dirty working tree.
    #[serde(default)]
    pub clean: Option<bool>,
    /// Any tag must match this glob.
    #[serde(default)]
    pub tag: Option<String>,
    /// The current branch has an upstream.
    #[serde(default)]
    pub upstream: Option<bool>,
    /// The branch is ahead of its upstream.
    #[serde(default)]
    pub ahead: Option<bool>,
    /// Environment variable equality checks.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Variable equality checks.
    #[serde(default)]
    pub var: BTreeMap<String, String>,
    /// A shell command that must exit 0.
    #[serde(default)]
    pub command: Option<String>,
    /// Extra alternatives; the predicate passes when any entry passes.
    #[serde(default)]
    pub any: Vec<When>,
    /// Invert the result.
    #[serde(default)]
    pub not: bool,
}

impl When {
    pub fn is_empty(&self) -> bool {
        self.branch.is_none()
            && self.clean.is_none()
            && self.tag.is_none()
            && self.upstream.is_none()
            && self.ahead.is_none()
            && self.env.is_empty()
            && self.var.is_empty()
            && self.command.is_none()
            && self.any.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Repository facts
// ---------------------------------------------------------------------------

/// A snapshot of repository state, collected once per run and on refresh.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct GitFacts {
    pub root: PathBuf,
    pub in_repo: bool,
    pub branch: Option<String>,
    pub sha: String,
    pub short_sha: String,
    /// `owner/name` parsed from the remote URL, when recognisable.
    pub repo: Option<String>,
    pub remote: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub dirty: bool,
    pub staged: u32,
    pub unstaged: u32,
    pub untracked: u32,
    pub latest_tag: Option<String>,
    pub tags: Vec<String>,
    /// Default branch of the remote, when it can be determined.
    pub base_branch: Option<String>,
}

impl GitFacts {
    pub fn branch_or(&self, fallback: &str) -> String {
        self.branch.clone().unwrap_or_else(|| fallback.to_string())
    }

    pub fn display_name(&self) -> String {
        self.repo
            .clone()
            .or_else(|| {
                self.root
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "repository".to_string())
    }

    pub fn is_clean(&self) -> bool {
        !self.dirty
    }

    pub fn has_upstream(&self) -> bool {
        self.upstream.is_some()
    }
}

/// Everything a template can read during a run.
#[derive(Debug, Clone, Default)]
pub struct Ctx {
    pub vars: BTreeMap<String, String>,
    pub env: BTreeMap<String, String>,
    pub facts: GitFacts,
    pub profile: Option<String>,
    pub dry_run: bool,
}

impl Ctx {
    pub fn new(facts: GitFacts) -> Self {
        Self {
            facts,
            ..Self::default()
        }
    }

    pub fn with_env(mut self, env: BTreeMap<String, String>) -> Self {
        self.env.extend(env);
        self
    }

    pub fn with_vars(mut self, vars: BTreeMap<String, String>) -> Self {
        self.vars.extend(vars);
        self
    }

    /// Resolve a dotted template path. Order: explicit namespaces, then vars,
    /// then well-known aliases, then the process environment.
    pub fn get(&self, path: &str) -> Option<String> {
        let path = path.trim();
        if let Some(rest) = path.strip_prefix("env.") {
            return self.env.get(rest).cloned();
        }
        if let Some(rest) = path.strip_prefix("vars.") {
            return self.vars.get(rest).cloned();
        }
        if let Some(rest) = path.strip_prefix("git.") {
            return match rest {
                "root" => Some(self.facts.root.display().to_string()),
                "branch" => self.facts.branch.clone(),
                "sha" => Some(self.facts.sha.clone()),
                "short_sha" => Some(self.facts.short_sha.clone()),
                "repo" => self.facts.repo.clone(),
                "remote" => self.facts.remote.clone(),
                "upstream" => self.facts.upstream.clone(),
                "latest_tag" => self.facts.latest_tag.clone(),
                "base_branch" => self.facts.base_branch.clone(),
                "dirty" => Some(self.facts.dirty.to_string()),
                "ahead" => Some(self.facts.ahead.to_string()),
                "behind" => Some(self.facts.behind.to_string()),
                _ => None,
            };
        }
        if path == "date" {
            return Some(chrono::Local::now().format("%Y-%m-%d").to_string());
        }
        if path == "time" {
            return Some(chrono::Local::now().format("%H:%M:%S").to_string());
        }
        if path == "datetime" {
            return Some(chrono::Local::now().format("%Y-%m-%d %H:%M").to_string());
        }
        if path == "profile" {
            return self.profile.clone();
        }

        if let Some(value) = self.vars.get(path) {
            return Some(value.clone());
        }

        match path {
            "branch" => self.facts.branch.clone(),
            "sha" => Some(self.facts.sha.clone()),
            "short_sha" => Some(self.facts.short_sha.clone()),
            "repo" => self.facts.repo.clone(),
            "version" => self.vars.get("version").cloned(),
            _ => self.env.get(path).cloned(),
        }
    }

    /// Variable names a user could reference, for error messages.
    pub fn known_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.vars.keys().cloned().collect();
        names.extend([
            "branch".into(),
            "sha".into(),
            "short_sha".into(),
            "repo".into(),
            "version".into(),
            "date".into(),
            "time".into(),
            "datetime".into(),
            "profile".into(),
            "git.branch".into(),
            "git.sha".into(),
            "git.short_sha".into(),
            "git.repo".into(),
            "git.upstream".into(),
            "git.latest_tag".into(),
            "git.base_branch".into(),
        ]);
        names.sort();
        names.dedup();
        names
    }
}

/// Display context for the header line and plan view.
#[derive(Debug, Clone, Default)]
pub struct Session {
    pub config_path: Option<PathBuf>,
    pub root: PathBuf,
    pub facts: GitFacts,
    pub profile: Option<String>,
}

impl Session {
    pub fn root_or_cwd(cwd: &Path) -> Self {
        Self {
            root: cwd.to_path_buf(),
            ..Self::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Engine events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Pending,
    Running,
    Ok,
    Failed,
    Skipped,
    Cancelled,
    /// Dry run: the step would have run.
    Planned,
}

impl StepStatus {
    pub fn is_terminal(self) -> bool {
        !matches!(self, StepStatus::Pending | StepStatus::Running)
    }

    pub fn glyph(self) -> &'static str {
        match self {
            StepStatus::Pending => "·",
            StepStatus::Running => "▶",
            StepStatus::Ok => "✓",
            StepStatus::Failed => "✗",
            StepStatus::Skipped => "–",
            StepStatus::Cancelled => "⊘",
            StepStatus::Planned => "◇",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Ok,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub enum EngineEvent {
    RunStarted {
        pipeline: String,
        dry_run: bool,
        steps: usize,
    },
    StepStarted {
        index: usize,
        label: String,
        detail: String,
        command: Option<String>,
    },
    Output {
        index: usize,
        stream: Stream,
        line: String,
    },
    StepFinished {
        index: usize,
        status: StepStatus,
        duration_ms: u64,
        summary: String,
    },
    RunFinished {
        status: RunStatus,
        duration_ms: u64,
        error: Option<String>,
    },
    Confirm {
        index: usize,
        prompt: String,
    },
    InteractiveStart {
        index: usize,
    },
    InteractiveEnd {
        index: usize,
    },
    Note {
        level: NoteLevel,
        message: String,
    },
}

/// A rendered description of what a step will do.
#[derive(Debug, Clone, Default)]
pub struct Preview {
    /// Short label for the step row, e.g. `origin/main`.
    pub detail: String,
    /// The command that will run, if this step maps to one.
    pub command: Option<String>,
}
