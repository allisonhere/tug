//! Cancellation, child-process control, and the confirm/interactive handshakes.
//!
//! This is the only cross-thread mutable state in tug. It is intentionally
//! small and lock-free on the cancellation path.

use std::io;
use std::process::{Child, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

/// Held by the UI or CLI; the engine receives the matching [`ControlRx`].
pub struct Control {
    cancel: AtomicBool,
    child: Mutex<Option<Child>>,
    answer: Mutex<Sender<bool>>,
    gate: Mutex<Sender<()>>,
}

/// Engine-side receivers for the two handshakes.
pub struct ControlRx {
    /// Answers to `confirm` requests, in order.
    pub answer: Receiver<bool>,
    /// Emitted by the UI once it has released the terminal.
    pub gate: Receiver<()>,
}

impl Control {
    pub fn new() -> (Arc<Self>, ControlRx) {
        let (answer_tx, answer_rx) = mpsc::channel();
        let (gate_tx, gate_rx) = mpsc::channel();
        let control = Arc::new(Self {
            cancel: AtomicBool::new(false),
            child: Mutex::new(None),
            answer: Mutex::new(answer_tx),
            gate: Mutex::new(gate_tx),
        });
        let rx = ControlRx {
            answer: answer_rx,
            gate: gate_rx,
        };
        (control, rx)
    }

    /// Request cancellation: flips the flag and kills any running child.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.kill_child();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    pub fn set_child(&self, child: Option<Child>) {
        if let Ok(mut slot) = self.child.lock() {
            // Dropping the previous value closes its pipes; it must already be reaped.
            *slot = child;
        }
    }

    /// Poll the running child without blocking.
    pub fn try_wait_child(&self) -> io::Result<Option<ExitStatus>> {
        let mut slot = match self.child.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        match slot.as_mut() {
            Some(child) => child.try_wait(),
            None => Ok(None),
        }
    }

    /// Release the child handle once it has been reaped.
    pub fn take_child(&self) -> Option<Child> {
        let mut slot = match self.child.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        slot.take()
    }

    /// Kill the running child, if any. Safe to call repeatedly.
    pub fn kill_child(&self) {
        if let Ok(mut slot) = self.child.lock() {
            if let Some(child) = slot.as_mut() {
                let _ = child.kill();
            }
        }
    }

    /// Answer a pending confirmation.
    pub fn answer(&self, yes: bool) {
        if let Ok(tx) = self.answer.lock() {
            let _ = tx.send(yes);
        }
    }

    /// Release the terminal for an interactive step.
    pub fn release_terminal(&self) {
        if let Ok(tx) = self.gate.lock() {
            let _ = tx.send(());
        }
    }
}

impl Default for Control {
    fn default() -> Self {
        // Only used for tests; real construction goes through `new`.
        let (tx, _rx) = mpsc::channel();
        let (gate, _grx) = mpsc::channel();
        Self {
            cancel: AtomicBool::new(false),
            child: Mutex::new(None),
            answer: Mutex::new(tx),
            gate: Mutex::new(gate),
        }
    }
}
