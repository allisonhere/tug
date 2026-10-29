//! Minimal, strict `{{ variable }}` templates.
//!
//! Supported syntax:
//!
//! - `{{ name }}`, `{{ git.branch }}`, `{{ env.HOME }}`, `{{ vars.deploy }}`
//! - `{{ name | fallback text }}` — fallback used when the variable is missing
//!
//! Anything else (filters, conditionals, loops) is deliberately unsupported: the
//! pipeline config describes *what* to do, and shell steps cover the rest.

use anyhow::{Result, bail};

use crate::model::Ctx;

/// Render every `{{ ... }}` placeholder in `input`.
pub fn render(input: &str, ctx: &Ctx, strict: bool) -> Result<String> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;

    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            bail!("unclosed `{{{{` in template: {}", snippet(input, start));
        };
        let expr = after[..end].trim();
        out.push_str(&resolve(expr, ctx, strict)?);
        rest = &after[end + 2..];
    }

    out.push_str(rest);
    Ok(out)
}

fn resolve(expr: &str, ctx: &Ctx, strict: bool) -> Result<String> {
    if expr.is_empty() {
        bail!("empty `{{{{ }}}}` placeholder");
    }

    let (name, fallback) = match expr.split_once('|') {
        Some((name, fallback)) => (name.trim(), Some(unquote(fallback.trim()))),
        None => (expr, None),
    };

    if name.is_empty() {
        bail!("empty variable name before `|`");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        bail!("template variable `{name}` contains unsupported characters");
    }

    match ctx.get(name) {
        Some(value) => Ok(value),
        None => {
            if let Some(fallback) = fallback {
                return Ok(fallback);
            }
            if strict {
                let known = ctx.known_names().join(", ");
                bail!("unknown template variable `{name}` (known: {known})");
            }
            Ok(String::new())
        }
    }
}

fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')))
    {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

fn snippet(input: &str, at: usize) -> String {
    let end = (at + 40).min(input.len());
    let mut cut = end;
    while !input.is_char_boundary(cut) && cut < input.len() {
        cut += 1;
    }
    input[at..cut].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Ctx {
        let mut context = Ctx::default();
        context.vars.insert("version".into(), "1.2.3".into());
        context.env.insert("HOME".into(), "/home/me".into());
        context.facts.branch = Some("main".into());
        context.facts.sha = "abcdef123456".into();
        context.facts.short_sha = "abcdef1".into();
        context.facts.repo = Some("allisonhere/tug".into());
        context
    }

    #[test]
    fn renders_known_variables() {
        let out = render(
            "tag {{ version }} for {{ repo }} on {{ branch }} ({{ git.short_sha }})",
            &ctx(),
            true,
        )
        .unwrap();
        assert_eq!(out, "tag 1.2.3 for allisonhere/tug on main (abcdef1)");
    }

    #[test]
    fn renders_env() {
        assert_eq!(
            render("{{ env.HOME }}/src", &ctx(), true).unwrap(),
            "/home/me/src"
        );
    }

    #[test]
    fn strict_missing_variable_fails_with_name() {
        let err = render("{{ nope }}", &ctx(), true).unwrap_err().to_string();
        assert!(err.contains("nope"), "{err}");
    }

    #[test]
    fn lenient_missing_variable_is_empty() {
        assert_eq!(render("a{{ nope }}b", &ctx(), false).unwrap(), "ab");
    }

    #[test]
    fn fallback_is_used() {
        assert_eq!(
            render("{{ nope | \"main\" }}", &ctx(), true).unwrap(),
            "main"
        );
    }

    #[test]
    fn missing_value_does_not_swallow_fallback_with_branch() {
        assert_eq!(
            render("{{ profile | local }}", &ctx(), true).unwrap(),
            "local"
        );
    }

    #[test]
    fn rejects_unclosed_placeholder() {
        assert!(render("{{ version", &ctx(), true).is_err());
    }

    #[test]
    fn leaves_plain_text_untouched() {
        let text = "npm run build && echo ok";
        assert_eq!(render(text, &ctx(), true).unwrap(), text);
    }
}
