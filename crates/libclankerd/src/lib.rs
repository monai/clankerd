//! libclankerd: the machine library (Docker Engine API naming).
//!
//! A *machine* is one Linux VM. [`Engine`] is the entry point: it creates,
//! looks up and lists machines; [`Machine`] is a handle with `inspect`, `start`,
//! `wait` and `remove`.
//!
//! Module layout (one concern each; later tickets add modules, they do not
//! reshape these):
//!
//! * `engine`: [`Engine`], [`EngineConfig`], shared internal state.
//! * `machine`: [`Machine`] handle and the start/monitor/reattach lifecycle.
//! * `config`: [`MachineConfig`] (what runs) and [`HostConfig`] (how it is hosted).
//! * `state`: [`MachineState`], [`Status`], [`WaitResult`].
//! * `store`: atomic on-disk persistence of config and state (private).
//! * `exec`: [`Exec`] lifecycle (create, start, resize, kill, inspect) and its streams.
//! * `guest`: host-side varlink client for clankerd-guestd (private).
//! * [`vmm`]: the [`Vmm`] trait and its implementations; the only seam tests fake.
//! * `error`: [`Error`] and [`ErrorKind`] (Docker's kinds).
//!
//! Machines outlive the process that started them: state lives on disk, the
//! VMM runs in its own process group, and [`Engine::new`] reattaches.

mod config;
mod engine;
mod error;
mod exec;
mod guest;
mod machine;
mod state;
mod store;
pub mod vmm;

pub use config::{HostConfig, MachineConfig};
pub use engine::{Engine, EngineConfig};
pub use error::{Error, ErrorKind, Result};
pub use exec::{Exec, ExecConfig, ExecInfo, ExecOutput, ExecStatus, ExecStdin, ExecStreams};
pub use machine::{Machine, MachineInfo};
pub use state::{MachineState, Status, WaitResult};
pub use vmm::Vmm;
