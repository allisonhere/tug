//! Config discovery, layering, validation, and `tug init` scaffolding.

pub mod init;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::model::{Action, BumpPart, Config, ConfirmPolicy, Force, Step, When};
use crate::template;

/// A config plus where it came from and any non-fatal problems found while parsing.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub config: Config,
    pub path: Option<PathBuf>,
    /// Files that contributed, lowest precedence first.
    pub sources: Vec<PathBuf>,
    /// Validation findings. Errors do not stop loading; they stop running.
    pub problems: Vec<Problem>,
}

impl LoadedConfig {
    pub fn empty() -> Self {
        Self {
            config: Config::default(),
            path: None,
            sources: Vec::new(),
            problems: Vec::new(),
        }
    }

    pub fn errors(&self) -> impl Iterator<Item = &Problem> {
        self.problems
            .iter()
            .filter(|p| p.severity == Severity::Error)
    }

    pub fn has_errors(&self) -> bool {
        self.errors().next().is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone)]
pub struct Problem {
    pub severity: Severity,
    /// Dotted path, e.g. `pipelines.release.steps[2].message`.
    pub path: String,
    pub message: String,
}

impl Problem {
    fn error(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            path: path.into(),
            message: message.into(),
        }
    }

    fn warning(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            path: path.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let level = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        let message = self.message.replace('\n', " ");
        if self.path.is_empty() {
            write!(f, "{level}: {message}")
        } else {
            write!(f, "{level}: {}: {message}", self.path)
        }
    }
}

/// Load configuration for `cwd`.
///
/// Precedence, lowest to highest: user config, repo config, `.tug.local.toml`.
/// An explicit `--config` path replaces the search entirely.
pub fn load(explicit: Option<&Path>, cwd: &Path) -> Result<LoadedConfig> {
    let paths: Vec<PathBuf> = match explicit {
        Some(path) => vec![path.to_path_buf()],
        None => candidate_paths(cwd)
            .into_iter()
            .filter(|path| path.exists())
            .collect(),
    };

    let mut merged = toml::Value::Table(toml::map::Map::new());
    let mut sources = Vec::new();

    for path in &paths {
        if !path.exists() {
            continue;
        }
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let value: toml::Value =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        merged = merge(merged, value);
        sources.push(path.clone());
    }

    let mut problems = unknown_keys(&merged);

    let config: Config = if sources.is_empty() {
        Config::default()
    } else {
        match merged.clone().try_into() {
            Ok(config) => config,
            Err(err) => {
                // A typo often breaks deserialisation (a misspelled `message`
                // reads as a missing field). Prefer the human explanation.
                if problems.iter().any(|p| p.severity == Severity::Error) {
                    problems.push(Problem::error(
                        "",
                        format!("could not parse the configuration: {err}"),
                    ));
                    return Ok(LoadedConfig {
                        config: Config::default(),
                        path: sources.last().cloned(),
                        sources,
                        problems,
                    });
                }
                return Err(err).context("deserialising merged configuration");
            }
        }
    };

    problems.extend(validate(&config));

    Ok(LoadedConfig {
        config,
        path: sources.last().cloned(),
        sources,
        problems,
    })
}

fn merge(base: toml::Value, overlay: toml::Value) -> toml::Value {
    match (base, overlay) {
        (toml::Value::Table(mut base), toml::Value::Table(overlay)) => {
            for (key, value) in overlay {
                let merged = match base.remove(&key) {
                    Some(existing) => merge(existing, value),
                    None => value,
                };
                base.insert(key, merged);
            }
            toml::Value::Table(base)
        }
        (_, overlay) => overlay,
    }
}

/// Locations searched when no explicit path is given, lowest precedence first.
pub fn candidate_paths(cwd: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(dir) = dirs::config_dir() {
        paths.push(dir.join("tug").join("config.toml"));
    }
    paths.push(cwd.join("tug.toml"));
    paths.push(cwd.join(".tug.toml"));
    paths.push(cwd.join(".tug.local.toml"));
    paths
}

/// Add a pipeline's variables and environment to an existing context.
///
/// Pipeline values sit below command-line overrides but above config and
/// profile values, matching the engine's run order.
pub fn context_with(
    loaded: &LoadedConfig,
    facts: &crate::model::GitFacts,
    profile: Option<&str>,
    dry_run: bool,
    pipeline: Option<&str>,
    overrides: &std::collections::BTreeMap<String, String>,
) -> crate::model::Ctx {
    let mut ctx = context(loaded, facts, profile, dry_run);
    if let Some(name) = pipeline {
        if let Some(definition) = loaded.config.pipelines.get(name) {
            ctx.vars.extend(definition.vars.clone());
            ctx.env.extend(definition.env.clone());
        }
    }
    ctx.vars.extend(overrides.clone());
    ctx
}

/// Build the template context shared by the plan view, the engine, and the UI.
pub fn context(
    loaded: &LoadedConfig,
    facts: &crate::model::GitFacts,
    profile: Option<&str>,
    dry_run: bool,
) -> crate::model::Ctx {
    use std::collections::BTreeMap;

    let mut vars = loaded.config.vars.clone();
    if let Some(name) = profile {
        if let Some(definition) = loaded.config.profiles.get(name) {
            vars.extend(definition.vars.clone());
        }
    }

    vars.insert("remote".to_string(), loaded.config.defaults.remote.clone());
    vars.insert(
        "tag_prefix".to_string(),
        loaded.config.defaults.tag_prefix.clone(),
    );
    if let Some(base) = loaded
        .config
        .defaults
        .base_branch
        .clone()
        .or_else(|| facts.base_branch.clone())
    {
        vars.insert("base_branch".to_string(), base);
    }
    if let Some(tag) = facts.latest_tag.clone() {
        vars.entry("latest_tag".to_string()).or_insert(tag);
    }

    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    env.extend(loaded.config.env.clone());
    if let Some(name) = profile {
        if let Some(definition) = loaded.config.profiles.get(name) {
            env.extend(definition.env.clone());
        }
    }

    crate::model::Ctx {
        vars,
        env,
        facts: facts.clone(),
        profile: profile.map(str::to_string),
        dry_run,
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

const TOP_KEYS: &[&str] = &[
    "version",
    "defaults",
    "vars",
    "env",
    "ui",
    "pipelines",
    "profiles",
];
const DEFAULTS_KEYS: &[&str] = &[
    "remote",
    "base_branch",
    "tag_prefix",
    "version_var",
    "shell",
    "strict_templates",
    "confirm",
    "timeout_secs",
];
const UI_KEYS: &[&str] = &["theme", "follow_log", "density"];
const PROFILE_KEYS: &[&str] = &["description", "vars", "env", "defaults"];
const PIPELINE_KEYS: &[&str] = &[
    "description",
    "order",
    "when",
    "env",
    "vars",
    "confirm",
    "steps",
];
const STEP_KEYS: &[&str] = &[
    "action",
    "label",
    "when",
    "env",
    "cwd",
    "timeout",
    "retries",
    "continue_on_error",
    "confirm",
    "silent",
    "capture",
];
const CAPTURE_KEYS: &[&str] = &["name", "regex", "group"];
const WHEN_KEYS: &[&str] = &[
    "branch", "clean", "tag", "upstream", "ahead", "env", "var", "command", "any", "not",
];

fn action_keys(action: &str) -> Option<&'static [&'static str]> {
    Some(match action {
        "git.stage" | "stage" => &["paths", "all", "update"],
        "git.unstage" | "unstage" => &["paths", "all"],
        "git.commit" | "commit" => &[
            "message",
            "all",
            "allow_empty",
            "amend",
            "signoff",
            "no_verify",
        ],
        "git.push" | "push" => &["remote", "branch", "set_upstream", "force", "tags"],
        "git.fetch" | "fetch" => &["remote", "prune", "tags"],
        "git.checkout" | "checkout" => &["target", "create"],
        "git.tag" | "tag" => &["name", "message", "force", "sign", "push", "remote"],
        "github.pr" | "pr" => &[
            "title",
            "body",
            "base",
            "draft",
            "update",
            "reviewers",
            "labels",
        ],
        "github.release" | "release" => &[
            "tag",
            "title",
            "notes",
            "notes_file",
            "draft",
            "prerelease",
            "generate_notes",
            "assets",
            "update",
        ],
        "github.watch" | "watch" => &["timeout_secs", "interval_secs"],
        "shell" | "run" => &["run", "interactive", "print_command"],
        "set" => &["key", "value"],
        "bump" => &["part", "var", "tag"],
        "confirm" => &["prompt"],
        "sleep" => &["ms"],
        _ => return None,
    })
}

fn unknown_keys(value: &toml::Value) -> Vec<Problem> {
    let mut problems = Vec::new();
    let Some(table) = value.as_table() else {
        return problems;
    };

    check_keys(table, "", TOP_KEYS, &mut problems);

    if let Some(defaults) = table.get("defaults").and_then(|v| v.as_table()) {
        check_keys(defaults, "defaults", DEFAULTS_KEYS, &mut problems);
    }
    if let Some(ui) = table.get("ui").and_then(|v| v.as_table()) {
        check_keys(ui, "ui", UI_KEYS, &mut problems);
    }
    if let Some(profiles) = table.get("profiles").and_then(|v| v.as_table()) {
        for (name, profile) in profiles {
            if let Some(profile) = profile.as_table() {
                check_keys(
                    profile,
                    &format!("profiles.{name}"),
                    PROFILE_KEYS,
                    &mut problems,
                );
            }
        }
    }

    if let Some(pipelines) = table.get("pipelines").and_then(|v| v.as_table()) {
        for (name, pipeline) in pipelines {
            let path = format!("pipelines.{name}");
            let Some(pipeline) = pipeline.as_table() else {
                continue;
            };
            check_keys(pipeline, &path, PIPELINE_KEYS, &mut problems);

            if let Some(when) = pipeline.get("when") {
                check_when(when, &format!("{path}.when"), &mut problems);
            }

            let Some(steps) = pipeline.get("steps").and_then(|v| v.as_array()) else {
                continue;
            };
            for (index, step) in steps.iter().enumerate() {
                let step_path = format!("{path}.steps[{index}]");
                let Some(step) = step.as_table() else {
                    problems.push(Problem::error(
                        &step_path,
                        "step must be a table, e.g. { action = \"git.stage\" }",
                    ));
                    continue;
                };
                let action = step.get("action").and_then(|v| v.as_str());
                match action.and_then(action_keys) {
                    Some(keys) => {
                        let mut allowed = STEP_KEYS.to_vec();
                        allowed.extend(keys.iter().copied());
                        check_keys(step, &step_path, &allowed, &mut problems);
                    }
                    None => {
                        check_keys(step, &step_path, STEP_KEYS, &mut problems);
                        problems.push(Problem::error(
                            format!("{step_path}.action"),
                            match action {
                                Some(name) => format!("unknown action `{name}`"),
                                None => "missing `action` key".to_string(),
                            },
                        ));
                    }
                }

                if let Some(when) = step.get("when") {
                    check_when(when, &format!("{step_path}.when"), &mut problems);
                }
                if let Some(capture) = step.get("capture").and_then(|v| v.as_table()) {
                    check_keys(
                        capture,
                        &format!("{step_path}.capture"),
                        CAPTURE_KEYS,
                        &mut problems,
                    );
                }
            }
        }
    }

    problems
}

fn check_keys(
    table: &toml::map::Map<String, toml::Value>,
    path: &str,
    allowed: &[&str],
    problems: &mut Vec<Problem>,
) {
    for key in table.keys() {
        if !allowed.contains(&key.as_str()) {
            let at = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            let hint = closest(key, allowed)
                .map(|candidate| format!(" (did you mean `{candidate}`?)"))
                .unwrap_or_default();
            problems.push(Problem::error(at, format!("unknown key `{key}`{hint}")));
        }
    }
}

/// The closest allowed key within a small edit distance.
fn closest<'a>(needle: &str, allowed: &[&'a str]) -> Option<&'a str> {
    let mut best: Option<(usize, &'a str)> = None;
    for candidate in allowed {
        let distance = edit_distance(needle, candidate);
        if distance <= 2 && best.is_none_or(|(best, _)| distance < best) {
            best = Some((distance, candidate));
        }
    }
    best.map(|(_, candidate)| candidate)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let substitution = previous[j] + usize::from(ca != cb);
            current[j + 1] = (previous[j + 1] + 1).min(current[j] + 1).min(substitution);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

fn check_when(when: &toml::Value, path: &str, problems: &mut Vec<Problem>) {
    let Some(table) = when.as_table() else {
        problems.push(Problem::error(path, "`when` must be a table"));
        return;
    };
    check_keys(table, path, WHEN_KEYS, problems);
    if let Some(any) = table.get("any").and_then(|v| v.as_array()) {
        for (index, item) in any.iter().enumerate() {
            check_when(item, &format!("{path}.any[{index}]"), problems);
        }
    }
}

/// Semantic validation of a parsed config.
pub fn validate(config: &Config) -> Vec<Problem> {
    let mut problems = Vec::new();

    if let Some(version) = config.version {
        if version > 1 {
            problems.push(Problem::warning(
                "version",
                format!("config version {version} is newer than tug understands (1)"),
            ));
        }
    }

    if !matches!(config.ui.theme.as_str(), "tide" | "light" | "mono") {
        problems.push(Problem::warning(
            "ui.theme",
            format!(
                "unknown theme `{}`; falling back to `tide` (known: tide, light, mono)",
                config.ui.theme
            ),
        ));
    }

    for (name, pipeline) in &config.pipelines {
        let path = format!("pipelines.{name}");
        if name.trim().is_empty() {
            problems.push(Problem::error(&path, "pipeline name is empty"));
        }
        if pipeline.steps.is_empty() {
            problems.push(Problem::error(&path, "pipeline has no steps"));
        }
        if let Some(when) = &pipeline.when {
            if when.is_empty() {
                problems.push(Problem::warning(
                    format!("{path}.when"),
                    "empty `when` block has no effect",
                ));
            }
        }

        let mut captures = BTreeSet::new();
        for (index, step) in pipeline.steps.iter().enumerate() {
            let step_path = format!("{path}.steps[{index}]");

            if let Some(capture) = &step.capture {
                if !captures.insert(capture.name.clone()) {
                    problems.push(Problem::warning(
                        format!("{step_path}.capture.name"),
                        format!(
                            "capture `{}` is defined earlier in this pipeline",
                            capture.name
                        ),
                    ));
                }
                if let Err(err) = regex::Regex::new(&capture.regex) {
                    problems.push(Problem::error(
                        format!("{step_path}.capture.regex"),
                        format!("invalid regex: {err}"),
                    ));
                }
            }

            problems.extend(check_action_strings(&step.action, &step_path));

            match &step.action {
                Action::Push {
                    force: Force::Force,
                    ..
                } => problems.push(Problem::warning(
                    format!("{step_path}.force"),
                    "force push is configured; use `force-with-lease` unless you mean it",
                )),
                Action::Bump {
                    part: BumpPart::None,
                    ..
                } => problems.push(Problem::warning(
                    format!("{step_path}.part"),
                    "bump part `none` does not change the version",
                )),
                _ => {}
            }

            if step.confirm && !step.action.is_mutating() {
                problems.push(Problem::warning(
                    format!("{step_path}.confirm"),
                    "confirmation on a read-only step is usually unnecessary",
                ));
            }
        }

        if matches!(pipeline.confirm, Some(ConfirmPolicy::Always)) && pipeline.steps.is_empty() {
            problems.push(Problem::warning(
                format!("{path}.confirm"),
                "no steps to confirm",
            ));
        }
    }

    problems
}

fn check_action_strings(action: &Action, path: &str) -> Vec<Problem> {
    let mut problems = Vec::new();
    for (field, text) in action_strings(action) {
        if let Err(err) = template::render(text, &crate::model::Ctx::default(), false) {
            problems.push(Problem::error(
                format!("{path}.{field}"),
                format!("template error: {err}"),
            ));
        }
    }
    problems
}

fn action_strings(action: &Action) -> Vec<(&'static str, &str)> {
    let mut strings = Vec::new();
    match action {
        Action::Commit { message, .. } => strings.push(("message", message.as_str())),
        Action::Push {
            branch: Some(branch),
            ..
        } => strings.push(("branch", branch.as_str())),
        Action::Checkout { target, .. } => strings.push(("target", target.as_str())),
        Action::Tag { name, message, .. } => {
            strings.push(("name", name.as_str()));
            if let Some(message) = message {
                strings.push(("message", message.as_str()));
            }
        }
        Action::Pr {
            title, body, base, ..
        } => {
            if let Some(title) = title {
                strings.push(("title", title.as_str()));
            }
            if let Some(body) = body {
                strings.push(("body", body.as_str()));
            }
            if let Some(base) = base {
                strings.push(("base", base.as_str()));
            }
        }
        Action::Release {
            tag, title, notes, ..
        } => {
            strings.push(("tag", tag.as_str()));
            if let Some(title) = title {
                strings.push(("title", title.as_str()));
            }
            if let Some(notes) = notes {
                strings.push(("notes", notes.as_str()));
            }
        }
        Action::Shell { run, .. } => strings.push(("run", run.as_str())),
        Action::Set { value, .. } => strings.push(("value", value.as_str())),
        Action::Confirm { prompt } => strings.push(("prompt", prompt.as_str())),
        _ => {}
    }
    strings
}

/// Steps whose `when` blocks reference state that is not known at load time.
pub fn when_of(step: &Step) -> Option<&When> {
    step.when.as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).expect("write config");
    }

    #[test]
    fn merge_is_deep_for_tables_and_replaces_arrays() {
        let base: toml::Value = toml::from_str(
            r#"
            [defaults]
            remote = "upstream"
            [pipelines.a]
            steps = [{ action = "set", key = "x", value = "1" }]
            "#,
        )
        .unwrap();
        let overlay: toml::Value = toml::from_str(
            r#"
            [defaults]
            tag_prefix = "rel-"
            [pipelines.a]
            description = "overridden"
            "#,
        )
        .unwrap();
        let merged: Config = merge(base, overlay).try_into().unwrap();
        assert_eq!(merged.defaults.remote, "upstream");
        assert_eq!(merged.defaults.tag_prefix, "rel-");
        assert_eq!(
            merged.pipelines["a"].description.as_deref(),
            Some("overridden")
        );
    }

    #[test]
    fn loads_repo_and_local_layers_in_order() {
        let dir = tempdir();
        write(
            dir.path(),
            "tug.toml",
            r#"
            [defaults]
            remote = "origin"
            [pipelines.push]
            steps = [{ action = "git.push" }]
            "#,
        );
        write(
            dir.path(),
            ".tug.local.toml",
            r#"
            [defaults]
            remote = "fork"
            "#,
        );
        let loaded = load(None, dir.path()).unwrap();
        assert_eq!(loaded.config.defaults.remote, "fork");
        assert!(loaded.config.pipelines.contains_key("push"));
        assert_eq!(loaded.sources.len(), 2);
    }

    #[test]
    fn unknown_step_key_is_an_error() {
        let dir = tempdir();
        write(
            dir.path(),
            "tug.toml",
            r#"
            [pipelines.push]
            steps = [{ action = "git.commit", message = "ok", mesage = "typo" }]
            "#,
        );
        let loaded = load(None, dir.path()).unwrap();
        let problem = loaded
            .problems
            .iter()
            .find(|p| p.message.contains("mesage") || p.message.contains("unknown key"))
            .expect("unknown key problem");
        assert_eq!(problem.severity, Severity::Error);
    }

    #[test]
    fn unknown_action_is_reported() {
        let dir = tempdir();
        write(
            dir.path(),
            "tug.toml",
            r#"
            [pipelines.push]
            steps = [{ action = "git.teleport" }]
            "#,
        );
        let loaded = load(None, dir.path()).unwrap();
        let problem = loaded
            .problems
            .iter()
            .find(|p| p.message.contains("unknown action"))
            .expect("unknown action problem");
        assert_eq!(problem.severity, Severity::Error);
        assert!(loaded.has_errors());
    }

    #[test]
    fn a_typo_beats_the_missing_field_error() {
        let dir = tempdir();
        write(
            dir.path(),
            "tug.toml",
            r#"
            [pipelines.push]
            steps = [{ action = "git.commit", mesage = "typo" }]
            "#,
        );
        let loaded = load(None, dir.path()).unwrap();
        assert!(
            loaded
                .problems
                .iter()
                .any(|p| p.message.contains("unknown key `mesage`")),
            "{:?}",
            loaded.problems
        );
        assert!(loaded.has_errors());
    }

    #[test]
    fn empty_pipeline_is_an_error() {
        let dir = tempdir();
        write(
            dir.path(),
            "tug.toml",
            r#"
            [pipelines.empty]
            description = "nothing here"
            "#,
        );
        let loaded = load(None, dir.path()).unwrap();
        assert!(loaded.has_errors());
    }

    #[test]
    fn template_syntax_is_validated() {
        let dir = tempdir();
        write(
            dir.path(),
            "tug.toml",
            r#"
            [pipelines.push]
            steps = [{ action = "git.commit", message = "{{ broken" }]
            "#,
        );
        let loaded = load(None, dir.path()).unwrap();
        assert!(
            loaded
                .problems
                .iter()
                .any(|p| p.message.contains("template"))
        );
    }

    #[test]
    fn force_push_warns() {
        let dir = tempdir();
        write(
            dir.path(),
            "tug.toml",
            r#"
            [pipelines.push]
            steps = [{ action = "git.push", force = "force" }]
            "#,
        );
        let loaded = load(None, dir.path()).unwrap();
        assert!(
            loaded
                .problems
                .iter()
                .any(|p| p.severity == Severity::Warning && p.message.contains("force push"))
        );
    }

    #[test]
    fn init_template_parses_and_validates_cleanly() {
        let dir = tempdir();
        let body = init::template(dir.path());
        write(dir.path(), "tug.toml", &body);
        let loaded = load(None, dir.path()).unwrap();
        assert!(!loaded.sources.is_empty());
        assert!(!loaded.config.pipelines.is_empty());
        let errors: Vec<_> = loaded.errors().cloned().collect();
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn pipeline_vars_sit_between_config_and_overrides() {
        use crate::model::{GitFacts, Pipeline};
        use std::collections::BTreeMap;

        let mut config = Config::default();
        config.vars.insert("who".into(), "config".into());
        config.pipelines.insert(
            "ship".into(),
            Pipeline {
                vars: BTreeMap::from([("who".into(), "pipeline".into())]),
                ..Pipeline::default()
            },
        );
        let loaded = LoadedConfig {
            config,
            ..LoadedConfig::empty()
        };

        let ctx = context_with(
            &loaded,
            &GitFacts::default(),
            None,
            false,
            Some("ship"),
            &BTreeMap::new(),
        );
        assert_eq!(ctx.get("who").as_deref(), Some("pipeline"));

        let overrides = BTreeMap::from([("who".into(), "override".into())]);
        let ctx = context_with(
            &loaded,
            &GitFacts::default(),
            None,
            false,
            Some("ship"),
            &overrides,
        );
        assert_eq!(ctx.get("who").as_deref(), Some("override"));
    }

    #[test]
    fn unknown_theme_is_a_warning_not_a_failure() {
        let dir = tempdir();
        write(
            dir.path(),
            "tug.toml",
            r#"
            [ui]
            theme = "neon"

            [pipelines.a]
            steps = [{ action = "sleep", ms = 1 }]
            "#,
        );
        let loaded = load(None, dir.path()).unwrap();
        assert!(
            loaded
                .problems
                .iter()
                .any(|p| p.path == "ui.theme" && p.severity == Severity::Warning),
            "{:?}",
            loaded.problems
        );
        assert!(!loaded.has_errors());
    }

    #[test]
    fn the_repository_config_is_valid() {
        let text = include_str!("../../tug.toml");
        let value: toml::Value = toml::from_str(text).expect("parse tug.toml");
        let config: Config = value.clone().try_into().expect("deserialise tug.toml");
        let mut problems = unknown_keys(&value);
        problems.extend(validate(&config));
        let errors: Vec<_> = problems
            .iter()
            .filter(|problem| problem.severity == Severity::Error)
            .collect();
        assert!(errors.is_empty(), "{errors:?}");
        assert!(!config.pipelines.is_empty());
    }
}
