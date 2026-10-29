//! Pipeline execution.
//!
//! The engine is a blocking function driven by the caller: the TUI runs it on a
//! worker thread, the CLI calls it directly. Progress is reported through
//! [`crate::model::EngineEvent`]; cancellation and confirmation handshakes go
//! through [`Control`].

pub mod control;
pub mod exec;
pub mod run;

pub use control::{Control, ControlRx};
pub use run::{RunSpec, run};
