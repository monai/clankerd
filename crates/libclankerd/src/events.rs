//! The events stream: one [`MachineEvent`] per state change, in order.

use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, SystemTime};

/// What happened to a machine (Docker's event actions, past tense).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventAction {
    Created,
    Started,
    /// The machine ended; see [`MachineEvent::exit_code`].
    Exited,
    /// A restart policy is about to start the machine again.
    Restarting,
    /// A signal was sent to the machine through the API.
    Killed,
    /// Its host configuration or root disk changed.
    Updated,
    Removed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MachineEvent {
    pub action: EventAction,
    pub machine_id: String,
    pub machine_name: String,
    pub time: SystemTime,
    /// Set for [`EventAction::Exited`]; `-1` when the machine vanished.
    pub exit_code: Option<i32>,
}

/// Fan-out to every subscriber.
#[derive(Default)]
pub(crate) struct EventBus {
    subscribers: Mutex<Vec<Sender<MachineEvent>>>,
}

impl EventBus {
    pub fn subscribe(&self) -> Events {
        let (tx, rx) = mpsc::channel();
        self.subscribers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(tx);
        Events { rx }
    }

    /// Delivers to every live subscriber; dropped ones are forgotten.
    pub fn emit(&self, event: MachineEvent) {
        self.subscribers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|tx| tx.send(event.clone()).is_ok());
    }
}

/// A subscription: sees the events emitted after it was created. Dropping it
/// unsubscribes.
pub struct Events {
    rx: Receiver<MachineEvent>,
}

impl Events {
    /// The next event, or `None` if none arrives within `timeout`.
    pub fn next_timeout(&mut self, timeout: Duration) -> Option<MachineEvent> {
        self.rx.recv_timeout(timeout).ok()
    }
}

impl Iterator for Events {
    type Item = MachineEvent;

    /// Blocks until the next event.
    fn next(&mut self) -> Option<MachineEvent> {
        self.rx.recv().ok()
    }
}
