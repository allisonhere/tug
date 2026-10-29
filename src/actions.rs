//! One executor and one preview per [`Action`].
//!
//! Actions never touch the terminal or spawn processes directly: every side
//! effect goes through [`Exec`], which the engine implements. That keeps dry-run,
//! timeouts, cancellation, and output capture in one place and makes actions
//! unit-testable with a fake executor.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::condition;
use crate::git::{CmdResult, CmdSpec};
use crate::github::{self, PrRequest, ReleaseRequest};
use crate::model::{Action, BumpPart, Ctx, Defaults, Force, NoteLevel, Preview};
use crate::template;

/// What an action needs from the engine.
pub trait Exec {
    fn ctx(&self) -> &Ctx;
    /// Repository root.
    fn root(&self) -> &Path;
    /// Working directory for this step (root unless overridden).
    fn cwd(&self) -> &Path;
    fn defaults(&self) -> &Defaults;
    /// When true, mutating work is described rather than performed.
    fn dry_run(&self) -> bool;
    fn cancelled(&self) -> bool;
    /// Run obeying dry-run.
    fn run(&mut self, spec: CmdSpec) -> Result<CmdResult>;
    /// Run even during a dry run. Only for inspection.
    fn run_readonly(&mut self, spec: CmdSpec) -> Result<CmdResult>;
    fn note(&mut self, level: NoteLevel, message: &str);
    fn set_var(&mut self, key: &str, value: String);
    /// Ask the user to approve something. Returns false when declined.
    fn confirm(&mut self, prompt: &str) -> Result<bool>;
    /// Emit a line into the step log.
    fn line(&mut self, text: &str);
}

#[derive(Debug, Clone, Default)]
pub struct Executed {
    /// Short text shown in the step summary column.
    pub summary: String,
    /// Combined output, used for `capture` and failure context.
    pub output: String,
}

impl Executed {
    fn new(summary: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            output: output.into(),
        }
    }
}

/// Perform one action. Errors are step failures.
pub fn execute(action: &Action, exec: &mut dyn Exec) -> Result<Executed> {
    match action {
        Action::Stage { paths, all, update } => stage(exec, paths, *all, *update),
        Action::Unstage { paths, all } => unstage(exec, paths, *all),
        Action::Commit {
            message,
            all,
            allow_empty,
            amend,
            signoff,
            no_verify,
        } => commit(
            exec,
            message,
            *all,
            *allow_empty,
            *amend,
            *signoff,
            *no_verify,
        ),
        Action::Push {
            remote,
            branch,
            set_upstream,
            force,
            tags,
        } => push(
            exec,
            remote.as_deref(),
            branch.as_deref(),
            *set_upstream,
            *force,
            *tags,
        ),
        Action::Fetch {
            remote,
            prune,
            tags,
        } => fetch(exec, remote.as_deref(), *prune, *tags),
        Action::Checkout { target, create } => checkout(exec, target, *create),
        Action::Tag {
            name,
            message,
            force,
            sign,
            push,
            remote,
        } => tag(
            exec,
            name,
            message.as_deref(),
            *force,
            *sign,
            *push,
            remote.as_deref(),
        ),
        Action::Pr {
            title,
            body,
            base,
            draft,
            update,
            reviewers,
            labels,
        } => pull_request(
            exec,
            title.as_deref(),
            body.as_deref(),
            base.as_deref(),
            *draft,
            *update,
            reviewers,
            labels,
        ),
        Action::Release {
            tag,
            title,
            notes,
            notes_file,
            draft,
            prerelease,
            generate_notes,
            assets,
            update,
        } => release(
            exec,
            tag,
            title.as_deref(),
            notes.as_deref(),
            notes_file.as_deref(),
            *draft,
            *prerelease,
            *generate_notes,
            assets,
            *update,
        ),
        Action::Watch {
            timeout_secs,
            interval_secs,
        } => watch(exec, *timeout_secs, *interval_secs),
        Action::Shell {
            run,
            interactive,
            print_command,
        } => shell(exec, run, *interactive, *print_command),
        Action::Set { key, value } => set(exec, key, value),
        Action::Bump { part, var, tag } => bump(exec, *part, var.as_deref(), tag.as_deref()),
        Action::Confirm { prompt } => confirm(exec, prompt),
        Action::Sleep { ms } => sleep(exec, *ms),
    }
}

// ---------------------------------------------------------------------------
// git actions
// ---------------------------------------------------------------------------

fn stage(exec: &mut dyn Exec, paths: &[String], all: bool, update: bool) -> Result<Executed> {
    let mut spec = CmdSpec::new("git", exec.cwd()).arg("add");
    if update {
        spec = spec.arg("-u");
    } else if all || paths.is_empty() {
        spec = spec.arg("-A");
    } else {
        spec = spec.arg("--").args(paths.iter().cloned());
    }

    if exec.dry_run() {
        return Ok(Executed::new("stage changes", spec.display()));
    }

    let result = checked(exec, spec, "git add")?;
    let staged = exec.run_readonly(CmdSpec::new("git", exec.cwd()).args([
        "diff",
        "--cached",
        "--name-only",
    ]))?;
    let count = staged
        .stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    let summary = match count {
        0 => "nothing to stage".to_string(),
        1 => "1 file staged".to_string(),
        n => format!("{n} files staged"),
    };
    Ok(Executed::new(summary, result.combined()))
}

fn unstage(exec: &mut dyn Exec, paths: &[String], all: bool) -> Result<Executed> {
    let mut spec = CmdSpec::new("git", exec.cwd()).arg("reset");
    if all || paths.is_empty() {
        spec = spec.args(["--", "."]);
    } else {
        spec = spec.arg("--").args(paths.iter().cloned());
    }
    if exec.dry_run() {
        return Ok(Executed::new("unstage changes", spec.display()));
    }
    let result = checked(exec, spec, "git reset")?;
    Ok(Executed::new("unstaged", result.combined()))
}

fn commit(
    exec: &mut dyn Exec,
    message: &str,
    all: bool,
    allow_empty: bool,
    amend: bool,
    signoff: bool,
    no_verify: bool,
) -> Result<Executed> {
    let message = render(exec, message)?;
    if message.trim().is_empty() {
        bail!("commit message is empty after template rendering");
    }

    let mut spec = CmdSpec::new("git", exec.cwd()).args(["commit", "-m", &message]);
    if all {
        spec = spec.arg("--all");
    }
    if allow_empty {
        spec = spec.arg("--allow-empty");
    }
    if amend {
        spec = spec.arg("--amend");
    }
    if signoff {
        spec = spec.arg("--signoff");
    }
    if no_verify {
        spec = spec.arg("--no-verify");
    }

    if exec.dry_run() {
        return Ok(Executed::new(first_line(&message), spec.display()));
    }

    let result = checked(exec, spec, "git commit")?;
    let sha = short_sha(exec).unwrap_or_default();
    let summary = if sha.is_empty() {
        first_line(&message).to_string()
    } else {
        format!("{sha} {}", truncate(first_line(&message), 48))
    };
    Ok(Executed::new(summary, result.combined()))
}

fn push(
    exec: &mut dyn Exec,
    remote: Option<&str>,
    branch: Option<&str>,
    set_upstream: bool,
    force: Force,
    tags: bool,
) -> Result<Executed> {
    let remote = remote
        .map(str::to_string)
        .unwrap_or_else(|| exec.defaults().remote.clone());
    let branch = branch
        .map(|b| render(exec, b))
        .transpose()?
        .or_else(|| exec.ctx().facts.branch.clone())
        .unwrap_or_else(|| "HEAD".to_string());

    let mut spec = CmdSpec::new("git", exec.cwd()).args(["push"]);
    if set_upstream && force == Force::No {
        spec = spec.arg("--set-upstream");
    }
    match force {
        Force::No => {}
        Force::WithLease => spec = spec.arg("--force-with-lease"),
        Force::Force => spec = spec.arg("--force"),
    }
    if tags {
        spec = spec.arg("--tags");
    }
    spec = spec.args([&remote, &branch]);

    if exec.dry_run() {
        return Ok(Executed::new(format!("{remote}/{branch}"), spec.display()));
    }

    let result = checked(exec, spec, "git push")?;
    Ok(Executed::new(
        format!("{remote}/{branch}"),
        result.combined(),
    ))
}

fn fetch(exec: &mut dyn Exec, remote: Option<&str>, prune: bool, tags: bool) -> Result<Executed> {
    let remote = remote.unwrap_or(&exec.defaults().remote).to_string();
    let mut spec = CmdSpec::new("git", exec.cwd()).args(["fetch", &remote]);
    if prune {
        spec = spec.arg("--prune");
    }
    if tags {
        spec = spec.arg("--tags");
    }
    if exec.dry_run() {
        return Ok(Executed::new(remote, spec.display()));
    }
    let result = checked(exec, spec, "git fetch")?;
    Ok(Executed::new(remote, result.combined()))
}

fn checkout(exec: &mut dyn Exec, target: &str, create: bool) -> Result<Executed> {
    let target = render(exec, target)?;
    let mut spec = CmdSpec::new("git", exec.cwd()).arg("checkout");
    if create {
        spec = spec.arg("-b");
    }
    spec = spec.arg(&target);

    if exec.dry_run() {
        return Ok(Executed::new(target, spec.display()));
    }
    let result = checked(exec, spec, "git checkout")?;
    Ok(Executed::new(target, result.combined()))
}

fn tag(
    exec: &mut dyn Exec,
    name: &str,
    message: Option<&str>,
    force: bool,
    sign: bool,
    also_push: bool,
    remote: Option<&str>,
) -> Result<Executed> {
    let name = render(exec, name)?;
    let message = message.map(|m| render(exec, m)).transpose()?;

    let mut spec = CmdSpec::new("git", exec.cwd()).arg("tag");
    if let Some(message) = &message {
        spec = spec.args(["-a", "-m", message]);
    }
    if sign {
        spec = spec.arg("-s");
    }
    if force {
        spec = spec.arg("-f");
    }
    spec = spec.arg(&name);

    if exec.dry_run() {
        return Ok(Executed::new(name, spec.display()));
    }

    let mut output = checked(exec, spec, "git tag")?.combined();

    if also_push {
        let remote = remote
            .map(str::to_string)
            .unwrap_or_else(|| exec.defaults().remote.clone());
        let refspec = format!("refs/tags/{name}");
        let push = CmdSpec::new("git", exec.cwd()).args(["push", &remote, &refspec]);
        output.push_str(&checked(exec, push, "git push tag")?.combined());
    }

    Ok(Executed::new(name, output))
}

// ---------------------------------------------------------------------------
// github actions
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn pull_request(
    exec: &mut dyn Exec,
    title: Option<&str>,
    body: Option<&str>,
    base: Option<&str>,
    draft: bool,
    update: bool,
    reviewers: &[String],
    labels: &[String],
) -> Result<Executed> {
    let branch = exec
        .ctx()
        .facts
        .branch
        .clone()
        .context("cannot open a pull request from a detached HEAD")?;

    let request = PrRequest {
        branch: branch.clone(),
        title: title.map(|t| render(exec, t)).transpose()?,
        body: body.map(|b| render(exec, b)).transpose()?,
        base: Some(
            base.map(str::to_string)
                .or_else(|| exec.defaults().base_branch.clone())
                .or_else(|| exec.ctx().facts.base_branch.clone())
                .unwrap_or_else(|| "main".to_string()),
        )
        .map(|b| render(exec, &b))
        .transpose()?,
        draft,
        reviewers: reviewers.to_vec(),
        labels: labels.to_vec(),
    };

    let root = exec.root().to_path_buf();
    let existing = exec.run_readonly(github::find_pr_spec(&root, &branch))?;
    let existing = if existing.success() {
        github::parse_pr_list(&existing.stdout)?
    } else {
        None
    };

    let (spec, action) = match &existing {
        Some(pr) if update => (github::edit_pr_spec(&root, pr.number, &request), "updated"),
        Some(pr) => bail!(
            "pull request #{} already exists for {branch}; set update = true to edit it",
            pr.number
        ),
        None => (github::create_pr_spec(&root, &request), "created"),
    };

    if exec.dry_run() {
        return Ok(Executed::new(
            format!("{action} PR {branch}"),
            spec.display(),
        ));
    }

    let result = checked(exec, spec, "gh pr")?;
    let url = github::first_url(&result.stdout)
        .or_else(|| existing.as_ref().map(|pr| pr.url.clone()))
        .unwrap_or_default();

    let summary = match &existing {
        Some(pr) => format!("PR #{} {action}", pr.number),
        None => format!("PR {action}"),
    };

    Ok(Executed::new(
        summary,
        format!("{url}\n{}", result.combined()),
    ))
}

#[allow(clippy::too_many_arguments)]
fn release(
    exec: &mut dyn Exec,
    tag: &str,
    title: Option<&str>,
    notes: Option<&str>,
    notes_file: Option<&str>,
    draft: bool,
    prerelease: bool,
    generate_notes: bool,
    assets: &[String],
    update: bool,
) -> Result<Executed> {
    let tag = render(exec, tag)?;
    let request = ReleaseRequest {
        tag: tag.clone(),
        title: title
            .map(|t| render(exec, t))
            .transpose()?
            .or_else(|| Some(tag.clone())),
        notes: notes.map(|n| render(exec, n)).transpose()?,
        notes_file: notes_file.map(|n| render(exec, n)).transpose()?,
        generate_notes,
        draft,
        prerelease,
        assets: assets
            .iter()
            .map(|a| render(exec, a))
            .collect::<Result<Vec<_>>>()?,
    };

    let root = exec.root().to_path_buf();
    let existing = exec.run_readonly(github::release_view_spec(&root, &tag))?;

    if existing.success() && !update {
        bail!("release {tag} already exists and update = false");
    }

    let mut output = String::new();
    let mut summary = format!("release {tag}");

    if existing.success() {
        let spec = github::release_edit_spec(&root, &request);
        if exec.dry_run() {
            return Ok(Executed::new(format!("update {tag}"), spec.display()));
        }
        output.push_str(&checked(exec, spec, "gh release edit")?.combined());
        summary = format!("release {tag} updated");
    } else {
        let spec = github::release_create_spec(&root, &request);
        if exec.dry_run() {
            return Ok(Executed::new(format!("create {tag}"), spec.display()));
        }
        let result = checked(exec, spec, "gh release create")?;
        if let Some(url) = github::first_url(&result.stdout) {
            summary = format!("release {tag} — {url}");
        } else {
            summary = format!("release {tag} created");
        }
        output.push_str(&result.combined());
    }

    if !request.assets.is_empty() && !exec.dry_run() {
        let spec = github::release_upload_spec(&root, &tag, &request.assets);
        output.push_str(&checked(exec, spec, "gh release upload")?.combined());
    }

    Ok(Executed::new(summary, output))
}

fn watch(
    exec: &mut dyn Exec,
    timeout_secs: Option<u64>,
    interval_secs: Option<u64>,
) -> Result<Executed> {
    let mut spec = github::checks_spec(exec.root(), interval_secs);
    // Watching is read-only, but it is long-running: honour a timeout.
    spec.timeout = Some(Duration::from_secs(timeout_secs.unwrap_or(3600)));
    if exec.dry_run() {
        return Ok(Executed::new("watch checks", spec.display()));
    }
    let result = checked(exec, spec, "gh pr checks")?;
    Ok(Executed::new("checks passed", result.combined()))
}

// ---------------------------------------------------------------------------
// local actions
// ---------------------------------------------------------------------------

fn shell(
    exec: &mut dyn Exec,
    run: &str,
    interactive: bool,
    print_command: bool,
) -> Result<Executed> {
    let command = render(exec, run)?;

    let (program, mut args) = match &exec.defaults().shell {
        Some(shell) => {
            let mut parts = shell.split_whitespace();
            let program = parts.next().unwrap_or("sh").to_string();
            (program, parts.map(str::to_string).collect::<Vec<_>>())
        }
        None => ("sh".to_string(), Vec::new()),
    };
    if program == "sh" && args.is_empty() {
        args = vec!["-c".to_string(), command.clone()];
    } else {
        args.push(command.clone());
    }

    let mut spec = CmdSpec::new(program, exec.cwd()).args(args);
    spec.inherit_stdio = interactive;
    spec.print_command = print_command;

    if exec.dry_run() {
        return Ok(Executed::new(truncate(&command, 60), spec.display()));
    }
    if exec.cancelled() {
        bail!("cancelled");
    }

    let result = checked(exec, spec, "shell command")?;
    Ok(Executed::new(truncate(&command, 60), result.combined()))
}

fn set(exec: &mut dyn Exec, key: &str, value: &str) -> Result<Executed> {
    let value = render(exec, value)?;
    exec.set_var(key, value.clone());
    Ok(Executed::new(format!("{key} = {value}"), String::new()))
}

fn bump(
    exec: &mut dyn Exec,
    part: crate::model::BumpPart,
    var: Option<&str>,
    tag_glob: Option<&str>,
) -> Result<Executed> {
    let prefix = exec.defaults().tag_prefix.clone();
    let pattern = tag_glob
        .map(str::to_string)
        .unwrap_or_else(|| format!("{prefix}*"));

    let out = exec.run_readonly(CmdSpec::new("git", exec.cwd()).args([
        "tag",
        "--list",
        &pattern,
        "--sort=-v:refname",
    ]))?;
    let candidate = out
        .stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string);

    let (version, note) = match candidate.as_deref() {
        Some(tag) => {
            let current = parse_version(tag, &prefix)
                .with_context(|| format!("cannot parse version from tag {tag:?}"))?;
            (bump_version(current, part), String::new())
        }
        None => (
            bump_version((0, 0, 0), part),
            " (no matching tags, starting from 0.0.0)".to_string(),
        ),
    };

    let formatted = format!("{}.{}.{}", version.0, version.1, version.2);
    let name = var
        .map(str::to_string)
        .unwrap_or_else(|| exec.defaults().version_var.clone());
    exec.set_var(&name, formatted.clone());
    exec.set_var("version_tag", format!("{prefix}{formatted}"));

    Ok(Executed::new(
        format!("{name} {formatted}{note}"),
        String::new(),
    ))
}

fn confirm(exec: &mut dyn Exec, prompt: &str) -> Result<Executed> {
    let prompt = render(exec, prompt)?;
    if exec.dry_run() {
        return Ok(Executed::new(truncate(&prompt, 60), String::new()));
    }
    if exec.confirm(&prompt)? {
        Ok(Executed::new("confirmed", String::new()))
    } else {
        bail!("declined by user")
    }
}

fn sleep(exec: &mut dyn Exec, ms: u64) -> Result<Executed> {
    let total = Duration::from_millis(ms);
    let mut waited = Duration::ZERO;
    while waited < total {
        if exec.cancelled() {
            bail!("cancelled");
        }
        let slice = Duration::from_millis(50).min(total - waited);
        std::thread::sleep(slice);
        waited += slice;
    }
    Ok(Executed::new(format!("{ms} ms"), String::new()))
}

// ---------------------------------------------------------------------------
// previews
// ---------------------------------------------------------------------------

/// The version a `bump` step would produce, resolved from the tag snapshot.
///
/// The real step asks git so it sees tags created mid-run; planning uses the
/// snapshot already collected for this run, which is enough to render later
/// templates such as `{{ version_tag }}` in the plan.
fn planned_bump(
    part: BumpPart,
    var: Option<&str>,
    tag: Option<&str>,
    ctx: &Ctx,
    defaults: &Defaults,
) -> (String, Option<String>, String) {
    let prefix = defaults.tag_prefix.as_str();
    let pattern = tag
        .map(str::to_string)
        .unwrap_or_else(|| format!("{prefix}*"));
    let current = ctx
        .facts
        .tags
        .iter()
        .filter(|candidate| condition::glob_match(&pattern, candidate))
        .filter_map(|candidate| parse_version(candidate, prefix).ok())
        .max();

    let next = bump_version(current.unwrap_or((0, 0, 0)), part);
    let rendered = format!("{}.{}.{}", next.0, next.1, next.2);
    let name = var
        .map(str::to_string)
        .unwrap_or_else(|| defaults.version_var.clone());
    (
        name,
        current.map(|(major, minor, patch)| format!("{major}.{minor}.{patch}")),
        rendered,
    )
}

/// Apply the in-memory effect of `set` and `bump` while planning.
///
/// Without this, a plan for a release pipeline would show raw
/// `{{ version_tag }}` placeholders for every later step.
pub fn apply_plan_vars(action: &Action, ctx: &mut Ctx, defaults: &Defaults) {
    match action {
        Action::Set { key, value } => {
            if let Ok(value) = template::render(value, ctx, defaults.strict_templates) {
                ctx.vars.insert(key.clone(), value);
            }
        }
        Action::Bump { part, var, tag } => {
            let (name, _, next) =
                planned_bump(*part, var.as_deref(), tag.as_deref(), ctx, defaults);
            let tag = format!("{}{next}", defaults.tag_prefix);
            ctx.vars.insert(name, next);
            ctx.vars.insert("version_tag".into(), tag);
        }
        _ => {}
    }
}

/// Describe what an action would do, resolving templates.
pub fn preview(action: &Action, ctx: &Ctx, defaults: &Defaults) -> Result<Preview> {
    let render = |text: &str| template::render(text, ctx, defaults.strict_templates);
    let root = ctx.facts.root.clone();

    let preview = match action {
        Action::Stage { paths, all, update } => {
            let mut spec = CmdSpec::new("git", &root).arg("add");
            if *update {
                spec = spec.arg("-u");
            } else if *all || paths.is_empty() {
                spec = spec.arg("-A");
            } else {
                spec = spec.arg("--").args(paths.iter().cloned());
            }
            Preview {
                detail: spec.display(),
                command: Some(spec.display()),
            }
        }
        Action::Unstage { paths, all } => {
            let mut spec = CmdSpec::new("git", &root).arg("reset");
            if *all || paths.is_empty() {
                spec = spec.arg("--").arg(".");
            } else {
                spec = spec.arg("--").args(paths.iter().cloned());
            }
            Preview {
                detail: spec.display(),
                command: Some(spec.display()),
            }
        }
        Action::Commit { message, amend, .. } => {
            let message = render(message)?;
            Preview {
                detail: truncate(first_line(&message), 60),
                command: Some(format!(
                    "git commit {}-m '{}'",
                    if *amend { "--amend " } else { "" },
                    first_line(&message)
                )),
            }
        }
        Action::Push {
            remote,
            branch,
            force,
            tags,
            ..
        } => {
            let remote_name = remote.clone().unwrap_or_else(|| defaults.remote.clone());
            let branch = match branch {
                Some(branch) => render(branch)?,
                None => ctx.facts.branch.clone().unwrap_or_else(|| "HEAD".into()),
            };
            let mut command = format!("git push {remote_name} {branch}");
            match force {
                Force::No => {}
                Force::WithLease => command.push_str(" --force-with-lease"),
                Force::Force => command.push_str(" --force"),
            }
            if *tags {
                command.push_str(" --tags");
            }
            Preview {
                detail: format!("{remote_name}/{branch}"),
                command: Some(command),
            }
        }
        Action::Fetch { remote, prune, .. } => {
            let remote_name = remote.clone().unwrap_or_else(|| defaults.remote.clone());
            Preview {
                detail: format!("{remote_name}{}", if *prune { " --prune" } else { "" }),
                command: Some(format!(
                    "git fetch {remote_name}{}",
                    if *prune { " --prune" } else { "" }
                )),
            }
        }
        Action::Checkout { target, create } => Preview {
            detail: render(target)?,
            command: Some(format!(
                "git checkout {}{}",
                if *create { "-b " } else { "" },
                render(target)?
            )),
        },
        Action::Tag { name, message, .. } => {
            let name = render(name)?;
            Preview {
                detail: name.clone(),
                command: Some(match message {
                    Some(message) => format!("git tag -a {} -m '{}'", name, render(message)?),
                    None => format!("git tag {name}"),
                }),
            }
        }
        Action::Pr {
            title, base, draft, ..
        } => {
            let branch = ctx
                .facts
                .branch
                .clone()
                .unwrap_or_else(|| "<branch>".into());
            let title = match title {
                Some(title) => render(title)?,
                None => "<from commits>".to_string(),
            };
            let base = base
                .clone()
                .or_else(|| defaults.base_branch.clone())
                .or_else(|| ctx.facts.base_branch.clone())
                .unwrap_or_else(|| "main".into());
            Preview {
                detail: format!("{branch} → {base}"),
                command: Some(format!(
                    "gh pr create --head {branch} --base {base}{}{} --title '{}'",
                    if *draft { " --draft" } else { "" },
                    if title == "<from commits>" {
                        " --fill"
                    } else {
                        ""
                    },
                    title
                )),
            }
        }
        Action::Release {
            tag,
            draft,
            prerelease,
            ..
        } => {
            let tag = render(tag)?;
            Preview {
                detail: format!(
                    "{tag}{}{}",
                    if *draft { " · draft" } else { "" },
                    if *prerelease { " · prerelease" } else { "" }
                ),
                command: Some(format!("gh release create {tag}")),
            }
        }
        Action::Watch { interval_secs, .. } => Preview {
            detail: "CI checks".to_string(),
            command: Some(format!(
                "gh pr checks --watch{}",
                interval_secs
                    .map(|i| format!(" --interval {i}"))
                    .unwrap_or_default()
            )),
        },
        Action::Shell {
            run,
            interactive: _,
            ..
        } => {
            let command = render(run)?;
            Preview {
                detail: truncate(&command, 70),
                command: Some(command),
            }
        }
        Action::Set { key, value } => Preview {
            detail: format!("{key} = {}", render(value)?),
            command: None,
        },
        Action::Bump { part, var, tag } => {
            let (name, current, next) =
                planned_bump(*part, var.as_deref(), tag.as_deref(), ctx, defaults);
            Preview {
                detail: format!(
                    "bump {name} {} → {next}",
                    current.unwrap_or_else(|| "(no tag)".into())
                ),
                command: None,
            }
        }
        Action::Confirm { prompt } => Preview {
            detail: truncate(&render(prompt)?, 70),
            command: None,
        },
        Action::Sleep { ms } => Preview {
            detail: format!("wait {ms} ms"),
            command: None,
        },
    };

    Ok(preview)
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn render(exec: &mut dyn Exec, input: &str) -> Result<String> {
    template::render(input, exec.ctx(), exec.defaults().strict_templates)
}

fn checked(exec: &mut dyn Exec, spec: CmdSpec, what: &str) -> Result<CmdResult> {
    let display = spec.display();
    let result = exec.run(spec)?;

    if result.cancelled {
        bail!("cancelled");
    }
    if result.timed_out {
        bail!("{what} timed out: {display}");
    }
    if !result.success() {
        let detail = result.stderr.trim();
        let detail = if detail.is_empty() {
            result.stdout.trim()
        } else {
            detail
        };
        let detail = truncate(detail, 400);
        if detail.is_empty() {
            bail!("{what} failed with exit {:?}: {display}", result.code);
        }
        bail!(
            "{what} failed with exit {:?}: {display}\n{detail}",
            result.code
        );
    }

    Ok(result)
}

fn short_sha(exec: &mut dyn Exec) -> Option<String> {
    let result = exec
        .run_readonly(CmdSpec::new("git", exec.cwd()).args(["rev-parse", "--short", "HEAD"]))
        .ok()?;
    if result.success() {
        Some(result.stdout.trim().to_string())
    } else {
        None
    }
}

pub fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("").trim()
}

pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn parse_version(tag: &str, prefix: &str) -> Result<(u64, u64, u64)> {
    let stripped = tag.strip_prefix(prefix).unwrap_or(tag);
    let core = stripped.split(['-', '+']).next().unwrap_or(stripped);
    let mut parts = core.split('.');
    let major = parts.next().unwrap_or("0").parse().unwrap_or(0);
    let minor = parts.next().unwrap_or("0").parse().unwrap_or(0);
    let patch = parts.next().unwrap_or("0").parse().unwrap_or(0);
    Ok((major, minor, patch))
}

fn bump_version(
    (major, minor, patch): (u64, u64, u64),
    part: crate::model::BumpPart,
) -> (u64, u64, u64) {
    use crate::model::BumpPart;
    match part {
        BumpPart::Major => (major + 1, 0, 0),
        BumpPart::Minor => (major, minor + 1, 0),
        BumpPart::Patch => (major, minor, patch + 1),
        BumpPart::None => (major, minor, patch),
    }
}

impl Executed {
    pub fn is_empty(&self) -> bool {
        self.summary.is_empty() && self.output.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_prefixed_version() {
        assert_eq!(parse_version("v1.2.3", "v").unwrap(), (1, 2, 3));
    }

    #[test]
    fn ignores_pre_release_suffix() {
        assert_eq!(parse_version("v1.2.3-rc.1", "v").unwrap(), (1, 2, 3));
    }

    #[test]
    fn bumps_each_part() {
        use crate::model::BumpPart;
        assert_eq!(bump_version((1, 2, 3), BumpPart::Patch), (1, 2, 4));
        assert_eq!(bump_version((1, 2, 3), BumpPart::Minor), (1, 3, 0));
        assert_eq!(bump_version((1, 2, 3), BumpPart::Major), (2, 0, 0));
        assert_eq!(bump_version((1, 2, 3), BumpPart::None), (1, 2, 3));
    }

    #[test]
    fn truncates_on_char_boundaries() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("日本語です", 3), "日本…");
    }

    #[test]
    fn first_line_trims() {
        assert_eq!(first_line("  subject  \nbody"), "subject");
    }
}
