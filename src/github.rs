//! `gh` CLI adapter.
//!
//! Everything here builds a [`CmdSpec`] or parses `gh` output. Execution goes
//! through the engine, so dry runs, timeouts, and streaming stay in one place.
//! tug never stores or reads GitHub tokens.

use std::path::Path;

use anyhow::{Context, Result};

use crate::git::CmdSpec;
use crate::model::Preview;

pub const GH: &str = "gh";

/// A pull request discovered for the current branch.
#[derive(Debug, Clone)]
pub struct PrInfo {
    pub number: u64,
    pub url: String,
    pub state: String,
}

#[derive(Debug, Clone, Default)]
pub struct PrRequest {
    pub branch: String,
    pub title: Option<String>,
    pub body: Option<String>,
    pub base: Option<String>,
    pub draft: bool,
    pub reviewers: Vec<String>,
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ReleaseRequest {
    pub tag: String,
    pub title: Option<String>,
    pub notes: Option<String>,
    pub notes_file: Option<String>,
    pub generate_notes: bool,
    pub draft: bool,
    pub prerelease: bool,
    pub assets: Vec<String>,
}

/// `gh pr list` for the head branch.
pub fn find_pr_spec(root: &Path, branch: &str) -> CmdSpec {
    CmdSpec::new(GH, root)
        .args([
            "pr",
            "list",
            "--head",
            branch,
            "--state",
            "open",
            "--json",
            "number,url,state",
            "--limit",
            "1",
        ])
        .env("GH_PROMPT_DISABLED", "1")
}

pub fn parse_pr_list(json: &str) -> Result<Option<PrInfo>> {
    let value: serde_json::Value =
        serde_json::from_str(json).context("parsing `gh pr list --json` output")?;
    let Some(first) = value.as_array().and_then(|items| items.first()) else {
        return Ok(None);
    };
    Ok(Some(PrInfo {
        number: first.get("number").and_then(|v| v.as_u64()).unwrap_or(0),
        url: first
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        state: first
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("OPEN")
            .to_string(),
    }))
}

pub fn create_pr_spec(root: &Path, req: &PrRequest) -> CmdSpec {
    let mut spec = CmdSpec::new(GH, root)
        .args(["pr", "create"])
        .env("GH_PROMPT_DISABLED", "1");

    match (&req.title, &req.body) {
        (None, None) => spec = spec.arg("--fill"),
        (title, body) => {
            if let Some(title) = title {
                spec = spec.args(["--title", title]);
            }
            if let Some(body) = body {
                spec = spec.args(["--body", body]);
            }
        }
    }

    spec = spec.args(["--head", &req.branch]);
    if let Some(base) = &req.base {
        spec = spec.args(["--base", base]);
    }
    if req.draft {
        spec = spec.arg("--draft");
    }
    for reviewer in &req.reviewers {
        spec = spec.args(["--reviewer", reviewer]);
    }
    for label in &req.labels {
        spec = spec.args(["--label", label]);
    }
    spec
}

pub fn edit_pr_spec(root: &Path, number: u64, req: &PrRequest) -> CmdSpec {
    let mut spec = CmdSpec::new(GH, root)
        .args(["pr", "edit", &number.to_string()])
        .env("GH_PROMPT_DISABLED", "1");
    if let Some(title) = &req.title {
        spec = spec.args(["--title", title]);
    }
    if let Some(body) = &req.body {
        spec = spec.args(["--body", body]);
    }
    if let Some(base) = &req.base {
        spec = spec.args(["--base", base]);
    }
    for reviewer in &req.reviewers {
        spec = spec.args(["--add-reviewer", reviewer]);
    }
    for label in &req.labels {
        spec = spec.args(["--add-label", label]);
    }
    spec
}

/// `gh pr checks --watch`, which exits non-zero when a check fails.
pub fn checks_spec(root: &Path, interval_secs: Option<u64>) -> CmdSpec {
    let mut spec = CmdSpec::new(GH, root)
        .args(["pr", "checks", "--watch", "--fail-fast"])
        .env("GH_PROMPT_DISABLED", "1");
    if let Some(interval) = interval_secs {
        spec = spec.args(["--interval", &interval.to_string()]);
    }
    spec
}

pub fn release_view_spec(root: &Path, tag: &str) -> CmdSpec {
    CmdSpec::new(GH, root)
        .args(["release", "view", tag, "--json", "tagName,url"])
        .env("GH_PROMPT_DISABLED", "1")
}

pub fn release_create_spec(root: &Path, req: &ReleaseRequest) -> CmdSpec {
    let mut spec = CmdSpec::new(GH, root)
        .args(["release", "create", &req.tag])
        .env("GH_PROMPT_DISABLED", "1");

    if let Some(title) = &req.title {
        spec = spec.args(["--title", title]);
    }
    if let Some(notes_file) = &req.notes_file {
        spec = spec.args(["--notes-file", notes_file]);
    } else if let Some(notes) = &req.notes {
        spec = spec.args(["--notes", notes]);
    } else if req.generate_notes {
        spec = spec.arg("--generate-notes");
    } else {
        spec = spec.args(["--notes", ""]);
    }
    if req.draft {
        spec = spec.arg("--draft");
    }
    if req.prerelease {
        spec = spec.arg("--prerelease");
    }
    for asset in &req.assets {
        spec = spec.arg(asset);
    }
    spec
}

pub fn release_edit_spec(root: &Path, req: &ReleaseRequest) -> CmdSpec {
    let mut spec = CmdSpec::new(GH, root)
        .args(["release", "edit", &req.tag])
        .env("GH_PROMPT_DISABLED", "1");
    if let Some(title) = &req.title {
        spec = spec.args(["--title", title]);
    }
    if let Some(notes_file) = &req.notes_file {
        spec = spec.args(["--notes-file", notes_file]);
    } else if let Some(notes) = &req.notes {
        spec = spec.args(["--notes", notes]);
    }
    spec = spec.args(["--draft", if req.draft { "true" } else { "false" }]);
    spec = spec.args([
        "--prerelease",
        if req.prerelease { "true" } else { "false" },
    ]);
    spec
}

pub fn release_upload_spec(root: &Path, tag: &str, assets: &[String]) -> CmdSpec {
    CmdSpec::new(GH, root)
        .args(["release", "upload", tag])
        .args(assets.iter().cloned())
        .arg("--clobber")
        .env("GH_PROMPT_DISABLED", "1")
}

/// The first URL printed by a `gh` command, if any.
pub fn first_url(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .find(|word| word.starts_with("https://github.com/"))
        .map(|word| word.trim_end_matches(['.', ',']).to_string())
}

/// Best-effort preview for a GitHub step.
pub fn preview_hint(action: &str, detail: &str) -> Preview {
    Preview {
        detail: format!("{action} {detail}").trim().to_string(),
        command: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pr_json() {
        let json = r#"[{"number":42,"url":"https://github.com/a/b/pull/42","state":"OPEN"}]"#;
        let pr = parse_pr_list(json).unwrap().unwrap();
        assert_eq!(pr.number, 42);
        assert_eq!(pr.state, "OPEN");
    }

    #[test]
    fn empty_pr_list_is_none() {
        assert!(parse_pr_list("[]").unwrap().is_none());
    }

    #[test]
    fn create_uses_fill_without_title_or_body() {
        let req = PrRequest {
            branch: "feature".into(),
            ..PrRequest::default()
        };
        let spec = create_pr_spec(Path::new("/tmp"), &req);
        assert!(spec.display().contains("--fill"), "{}", spec.display());
        assert!(spec.display().contains("--head feature"));
    }

    #[test]
    fn create_passes_labels_and_reviewers() {
        let req = PrRequest {
            branch: "main".into(),
            title: Some("fix: x".into()),
            body: Some("body".into()),
            base: Some("main".into()),
            reviewers: vec!["alice".into()],
            labels: vec!["bug".into()],
            ..PrRequest::default()
        };
        let display = create_pr_spec(Path::new("/tmp"), &req).display();
        assert!(display.contains("--reviewer alice"));
        assert!(display.contains("--label bug"));
        assert!(display.contains("--base main"));
        assert!(!display.contains("--fill"));
    }

    #[test]
    fn release_edit_sets_flags_explicitly() {
        let req = ReleaseRequest {
            tag: "v1.0.0".into(),
            draft: false,
            prerelease: true,
            ..ReleaseRequest::default()
        };
        let display = release_edit_spec(Path::new("/tmp"), &req).display();
        assert!(display.contains("--draft false"));
        assert!(display.contains("--prerelease true"));
    }

    #[test]
    fn extracts_first_url() {
        let out = "Creating pull request for feature into main\nhttps://github.com/a/b/pull/7\n";
        assert_eq!(
            first_url(out).as_deref(),
            Some("https://github.com/a/b/pull/7")
        );
    }
}
