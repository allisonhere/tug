//! Human-readable descriptions of what a pipeline will do.

use anyhow::{Context, Result};

use crate::actions;
use crate::condition;
use crate::git;
use crate::model::{Config, Ctx, Pipeline};

/// One row of the pre-flight plan.
#[derive(Debug, Clone, Default)]
pub struct PlanRow {
    pub index: usize,
    pub label: String,
    pub detail: String,
    pub command: Option<String>,
    pub mutating: bool,
    /// Set when the step is gated out, e.g. `branch mismatch`.
    pub gate: Option<String>,
}

/// Build the ordered plan for `pipeline`.
pub fn build(config: &Config, ctx: &Ctx, pipeline: &str) -> Result<Vec<PlanRow>> {
    let pipeline = pipeline_def(config, pipeline)?;
    let root = ctx.facts.root.clone();
    // `set` and `bump` change variables mid-run, so later rows must see their
    // effect; otherwise a release plan shows raw `{{ version_tag }}`.
    let mut ctx = ctx.clone();
    let mut rows = Vec::with_capacity(pipeline.steps.len());

    for (offset, step) in pipeline.steps.iter().enumerate() {
        let preview = actions::preview(&step.action, &ctx, &config.defaults).ok();
        let gate = step.when.as_ref().and_then(|when| {
            if when.is_empty() {
                return None;
            }
            let verdict =
                condition::evaluate(when, &ctx, &mut |command| git::shell_ok(command, &root));
            (!verdict.passed).then_some(verdict.reason)
        });

        rows.push(PlanRow {
            index: offset + 1,
            label: step.title(),
            detail: preview
                .as_ref()
                .map(|p| p.detail.clone())
                .unwrap_or_else(|| step.action.kind().to_string()),
            command: preview.and_then(|p| p.command),
            mutating: step.action.is_mutating(),
            gate,
        });

        actions::apply_plan_vars(&step.action, &mut ctx, &config.defaults);
    }

    Ok(rows)
}

fn pipeline_def<'a>(config: &'a Config, name: &str) -> Result<&'a Pipeline> {
    config
        .pipelines
        .get(name)
        .with_context(|| format!("unknown pipeline `{name}`"))
}

/// Render a plan as plain text for `tug plan` and non-TTY output.
pub fn render(config: &Config, ctx: &Ctx, pipeline: &str) -> Result<String> {
    let steps = build(config, ctx, pipeline)?;
    let mut out = String::new();

    out.push_str(&format!("tug · {}\n", ctx.facts.display_name()));
    out.push_str(&format!(
        "  branch {} · {}{}{}\n\n",
        ctx.facts
            .branch
            .clone()
            .unwrap_or_else(|| "(detached)".into()),
        if ctx.facts.dirty { "dirty" } else { "clean" },
        ctx.profile
            .as_ref()
            .map(|profile| format!(" · profile {profile}"))
            .unwrap_or_default(),
        if ctx.dry_run { " · dry run" } else { "" },
    ));

    let width = steps
        .iter()
        .map(|row| row.label.chars().count())
        .max()
        .unwrap_or(8)
        .min(24);

    for row in &steps {
        let gate = row
            .gate
            .as_ref()
            .map(|reason| format!("  [gated: {reason}]"))
            .unwrap_or_default();
        out.push_str(&format!(
            "  {:>2}  {:<width$}  {}{}\n",
            row.index,
            row.label,
            row.detail,
            gate,
            width = width
        ));
        if let Some(command) = &row.command {
            out.push_str(&format!("      $ {command}\n"));
        }
    }

    let mutating = steps.iter().filter(|row| row.mutating).count();
    out.push_str(&format!(
        "\n{} steps · {} mutating\n",
        steps.len(),
        mutating
    ));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Action, BumpPart, GitFacts, Step};

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

    #[test]
    fn plan_carries_set_and_bump_into_later_rows() {
        let mut config = Config::default();
        config.pipelines.insert(
            "release".into(),
            Pipeline {
                steps: vec![
                    step(Action::Set {
                        key: "name".into(),
                        value: "world".into(),
                    }),
                    step(Action::Bump {
                        part: BumpPart::Patch,
                        var: None,
                        tag: None,
                    }),
                    step(Action::Tag {
                        name: "{{ version_tag }}".into(),
                        message: Some("release {{ name }}".into()),
                        force: false,
                        sign: false,
                        push: false,
                        remote: None,
                    }),
                ],
                ..Pipeline::default()
            },
        );

        let ctx = Ctx::new(GitFacts {
            tags: vec!["v1.2.3".into()],
            ..GitFacts::default()
        });
        let rows = build(&config, &ctx, "release").expect("plan");

        assert_eq!(rows[0].detail, "name = world");
        assert_eq!(rows[1].detail, "bump version 1.2.3 → 1.2.4");
        assert_eq!(rows[2].detail, "v1.2.4");
        assert_eq!(
            rows[2].command.as_deref(),
            Some("git tag -a v1.2.4 -m 'release world'")
        );
    }

    #[test]
    fn gates_see_variables_set_earlier_in_the_plan() {
        use crate::model::When;
        use std::collections::BTreeMap;

        let mut config = Config::default();
        config.pipelines.insert(
            "gated".into(),
            Pipeline {
                steps: vec![
                    step(Action::Set {
                        key: "deploy".into(),
                        value: "yes".into(),
                    }),
                    Step {
                        when: Some(When {
                            var: BTreeMap::from([("deploy".into(), "no".into())]),
                            ..When::default()
                        }),
                        ..step(Action::Sleep { ms: 1 })
                    },
                ],
                ..Pipeline::default()
            },
        );

        let ctx = Ctx::new(GitFacts::default());
        let rows = build(&config, &ctx, "gated").expect("plan");
        assert!(rows[1].gate.is_some(), "{:?}", rows[1].gate);
    }
}
