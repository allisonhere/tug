# tug

**Ship pipelines for git and GitHub, driven by one config file.**

`tug` reads a `tug.toml`, shows you exactly what will happen, then runs it step by
step: stage, commit, push, open or update a pull request, tag, publish a release,
watch CI, and run your own shell commands in between. It is a TUI first and a
scriptable CLI second.

```text
 tug  ship pipelines                                                allisonhere/tug  origin  v0.3.1
   feat/eyecandy   dirty   1 staged  2 unstaged  1 untracked             3 pipeline(s)  ·  tug.toml
────────────────────────────────────────────────────────────────────────────────────────────────────
╭ pipelines           3 ╮ ╭ push                                                               1/3 ╮
│▌ push             ▶ 3 │ │▰▰▰▰▰▰▰▰▰▰▰▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱▱  1/3  1.2s   running                   │
│   Stage everything, … │ │                                                                        │
│  pr                 3 │ │  ✓  1  git.stage             git add -A                           32ms │
│   Push, open or upda… │ │  ⠋  2  git.commit            feat: add the thing                       │
│  release            1 │ │  ·  3  git.push              origin/feat/eyecandy                      │
│   Bump the version, … │ │                                                                        │
│                       │ │                                                                        │
│                       │ │                                                                        │
│                       │ │                                                                        │
│                       │ │                                                                        │
│                       │ │                                                                        │
│                       │ │                                                                        │
│                       │ │                                                                        │
│                       │ │                                                                        │
│                       │ ╰────────────────────────────────────────────────────────────────────────╯
│                       │ ─ log ──────────────────────────────────────────────────────────── follow
│                       │ ▏▸ push · 3 steps
│                       │ ▏$ git add -A
│                       │ ▏
│                       │ ▏▸ 4 files staged
│                       │ ▏$ git commit -m 'feat: add the thing'
│                       │ ▏[feat/eyecandy 9f2c1ab] feat: add the thing
│                       │ ▏ 3 files changed, 121 insertions(+), 4 deletions(-)
╰───────────────────────╯
 x cancel  g/G log  f follow  ? help                                                        running
```

## Why

- **The plan is the product.** Every run starts with a resolved plan: the exact
  commands, in order, with templates filled in and gates evaluated. Nothing
  mutates before you see it.
- **One file, any workflow.** Pipelines are named lists of steps. Reorder them,
  gate them on branch or tree state, add conditions, retries, timeouts, captures,
  and profiles without writing a script.
- **Safe by default.** A dry run performs zero mutations. Force-pushes warn during
  validation. Releases refuse to clobber unless told to. Interrupting a run kills
  the child process and stops the pipeline.
- **GitHub without tokens.** All GitHub work goes through your authenticated `gh`
  CLI. `tug` never stores credentials.

## Install

Requires Rust 1.85+ and `git`. GitHub steps need [`gh`](https://cli.github.com/)
on `PATH`, authenticated with `gh auth login`.

```bash
git clone https://github.com/allisonhere/tug
cd tug
cargo install --path .
```

## Quick start

```bash
cd your-repo
tug init                 # writes a starter tug.toml
tug validate             # checks keys, actions, templates, gates
tug plan push            # prints the resolved plan
tug run push -y          # run it headlessly
tug                      # or open the TUI
```

The starter config gives you three pipelines — `push`, `pr`, and `release` — and
a `local` profile. Edit it to fit; the TUI reloads with `r`.

## How it works

A config is a set of named **pipelines**. Each pipeline is an ordered list of
**steps**, and each step is one **action**:

| group | actions |
| --- | --- |
| git | `git.stage`, `git.unstage`, `git.commit`, `git.push`, `git.fetch`, `git.checkout`, `git.tag` |
| GitHub | `github.pr`, `github.release`, `github.watch` |
| local | `shell`, `set`, `bump`, `confirm`, `sleep` |

Steps can be gated (`when`), templated (`{{ version }}`), retried, timed out,
silenced, and mined for values (`capture`). See
[docs/configuration.md](docs/configuration.md) for the full reference.

```toml
[pipelines.push]
description = "Stage everything, commit, and push"
order = 10

steps = [
  { action = "git.stage", all = true },
  { action = "git.commit", message = "{{ summary | chore: update }}" },
  { action = "git.push", set_upstream = true },
]
```

Run it with `tug run push --set summary="feat: add the thing"`.

### Templates

```toml
{ action = "git.tag", name = "{{ version_tag }}", message = "release {{ version }}" }
```

`{{ branch }}`, `{{ sha }}`, `{{ short_sha }}`, `{{ repo }}`, `{{ version }}`,
`{{ git.latest_tag }}`, `{{ git.base_branch }}`, `{{ env.HOME }}`, `{{ profile }}`,
and any `[vars]` or `--set` value are available. Use `{{ name | fallback }}` when
a value may be missing. Unknown variables fail the step unless
`defaults.strict_templates = false`.

## CLI

```text
tug [--config PATH] [--profile NAME] [--set K=V]... [--dry-run] [-y] [COMMAND]

  (no command)        open the TUI
  run <pipeline>      run a pipeline to completion
  plan <pipeline>     print the resolved plan
  list                list pipelines with gate status
  validate            validate the configuration
  init [--force]      write a starter tug.toml
```

- `--config` replaces config discovery entirely. `TUG_CONFIG` does the same.
- `--profile` applies a `[profiles.*]` overlay.
- `--set K=V` is the highest-precedence variable and works in the TUI too.
- `--dry-run` describes mutating steps without performing them.
- `-y` skips every confirmation, including `step.confirm` and `confirm` actions.

Exit codes: `0` success, `1` failure or aborted, `130` cancelled.

## TUI keys

| key | action |
| --- | --- |
| `enter` | review the plan, then run |
| `d` | review the plan in dry-run mode |
| `x`, `ctrl-c` | cancel the current run |
| `j` / `k`, `↓` / `↑` | move the selection, or scroll the log |
| `[` / `]` | previous / next pipeline |
| `tab` | switch between pipelines and log |
| `/` | filter pipelines |
| `g` / `G` | oldest / newest log line |
| `J` / `K` | page the log |
| `f` | toggle log follow |
| `r` | reload `tug.toml` |
| `e` | edit the config in `$EDITOR` |
| `i` | write a starter config when there is none |
| `c` | repository context |
| `?` | help |
| `q` | quit |

Interactive shell steps hand the terminal over to the child process and take it
back when the step ends. `NO_COLOR`, `TERM=dumb`, or `ui.theme = "mono"` switch to
a colourless, glyph-driven rendering.

## Safety

- The plan modal is the pre-flight confirmation. Dry-run steps report `planned`
  and execute nothing that mutates.
- `git push force = "force"` triggers a validation warning; prefer
  `force = "with-lease"`.
- `github.release` and `github.pr` use `update = true` by default, so a re-run
  refreshes the existing object instead of failing.
- A failing step stops the pipeline unless `continue_on_error = true`; the run
  still ends failed either way.
- `ctrl-c` during a run kills the running child, then reports `cancelled`.

## Documentation

- [Configuration reference](docs/configuration.md) — every key, action, gate, and
  template.
- [Architecture](docs/architecture.md) — layers, threading, planning, and the
  rules the UI follows.

The repository's own [`tug.toml`](tug.toml) is a working example: `check`, `push`,
`pr`, and `release`.

## License

MIT.
