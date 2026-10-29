//! Starter `tug.toml` generation.

use std::path::Path;

const TEMPLATE: &str = r#"# tug.toml — ship pipelines for @NAME@
#
# tug runs each pipeline top to bottom, shows the resolved plan before mutating
# anything, and stops at the first failure.
#
# Every string can use templates:
#   {{ branch }}  {{ sha }}  {{ short_sha }}  {{ repo }}  {{ version }}
#   {{ git.branch }}  {{ git.latest_tag }}  {{ git.base_branch }}
#   {{ env.HOME }}  {{ vars.anything }}  {{ profile }}
#   {{ name | fallback }}   — fallback when the variable is missing
#
# Actions: git.stage, git.unstage, git.commit, git.push, git.fetch,
#          git.checkout, git.tag, github.pr, github.release, github.watch,
#          shell, set, bump, confirm, sleep

version = 1

[defaults]
# Remote used by push / fetch / tag.
remote = "origin"
# Base branch for pull requests. Delete to detect origin/HEAD.
base_branch = "main"
# Prefix for tags and for `bump`.
tag_prefix = "v"
# Ask before a pipeline that mutates something: "always" | "destructive" | "never"
confirm = "destructive"
# strict_templates = true (default) fails on unknown {{ variables }}.

[vars]
# Shared variables, available as {{ vars.name }} or {{ name }}.
# channel = "stable"

# ---------------------------------------------------------------------------
# push — the everyday path
# ---------------------------------------------------------------------------
[pipelines.push]
description = "Stage everything, commit, and push"
order = 10

steps = [
  # `all` stages every change, including deletions.
  { action = "git.stage", all = true },

  # Falls back when `summary` is not provided with --set summary=...
  { action = "git.commit", message = "{{ summary | chore: update }}" },

  { action = "git.push", set_upstream = true },
]

# ---------------------------------------------------------------------------
# pr — push, open or update a pull request, watch CI
# ---------------------------------------------------------------------------
[pipelines.pr]
description = "Push, open or update a pull request, then watch CI"
order = 20

steps = [
  { action = "git.push", set_upstream = true },

  # With no title the PR is filled from the commits; existing PRs are updated.
  { action = "github.pr", base = "main" },

  { action = "github.watch", interval_secs = 10 },
]

# ---------------------------------------------------------------------------
# release — bump, tag, push, publish
# ---------------------------------------------------------------------------
[pipelines.release]
description = "Bump the version, tag it, push, and publish a GitHub release"
order = 30
# Only from a clean tree on the release branch.
when = { branch = "main", clean = true }

steps = [
  # Reads the newest matching tag and writes {{ version }} and {{ version_tag }}.
  { action = "bump", part = "patch" },

  { action = "git.tag", name = "{{ version_tag }}", message = "{{ version_tag }}", push = true },

  { action = "github.release", tag = "{{ version_tag }}", generate_notes = true },
]

# ---------------------------------------------------------------------------
# profiles — selected with --profile
# ---------------------------------------------------------------------------
[profiles.local]
description = "Extra local environment while iterating"

[profiles.local.env]
TUG_LOCAL = "1"
"#;

/// The `tug init` document, tailored to the repository name when we can guess it.
pub fn template(cwd: &Path) -> String {
    let name = cwd
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "this repository".to_string());
    TEMPLATE.replace("@NAME@", &name)
}

/// Write the starter config, refusing to clobber unless `force` is set.
pub fn write(dir: &Path, force: bool) -> anyhow::Result<std::path::PathBuf> {
    let path = dir.join("tug.toml");
    if path.exists() && !force {
        anyhow::bail!(
            "{} already exists (use --force to overwrite)",
            path.display()
        );
    }
    std::fs::write(&path, template(dir))?;
    Ok(path)
}
