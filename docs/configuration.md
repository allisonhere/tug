# Configuration

`tug` is driven by one TOML document. This page is the complete reference: where
the file is found, how files merge, every action, the step envelope, gates,
templates, captures, and what the validator checks.

If you only read one section, read [Steps](#steps) and
[Actions](#actions).

- [Where the config comes from](#where-the-config-comes-from)
- [Merging](#merging)
- [Top-level keys](#top-level-keys)
- [Defaults](#defaults)
- [Variables and the template context](#variables-and-the-template-context)
- [Profiles](#profiles)
- [Pipelines](#pipelines)
- [Steps](#steps)
- [Actions](#actions)
- [Gates (`when`)](#gates-when)
- [Captures](#captures)
- [Validation](#validation)
- [Dry runs and confirmations](#dry-runs-and-confirmations)
- [Exit codes](#exit-codes)

## Where the config comes from

With no `--config` (or `TUG_CONFIG`), `tug` reads these in order and merges them:

| order | path | intent |
| --- | --- | --- |
| 1 | `$XDG_CONFIG_HOME/tug/config.toml` | your personal baseline |
| 2 | `<repo>/tug.toml` | the committed project config |
| 3 | `<repo>/.tug.toml` | an alternate committed config |
| 4 | `<repo>/.tug.local.toml` | your machine-only overrides |

Later files win. Missing files are skipped silently. `--config PATH` replaces the
whole list, so nothing else is merged in.

`tug` looks for `tug.toml` relative to the repository root (the nearest directory
containing `.git`), not the current subdirectory. `tug init` writes to the same
place.

Commit `tug.toml`. Add `.tug.local.toml` to `.gitignore`.

## Merging

Tables merge deeply; arrays and scalars replace wholesale.

```toml
# tug.toml
[defaults]
remote = "origin"

[pipelines.push]
steps = [{ action = "git.commit", message = "chore: update" }]
```

```toml
# .tug.local.toml — adds a step to defaults without rewriting them
[defaults]
remote = "fork"
```

`pipelines.push.steps` is an array, so overriding it replaces the entire list.

## Top-level keys

```toml
version = 1                 # reserved for future migrations
```

| key | type | meaning |
| --- | --- | --- |
| `version` | integer | schema version; currently informational |
| `defaults` | table | global defaults, see below |
| `vars` | table of strings | variables available to every template |
| `env` | table of strings | environment variables injected into every step |
| `ui` | table | terminal presentation only |
| `profiles` | table of tables | named overlays |
| `pipelines` | table of tables | the actual work |

### `ui`

| key | type | default | meaning |
| --- | --- | --- | --- |
| `theme` | string | `"tide"` | `tide`, `light`, or `mono` |
| `follow_log` | bool | `true` | pin the log to the newest line while running |
| `density` | string | `"compact"` | `compact` or `comfortable` |

`mono` and the `NO_COLOR` / `TERM=dumb` environment variables force a colorless,
glyph-driven rendering. The engine does not read `ui` at all.

## Defaults

```toml
[defaults]
remote = "origin"
base_branch = "main"
tag_prefix = "v"
version_var = "version"
shell = "bash -lc"
strict_templates = true
confirm = "destructive"
timeout_secs = 600
```

| key | type | default | meaning |
| --- | --- | --- | --- |
| `remote` | string | `"origin"` | remote for push, fetch, and tag |
| `base_branch` | string | detected | PR base and `{{ base_branch }}` |
| `tag_prefix` | string | `"v"` | prefix for `bump` and tag suggestions |
| `version_var` | string | `"version"` | variable name `bump` writes |
| `shell` | string | `sh -c` | shell used by `shell` steps |
| `strict_templates` | bool | `true` | fail on unknown `{{ variables }}` |
| `confirm` | string | `"destructive"` | pre-run confirmation policy |
| `timeout_secs` | integer | none | default per-step timeout |

`confirm` values:

- `destructive` — confirm before running a pipeline that contains any mutating
  step. This is the default.
- `always` — confirm before every pipeline.
- `never` — never ask before a pipeline; step-level `confirm = true` still asks.

`shell` is split on whitespace into program plus leading arguments, then the
resolved command is appended. The default `sh -c` works everywhere.

## Variables and the template context

Templates use `{{ name }}`. Values come from, lowest to highest precedence:

1. repository facts and computed values (`branch`, `sha`, `repo`, `date`, …)
2. `[vars]`
3. the active `[profiles.<name>.vars]`
4. `[pipelines.<name>.vars]` (engine runs only)
5. `--set KEY=VALUE`, then `set`, `bump`, and `capture` during the run

Built-ins:

| variable | value |
| --- | --- |
| `branch` | current branch name |
| `sha` | full commit SHA |
| `short_sha` | abbreviated SHA |
| `repo` | `owner/name` parsed from the remote |
| `remote` | `[defaults].remote` |
| `upstream` | upstream ref, when set |
| `base_branch` | `[defaults].base_branch` or the detected default |
| `latest_tag` | most recent tag by creation date |
| `tag_prefix` | `[defaults].tag_prefix` |
| `version` | version variable, when set by `bump` or `set` |
| `profile` | active profile name, when one is set |
| `date` | `YYYY-MM-DD` |
| `time` | `HH:MM:SS` |
| `datetime` | `YYYY-MM-DD HH:MM` |
| `git.<name>` | alias for the repository facts (`git.branch`, `git.sha`, …) |
| `env.NAME` | process environment after `[env]` and profile `env` |
| `vars.NAME` | explicit lookup of a config variable |

Fallbacks use a pipe:

```toml
message = "{{ summary | chore: update }}"
name = "{{ version_tag | snapshot }}"
```

A fallback is used only when the variable is unset, never when it is set to an
empty string. Unknown variables are a step failure while
`strict_templates = true`; with it disabled they render as empty strings. `env.*`
lookups are always lenient.

## Profiles

A profile is an overlay selected with `--profile NAME` (also in the TUI). It can
add variables, environment, and defaults, and it applies on top of the merged
config before the run starts.

```toml
[profiles.local]
description = "fast loop: skip CI watching"
vars = { base_branch = "develop" }

[profiles.local.defaults]
remote = "fork"

[profiles.ci]
env = { CI = "1" }
```

Variables and env are additive. `defaults` fields override the corresponding
`[defaults]` key. Profiles cannot rewrite pipelines; use gates for that.

## Pipelines

```toml
[pipelines.push]
description = "Stage everything, commit, and push"
order = 10
when = { branch = "main", clean = false }
confirm = "destructive"
env = { GIT_TERMINAL_PROMPT = "0" }
vars = { summary = "chore: update" }

steps = [ /* see below */ ]
```

| key | type | meaning |
| --- | --- | --- |
| `description` | string | one-line summary in the UI |
| `order` | integer | sort key, lowest first; ties by name |
| `when` | table | pipeline-level gate; a failing gate disables it |
| `confirm` | string | overrides `[defaults].confirm` |
| `env` | table | environment for every step in the pipeline |
| `vars` | table | variables for every template in the pipeline |
| `steps` | array | ordered steps; required |

A gated pipeline still appears in the TUI, marked with the reason. `tug list`
prints `gated: <reason>`. The engine refuses to run it.

## Steps

Every step is a table with one `action` and a shared envelope:

```toml
{
  action = "git.commit",
  message = "feat: add the thing",
  label = "commit changes",
  when = { clean = false },
  env = { GIT_AUTHOR_NAME = "allie" },
  cwd = "packages/web",
  timeout = 120,
  retries = 2,
  continue_on_error = false,
  confirm = false,
  silent = false,
  capture = { name = "sha", regex = "\\[([0-9a-f]{7,})\\]" },
}
```

| key | type | default | meaning |
| --- | --- | --- | --- |
| `action` | string | required | see [Actions](#actions) |
| `label` | string | action kind | display name |
| `when` | table | none | skip the step unless the gate passes |
| `env` | table | none | extra environment for this step |
| `cwd` | string | repo root | working directory, relative to root |
| `timeout` | integer | `[defaults].timeout_secs` | seconds; unset means no limit |
| `retries` | integer | `0` | extra attempts after a failure |
| `continue_on_error` | bool | `false` | keep going after this step fails |
| `confirm` | bool | `false` | ask before this step runs (`--yes` skips it) |
| `silent` | bool | `false` | suppress output unless the step fails |
| `capture` | table | none | extract a variable from the output |

`label` is named `label`, not `name`, because `name` belongs to `git.tag`.

Each attempt gets a fresh timeout. Cancellation is checked between attempts and
between lines of output. A `continue_on_error` failure is logged and later steps
still run, but the run finishes `Failed` and the CLI exits non-zero, so a broken
step cannot disappear into a green build.

## Actions

Aliases (`commit`, `push`, `pr`, …) work anywhere the canonical name does.
Canonical names are listed here.

### Git

#### `git.stage`

| key | type | default |
| --- | --- | --- |
| `paths` | array of strings | `[]` |
| `all` | bool | `false` |
| `update` | bool | `false` |

`all = true` stages everything (`git add -A`). `update = true` stages tracked
changes only (`git add -u`). With neither, `paths` is required.

#### `git.unstage`

| key | type | default |
| --- | --- | --- |
| `paths` | array of strings | `[]` |
| `all` | bool | `false` |

`all = true` runs `git reset`.

#### `git.commit`

| key | type | default |
| --- | --- | --- |
| `message` | string | required |
| `all` | bool | `false` |
| `allow_empty` | bool | `false` |
| `amend` | bool | `false` |
| `signoff` | bool | `false` |
| `no_verify` | bool | `false` |

The message is templated. `all = true` adds `-a`, so tracked changes are staged
for this commit only.

#### `git.push`

| key | type | default |
| --- | --- | --- |
| `remote` | string | `[defaults].remote` |
| `branch` | string | current branch |
| `set_upstream` | bool | `false` |
| `force` | string | `"no"` |
| `tags` | bool | `false` |

`force` is `"no"`, `"with-lease"`, or `"force"`. Plain `"force"` produces a
validation warning. `set_upstream = true` adds `--set-upstream`, which is what
you want on a branch that has never been pushed.

#### `git.fetch`

| key | type | default |
| --- | --- | --- |
| `remote` | string | `[defaults].remote` |
| `prune` | bool | `false` |
| `tags` | bool | `false` |

#### `git.checkout`

| key | type | default |
| --- | --- | --- |
| `target` | string | required |
| `create` | bool | `false` |

`create = true` runs `git checkout -b <target>`.

#### `git.tag`

| key | type | default |
| --- | --- | --- |
| `name` | string | required |
| `message` | string | none |
| `force` | bool | `false` |
| `sign` | bool | `false` |
| `push` | bool | `false` |
| `remote` | string | `[defaults].remote` |

A `message` makes an annotated tag. `push = true` pushes the tag immediately.

### GitHub

All GitHub actions shell out to `gh`. It must be installed and authenticated with
`gh auth login`. `tug` never reads, stores, or forwards a token.

#### `github.pr`

| key | type | default |
| --- | --- | --- |
| `title` | string | branch name |
| `body` | string | empty |
| `base` | string | `[defaults].base_branch` |
| `draft` | bool | `false` |
| `update` | bool | `true` |
| `reviewers` | array of strings | `[]` |
| `labels` | array of strings | `[]` |

With `update = true` an existing PR for the branch is edited instead of the
action failing, so pipelines are re-runnable. With `update = false` a second run
fails when a PR already exists.

#### `github.release`

| key | type | default |
| --- | --- | --- |
| `tag` | string | required |
| `title` | string | tag name |
| `notes` | string | none |
| `notes_file` | string | none |
| `draft` | bool | `false` |
| `prerelease` | bool | `false` |
| `generate_notes` | bool | `false` |
| `assets` | array of strings | `[]` |
| `update` | bool | `true` |

`notes` and `notes_file` are mutually exclusive. `generate_notes` asks GitHub to
build notes from commits. `assets` are globbed and uploaded. `update = true`
edits an existing release for the tag.

#### `github.watch`

| key | type | default |
| --- | --- | --- |
| `timeout_secs` | integer | 3600 |
| `interval_secs` | integer | `gh` default |

Polls `gh pr checks --watch` until the checks settle. A timeout fails the step.
Watching is read-only, so it still runs during a dry run.

### Local

#### `shell`

| key | type | default |
| --- | --- | --- |
| `run` | string | required |
| `interactive` | bool | `false` |
| `print_command` | bool | `true` |

`run` is templated and executed with `[defaults].shell` (or `sh -c`). The resolved
command is echoed before it runs unless `print_command = false`.

`interactive = true` hands the terminal to the child process. In the TUI the app
leaves the alternate screen, the child owns the TTY, and the app resumes when it
exits. Interactive steps require a real terminal; headless runs execute them as
normal commands.

Shell steps count as mutating, because `tug` cannot see inside them. That means
they appear in the plan with their command and trigger confirmation under the
`destructive` policy.

#### `set`

| key | type | default |
| --- | --- | --- |
| `key` | string | required |
| `value` | string | required |

Writes a variable into the run context. `value` is templated first.

```toml
{ action = "set", key = "version_tag", value = "{{ tag_prefix }}{{ version }}" }
```

#### `bump`

| key | type | default |
| --- | --- | --- |
| `part` | string | `"patch"` |
| `var` | string | `[defaults].version_var` |
| `tag` | string | `<tag_prefix>*` |

Reads the newest matching tag, increments `major`, `minor`, or `patch`, and writes
the result to `var` without the prefix. `part = "none"` keeps the version and only
rewrites the variable; validation warns about it.

#### `confirm`

| key | type | default |
| --- | --- | --- |
| `prompt` | string | required |

Pauses for an explicit yes/no. Headless runs without `--yes` prompt on stdin;
`--yes` auto-confirms. Decline fails the step.

#### `sleep`

| key | type | default |
| --- | --- | --- |
| `ms` | integer | `500` |

Cancellation interrupts the sleep.

## Gates (`when`)

`when` appears on pipelines and steps. Every present field must hold; `any` adds
alternatives and `not` inverts the whole predicate.

```toml
when = { branch = "release/*", clean = true, not = false }
when = { any = [{ branch = "main" }, { tag = "v*" }] }
```

| key | type | meaning |
| --- | --- | --- |
| `branch` | glob | current branch matches |
| `clean` | bool | working tree clean (`true`) or dirty (`false`) |
| `tag` | glob | any reachable tag matches |
| `upstream` | bool | the branch has an upstream |
| `ahead` | bool | the branch is ahead of its upstream |
| `env` | table | environment equals every entry |
| `var` | table | run variable equals every entry |
| `command` | string | shell command exits 0 |
| `any` | array of `when` | passes when any sub-predicate passes |
| `not` | bool | invert the result |

Globs support `*` and `?`. An empty `when` block is a validation warning because
it never changes anything.

Gate evaluation happens before each step, so a step can react to a previous step
(for example, `bump` setting `version` before a gated tag step).

## Captures

`capture` reads a regex out of a step's combined stdout and stderr and stores it
for later templates:

```toml
steps = [
  { action = "shell", run = "cat package.json", capture = { name = "raw", regex = "\"version\": \"([^\"]+)\"" } },
  { action = "set", key = "version_tag", value = "v{{ raw }}" },
  { action = "git.tag", name = "{{ version_tag }}", push = true },
]
```

| key | type | default |
| --- | --- | --- |
| `name` | string | required |
| `regex` | string | required |
| `group` | integer | `1`, or the whole match when the regex has no groups |

Rules:

- The capture name is resolved by strict templates like any other variable, and
  it is available to every later step.
- A regex that matches nothing logs a warning and leaves the variable unset; the
  step itself still succeeds, so guard later templates with a fallback.
- Captures work with `shell` and inspection commands, which stream their output.
  The plan shows the command that produces them.
- The validator rejects invalid regexes and warns about duplicate names in one
  pipeline.

## Validation

`tug validate` reports every problem it can find without running anything, and
exits non-zero when any are errors. It checks:

- unknown top-level, pipeline, step, and action keys, with `did you mean` hints
- invalid action names
- invalid `capture.regex` values
- template syntax in every templated string
- empty pipeline names and pipelines with no steps
- empty `when` blocks
- force pushes, `bump part = "none"`, and confirmations on read-only steps
  (warnings, not errors)

Problems carry a path, so a message like
`pipelines.push.steps[1].message: unknown key `mesage`` points at the exact key.
Unknown keys are reported *before* the parse error they cause, because a typo
often surfaces as a confusing `missing field` message. The validator runs on
every load; `tug run`, `tug plan`, and the TUI all refuse to proceed while errors
remain.

## Dry runs and confirmations

A dry run reads the config, evaluates gates, resolves templates, and builds the
plan, but performs no mutation:

- read-only inspection still runs, so summaries, PR URLs, and captures are real
- mutating steps report `planned` with the command they would have run
- `set` and `bump` still update in-memory variables; nothing on disk changes
- the `confirm` action and `sleep` return immediately
- interactive steps are described, not handed the terminal

Start one with `--dry-run` on the CLI or `d` in the plan modal. A run that
completes in dry-run mode reports `planned`, not `shipped`.

Confirmation layers:

1. The **plan modal** in the TUI is the pre-flight confirmation. It shows every
   step, its mutating marker, the resolved command, and any gate reason. `d`
   toggles dry-run; `enter` starts the run.
2. `[defaults].confirm` controls the headless prompt. `--yes` skips it.
3. `step.confirm = true` asks before that step, in addition to the pipeline
   prompt. `--yes` skips it.
4. The `confirm` action asks whenever it is reached, unless the run is dry or
   `--yes` is set.

Cancelling (`x` or `ctrl-c`) kills the running child process, marks the run
`cancelled`, and leaves the rest of the pipeline untouched. Completed steps are
not rolled back.

## Exit codes

| code | meaning |
| --- | --- |
| `0` | the pipeline completed with every step passing |
| `1` | a step failed, validation failed, the config could not be loaded, or you declined a prompt |
| `130` | the run was cancelled |
