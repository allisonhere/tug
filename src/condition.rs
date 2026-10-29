//! Evaluation of `when` gates against the current run context.

use crate::model::{Ctx, When};

/// Outcome of a condition check, with a human-readable explanation either way.
#[derive(Debug, Clone)]
pub struct Verdict {
    pub passed: bool,
    pub reason: String,
}

impl Verdict {
    fn pass() -> Self {
        Self {
            passed: true,
            reason: String::new(),
        }
    }

    fn fail(reason: impl Into<String>) -> Self {
        Self {
            passed: false,
            reason: reason.into(),
        }
    }
}

/// Evaluate a `when` block.
///
/// `run_command` is called for `command` predicates; it must return true when the
/// command exits 0. It is passed explicitly so this module stays testable without
/// spawning processes.
pub fn evaluate(when: &When, ctx: &Ctx, run_command: &mut dyn FnMut(&str) -> bool) -> Verdict {
    let mut verdict = evaluate_inner(when, ctx, run_command);
    if when.not {
        verdict.passed = !verdict.passed;
        verdict.reason = if verdict.passed {
            String::new()
        } else {
            format!("negated condition matched: {}", verdict.reason)
        };
    }
    verdict
}

fn evaluate_inner(when: &When, ctx: &Ctx, run_command: &mut dyn FnMut(&str) -> bool) -> Verdict {
    if let Some(pattern) = &when.branch {
        let branch = ctx.facts.branch.clone().unwrap_or_default();
        if !glob_match(pattern, &branch) {
            return Verdict::fail(format!("branch {branch:?} does not match {pattern:?}"));
        }
    }

    if let Some(want_clean) = when.clean {
        let clean = ctx.facts.is_clean();
        if clean != want_clean {
            return Verdict::fail(if want_clean {
                "working tree is not clean"
            } else {
                "working tree is clean"
            });
        }
    }

    if let Some(pattern) = &when.tag {
        let matched = ctx.facts.tags.iter().any(|tag| glob_match(pattern, tag));
        if !matched {
            return Verdict::fail(format!("no tag matches {pattern:?}"));
        }
    }

    if let Some(want) = when.upstream {
        if ctx.facts.has_upstream() != want {
            return Verdict::fail(if want {
                "branch has no upstream"
            } else {
                "branch has an upstream"
            });
        }
    }

    if let Some(want) = when.ahead {
        let ahead = ctx.facts.ahead > 0;
        if ahead != want {
            return Verdict::fail(if want {
                "branch is not ahead of its upstream"
            } else {
                "branch is ahead of its upstream"
            });
        }
    }

    for (key, want) in &when.env {
        let actual = ctx.env.get(key).cloned().unwrap_or_default();
        if &actual != want {
            return Verdict::fail(format!("env {key}={actual:?} does not equal {want:?}"));
        }
    }

    for (key, want) in &when.var {
        let actual = ctx.vars.get(key).cloned().unwrap_or_default();
        if &actual != want {
            return Verdict::fail(format!("variable {key}={actual:?} does not equal {want:?}"));
        }
    }

    if let Some(command) = &when.command {
        if !run_command(command) {
            return Verdict::fail(format!("command failed: {command}"));
        }
    }

    if !when.any.is_empty() {
        let mut reasons = Vec::new();
        for alternative in &when.any {
            let verdict = evaluate(alternative, ctx, run_command);
            if verdict.passed {
                return Verdict::pass();
            }
            reasons.push(verdict.reason);
        }
        return Verdict::fail(format!("none matched: {}", reasons.join("; ")));
    }

    Verdict::pass()
}

/// Shell-style glob matching with `*` (any run) and `?` (one character).
///
/// Deliberately small: pipeline gates name branches and tags, not file paths.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    glob_bytes(pattern.as_bytes(), text.as_bytes())
}

fn glob_bytes(pattern: &[u8], text: &[u8]) -> bool {
    match pattern.split_first() {
        None => text.is_empty(),
        Some((b'*', rest)) => {
            // Collapse consecutive stars, then try every split point.
            let rest = rest
                .iter()
                .skip_while(|b| **b == b'*')
                .copied()
                .collect::<Vec<_>>();
            if rest.is_empty() {
                return true;
            }
            (0..=text.len()).any(|i| glob_bytes(&rest, &text[i..]))
        }
        Some((b'?', rest)) => !text.is_empty() && glob_bytes(rest, &text[1..]),
        Some((byte, rest)) => text.first() == Some(byte) && glob_bytes(rest, &text[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::GitFacts;
    use std::collections::BTreeMap;

    fn ctx() -> Ctx {
        let mut facts = GitFacts {
            in_repo: true,
            branch: Some("main".into()),
            tags: vec!["v1.0.0".into(), "v1.1.0".into()],
            ..GitFacts::default()
        };
        facts.ahead = 2;
        facts.upstream = Some("origin/main".into());
        facts.dirty = false;
        let mut ctx = Ctx::new(facts);
        ctx.env.insert("CI".into(), "true".into());
        ctx.vars.insert("deploy".into(), "yes".into());
        ctx
    }

    fn yes(_: &str) -> bool {
        true
    }

    #[test]
    fn empty_when_passes() {
        assert!(evaluate(&When::default(), &ctx(), &mut yes).passed);
    }

    #[test]
    fn branch_glob_matches() {
        let when = When {
            branch: Some("mai*".into()),
            ..When::default()
        };
        assert!(evaluate(&when, &ctx(), &mut yes).passed);
    }

    #[test]
    fn branch_mismatch_explains_itself() {
        let when = When {
            branch: Some("release/*".into()),
            ..When::default()
        };
        let verdict = evaluate(&when, &ctx(), &mut yes);
        assert!(!verdict.passed);
        assert!(verdict.reason.contains("release/*"), "{}", verdict.reason);
    }

    #[test]
    fn clean_gate() {
        let when = When {
            clean: Some(true),
            ..When::default()
        };
        assert!(evaluate(&when, &ctx(), &mut yes).passed);
        let when = When {
            clean: Some(false),
            ..When::default()
        };
        assert!(!evaluate(&when, &ctx(), &mut yes).passed);
    }

    #[test]
    fn tag_glob() {
        let when = When {
            tag: Some("v1.*".into()),
            ..When::default()
        };
        assert!(evaluate(&when, &ctx(), &mut yes).passed);
        let when = When {
            tag: Some("v9.*".into()),
            ..When::default()
        };
        assert!(!evaluate(&when, &ctx(), &mut yes).passed);
    }

    #[test]
    fn env_and_var_checks() {
        let mut env = BTreeMap::new();
        env.insert("CI".to_string(), "true".to_string());
        let when = When {
            env,
            ..When::default()
        };
        assert!(evaluate(&when, &ctx(), &mut yes).passed);

        let mut var = BTreeMap::new();
        var.insert("deploy".to_string(), "no".to_string());
        let when = When {
            var,
            ..When::default()
        };
        assert!(!evaluate(&when, &ctx(), &mut yes).passed);
    }

    #[test]
    fn any_group_is_or() {
        let when = When {
            any: vec![
                When {
                    branch: Some("nope".into()),
                    ..When::default()
                },
                When {
                    branch: Some("main".into()),
                    ..When::default()
                },
            ],
            ..When::default()
        };
        assert!(evaluate(&when, &ctx(), &mut yes).passed);
    }

    #[test]
    fn not_inverts() {
        let when = When {
            branch: Some("main".into()),
            not: true,
            ..When::default()
        };
        assert!(!evaluate(&when, &ctx(), &mut yes).passed);
    }

    #[test]
    fn command_predicate_uses_runner() {
        let when = When {
            command: Some("test".into()),
            ..When::default()
        };
        assert!(evaluate(&when, &ctx(), &mut yes).passed);
        assert!(!evaluate(&when, &ctx(), &mut |_| false).passed);
    }

    #[test]
    fn glob_edges() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("v*", "v1.0"));
        assert!(glob_match("release/*", "release/1.2"));
        assert!(!glob_match("release/*", "release"));
        assert!(glob_match("v?.?.?", "v1.2.3"));
        assert!(!glob_match("v?.?.?", "v1.2.34"));
        assert!(glob_match("exact", "exact"));
        assert!(!glob_match("exact", "exactly"));
    }
}
