//! Git facts and command helpers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::model::GitFacts;

/// A fully specified child process.
#[derive(Debug, Clone)]
pub struct CmdSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub timeout: Option<Duration>,
    /// Run with the terminal attached instead of piping output.
    pub inherit_stdio: bool,
    /// Echo the command before running it.
    pub print_command: bool,
    /// Capture output without echoing it to the step log. Used for inspection.
    pub quiet: bool,
}

impl CmdSpec {
    pub fn new(program: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: cwd.into(),
            env: BTreeMap::new(),
            timeout: None,
            inherit_stdio: false,
            print_command: true,
            quiet: false,
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// A shell-quoted rendering for display and dry runs.
    pub fn display(&self) -> String {
        let mut parts = vec![self.program.clone()];
        parts.extend(self.args.iter().map(|arg| quote(arg)));
        parts.join(" ")
    }
}

fn quote(arg: &str) -> String {
    let safe = !arg.is_empty()
        && arg.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '-' | '_' | '.' | '/' | ':' | '@' | '=' | '+' | ',')
        });
    if safe {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

#[derive(Debug, Clone, Default)]
pub struct CmdResult {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub cancelled: bool,
}

impl CmdResult {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    pub fn combined(&self) -> String {
        let mut out = self.stdout.clone();
        if !self.stderr.trim().is_empty() {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&self.stderr);
        }
        out
    }
}

/// The repository root that contains `start`, if any.
pub fn discover_root(start: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(start)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if root.is_empty() {
        None
    } else {
        Some(PathBuf::from(root))
    }
}

/// Run a git command to completion and capture its output.
pub fn run_git(root: &Path, args: &[&str]) -> Result<CmdResult> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))?;
    Ok(CmdResult {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        timed_out: false,
        cancelled: false,
    })
}

fn git_stdout(root: &Path, args: &[&str]) -> Option<String> {
    let out = run_git(root, args).ok()?;
    if !out.success() {
        return None;
    }
    Some(out.stdout.trim_end().to_string())
}

/// Collect a full snapshot of repository state.
///
/// Every field is best-effort: a detached HEAD, an empty repo, or a missing
/// remote yields a default rather than an error.
pub fn facts(root: &Path, remote_hint: &str) -> Result<GitFacts> {
    let mut facts = GitFacts {
        root: root.to_path_buf(),
        in_repo: true,
        ..GitFacts::default()
    };

    let remotes = git_stdout(root, &["remote"]).unwrap_or_default();
    let remote_names: Vec<&str> = remotes.lines().filter(|l| !l.is_empty()).collect();
    let remote = if remote_names.contains(&remote_hint) {
        Some(remote_hint.to_string())
    } else {
        remote_names.first().map(|name| (*name).to_string())
    };
    facts.remote = remote.clone();

    // `rev-parse --abbrev-ref HEAD` fails on a repository with no commits, so
    // fall back to the symbolic ref: a fresh `git init` still has a branch.
    let branch = git_stdout(root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .filter(|branch| branch != "HEAD" && !branch.is_empty())
        .or_else(|| git_stdout(root, &["symbolic-ref", "--short", "HEAD"]))
        .filter(|branch| !branch.is_empty());
    facts.branch = branch;

    if let Some(sha) = git_stdout(root, &["rev-parse", "HEAD"]) {
        facts.short_sha = sha.chars().take(7).collect();
        facts.sha = sha;
    }

    if let Some(remote_name) = &remote {
        if let Some(url) = git_stdout(root, &["remote", "get-url", remote_name]) {
            facts.repo = parse_repo_slug(&url);
        }
        if let Some(head) = git_stdout(
            root,
            &[
                "symbolic-ref",
                "--quiet",
                &format!("refs/remotes/{remote_name}/HEAD"),
            ],
        ) {
            if let Some(base) = head.rsplit('/').next() {
                facts.base_branch = Some(base.to_string());
            }
        }
    }
    if facts.base_branch.is_none() {
        facts.base_branch = Some("main".to_string());
    }

    if let Some(upstream) = git_stdout(
        root,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    ) {
        if !upstream.is_empty() {
            facts.upstream = Some(upstream);
        }
    }

    if facts.upstream.is_some() {
        if let Some(counts) = git_stdout(
            root,
            &["rev-list", "--left-right", "--count", "HEAD...@{u}"],
        ) {
            let mut parts = counts.split_whitespace();
            facts.ahead = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            facts.behind = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        }
    }

    if let Some(status) = git_stdout(root, &["status", "--porcelain"]) {
        for line in status.lines() {
            let mut chars = line.chars();
            let x = chars.next().unwrap_or(' ');
            let y = chars.next().unwrap_or(' ');
            if x == '?' && y == '?' {
                facts.untracked += 1;
                continue;
            }
            if x != ' ' && x != '!' {
                facts.staged += 1;
            }
            if y != ' ' && y != '!' {
                facts.unstaged += 1;
            }
        }
        facts.dirty = facts.staged + facts.unstaged + facts.untracked > 0;
    }

    if let Some(tags) = git_stdout(root, &["tag", "--sort=-creatordate"]) {
        facts.tags = tags
            .lines()
            .filter(|line| !line.is_empty())
            .take(200)
            .map(str::to_string)
            .collect();
        facts.latest_tag = facts.tags.first().cloned();
    }

    Ok(facts)
}

/// Extract `owner/repo` from a GitHub remote URL.
pub fn parse_repo_slug(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches('/');
    let host = "github.com";
    let (_, tail) = trimmed.split_once(host)?;
    let tail = tail.trim_start_matches(['/', ':']);
    let tail = tail.strip_suffix(".git").unwrap_or(tail);
    let mut parts = tail.split('/').filter(|p| !p.is_empty());
    let owner = parts.next()?;
    let repo = parts.next()?;
    Some(format!("{owner}/{repo}"))
}

/// Run a shell predicate for `when.command`, returning whether it exited 0.
pub fn shell_ok(command: &str, cwd: &Path) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_https_remote() {
        assert_eq!(
            parse_repo_slug("https://github.com/allisonhere/tug.git").as_deref(),
            Some("allisonhere/tug")
        );
    }

    #[test]
    fn parses_ssh_remote() {
        assert_eq!(
            parse_repo_slug("git@github.com:allisonhere/tug.git").as_deref(),
            Some("allisonhere/tug")
        );
    }

    #[test]
    fn parses_ssh_url_remote() {
        assert_eq!(
            parse_repo_slug("ssh://git@github.com/allisonhere/tug").as_deref(),
            Some("allisonhere/tug")
        );
    }

    #[test]
    fn rejects_non_github() {
        assert_eq!(parse_repo_slug("https://gitlab.com/a/b.git"), None);
    }

    #[test]
    fn an_empty_repository_still_has_a_branch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let status = std::process::Command::new("git")
            .args(["init", "-q", "-b", "trunk"])
            .current_dir(dir.path())
            .status()
            .expect("git init");
        assert!(status.success());

        let facts = facts(dir.path(), "origin").expect("facts");
        assert_eq!(facts.branch.as_deref(), Some("trunk"));
        assert!(facts.sha.is_empty());
        assert!(facts.in_repo);
    }

    #[test]
    fn quotes_arguments() {
        let spec = CmdSpec::new("git", "/tmp")
            .arg("commit")
            .arg("-m")
            .arg("fix: two words");
        assert_eq!(spec.display(), "git commit -m 'fix: two words'");
    }
}
