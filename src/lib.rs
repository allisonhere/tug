//! tug — config-driven ship pipelines for git and GitHub.
//!
//! Layer map (see `docs/architecture.md` for the full rationale):
//!
//! | module        | responsibility                                  |
//! |---------------|-------------------------------------------------|
//! | `model`       | shared data contract (config, facts, events)    |
//! | `template`    | `{{ var }}` rendering                           |
//! | `condition`   | `when` predicate evaluation                     |
//! | `config`      | discovery, merge, validation, `init` template   |
//! | `git`         | git facts + streaming git commands              |
//! | `github`      | `gh` adapter                                    |
//! | `actions`     | one executor + preview per action               |
//! | `engine`      | pipeline runner, cancellation, dry-run          |
//! | `plan`        | human-readable resolved plan                    |
//! | `tui`         | ratatui front end                               |
//! | `cli`         | argument parsing and headless entry points      |

pub mod actions;
pub mod cli;
pub mod condition;
pub mod config;
pub mod engine;
pub mod git;
pub mod github;
pub mod model;
pub mod plan;
pub mod template;
pub mod tui;
