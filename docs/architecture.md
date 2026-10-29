# Architecture

`tug` is a config-driven pipeline runner with two clients: a ratatui TUI and a
scriptable CLI. Both drive the same engine, so a pipeline behaves identically
whether you press `enter` or type `tug run`.

```text
tug.toml
   │
   ▼
config::load ──► LoadedConfig ──► config::context ──► Ctx
   │                                                    │
   │                                                    ├──► plan::build ──► TUI plan modal
   ▼                                                    │                  └─► `tug plan`
validate / unknown keys                                 │
                                                        ▼
                                            engine::run (worker thread)
                                                        │
                                            EngineEvent stream
                                                   │        │
                                                   ▼        ▼
                                                TUI log   CLI stdout
```

## Layers

| module | responsibility | must not |
| --- | --- | --- |
| `model` | the shared data contract: config types, `Action`, `When`, `Ctx`, `GitFacts`, `EngineEvent`, statuses | perform I/O |
| `template` | `{{ variable }}` rendering with fallbacks | know about actions |
| `condition` | `when` evaluation and glob matching | spawn processes itself |
| `config` | discovery, merging, validation, starter config | run steps |
| `git` | repository facts and `CmdSpec` builders | touch the terminal |
| `github` | `gh` command builders and response parsing | hold credentials |
| `actions` | one executor and one preview per action, behind the `Exec` trait | spawn processes directly |
| `engine` | ordering, gates, retries, timeouts, capture, cancellation, events | draw anything |
| `plan` | resolved, side-effect-free preview of a pipeline | mutate repo state |
| `tui` | terminal lifecycle, state machine, rendering | run actions |
| `cli` | argument parsing and headless output | duplicate engine behavior |

The dependency direction is one-way: `tui` and `cli` depend on `engine`, which
depends on `actions`, which depends on `git`/`github`/`template`/`condition`.
Nothing below `tui` imports `tui`.

## The two contracts

### `Exec`

Actions never call `std::process::Command`. Every side effect goes through the
`Exec` trait in `src/actions.rs`:

```rust
fn run(&mut self, spec: CmdSpec) -> Result<CmdResult>;
fn run_readonly(&mut self, spec: CmdSpec) -> Result<CmdResult>;
fn dry_run(&self) -> bool;
fn cancelled(&self) -> bool;
fn confirm(&mut self, prompt: &str) -> Result<bool>;
fn set_var(&mut self, key: &str, value: String);
```

This is what makes dry runs, cancellation, timeouts, and output capture uniform.
A fake `Exec` can unit-test any action without git, `gh`, or a terminal.

`run` honours dry-run for mutating commands. `run_readonly` always executes, with
output capture on and log echo off; it is only used for inspection (facts, PR
lookup, `gh release view`, tag listing).

### `EngineEvent`

The engine is blocking and reports progress as events:

```text
RunStarted · StepStarted · Output · Note · StepFinished
Confirm · InteractiveStart · InteractiveEnd · RunFinished
```

The TUI forwards them to its own channel; the CLI prints them. Neither reads
engine internals.

## Threading

```text
main thread            input thread           worker thread
───────────            ────────────           ─────────────
draw + key handling ◄── crossterm events      engine::run
      │                                          │
      └────────────── mpsc<Event> ◄──────────────┘
                         │
                    Control (Arc)
                         │
              cancel / confirm / terminal handover
```

- The UI thread owns `App` and the terminal. It never blocks on a child process.
- The worker runs one pipeline and exits.
- `Control` is the only cross-thread mutation path: cancel, answer a confirmation,
  release the terminal.
- Interactive steps and `$EDITOR` use a suspend/resume handshake: the UI leaves the
  alternate screen, signals the worker, waits for `InteractiveEnd` (or the editor
  to exit), then repaints from scratch.

## Planning versus running

Planning is a pure function of config and facts:

1. Evaluate pipeline-level gates for the list view.
2. For each step, resolve templates and build a `Preview`.
3. Evaluate the step gate.
4. Apply the in-memory effect of `set` and `bump` so later rows can render
   `{{ version_tag }}` and friends.
5. Emit a `PlanRow` with label, detail, command, mutating flag, and gate reason.

The same rows feed the TUI plan modal and `tug plan`. Planning never spawns a
command, so opening the modal cannot change the repository.

Running repeats steps 2–4 per step against live state, then executes, retries,
captures output, and streams events. Gates are re-evaluated immediately before
each step, so an earlier step can change what a later gate sees.

## Dry runs

`spec.dry_run` is threaded into the `Exec` implementation and into action
previews. The rules:

- Mutating commands are described, not spawned.
- Inspection commands still run, so summaries, URLs, and captures are real.
- `set` and `bump` update the in-memory context only.
- Steps report `StepStatus::Planned` instead of `Ok`, and a completed run says
  `planned`, not `shipped`.
- `confirm` and `sleep` return immediately.

## Cancellation

`Control::cancel` sets an atomic flag and kills the running child. The runner
checks the flag between lines of output, between retries, and inside `sleep`. The
engine reports `RunStatus::Cancelled`; the CLI exits `130`; the TUI marks the run
cancelled and leaves completed steps alone. There is no rollback.

## TUI rules

- **Render is pure.** `draw(frame, &App)` reads state and paints. No I/O, no locks,
  no process spawning. A frame can never stall a run.
- **Layout is one function.** `layout()` owns geometry; `log_inner_height()` must
  stay in sync with it, and a test asserts they agree at several heights.
- **Never draw while suspended.** The event loop checks `app.suspend` before every
  draw so a child process can own the terminal.
- **The plan modal is the safety gate.** Entering a run from the UI always goes
  through the modal unless the user is already looking at it.
- **Small terminals degrade, never panic.** Below 62×14 the app switches to a
  compact list; the minimum usable size is a single line.

Themes are semantic roles (`fg`, `muted`, `accent`, `success`, `warning`,
`danger`, `border`, `selection`). `mono`, `NO_COLOR`, and `TERM=dumb` drop color
and lean on glyphs.

## Testing

- Unit tests live beside the code.
- Template, condition, config, plan, and action tests use synthetic data and a
  fake `Exec`; they do not need a network, `gh`, or a TTY.
- Engine tests use temporary repositories and local bare remotes.
- Render tests use ratatui's `TestBackend` and assert on the buffer.
- `cargo run --example preview -- <mode>` dumps a real frame to stdout for quick
  visual checks (`idle`, `plan`, `help`, `info`, `confirm`, `error`, `done`,
  `run`, `tiny`).
- Tests that shell out use `sh` and `git` only, and skip nothing silently: a
  missing tool is a clear failure.

`just`-style recipes are not used yet. The definition of done is:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## Deliberate boundaries

- GitHub work is exclusively `gh`. No HTTP client, no token storage, no OAuth.
- Config is TOML and merged by table; there is no scripting language inside it.
  When a pipeline needs logic, use a `shell` step.
- Pipelines are independent. Chaining is expressed by repeating steps, not by a
  DAG.
- The engine is headless and single-run per process. The TUI keeps the process
  alive and starts a fresh worker per run.
- `continue_on_error` keeps the pipeline moving but the run still ends `Failed`,
  so a tolerated failure is visible to CI.
- No rollback. A failed pipeline leaves earlier steps applied, and the log says
  exactly where it stopped.
