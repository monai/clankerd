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
//! * `guest`: host-side varlink client for clankerd-guestd (private).
//! * [`vmm`]: the [`Vmm`] trait and its implementations; the only seam tests fake.
//! * `tunnel`: published ports, host-gateway ports and socket bindings.
//! * `error`: [`Error`] and [`ErrorKind`] (Docker's kinds).
//!
//! Machines outlive the process that started them: state lives on disk, the
//! VMM runs in its own process group, and [`Engine::new`] reattaches.

mod config;
mod engine;
mod error;
mod guest;
mod machine;
mod state;
mod store;
mod tunnel;
pub mod vmm;

pub use config::{HostConfig, MachineConfig, PortBinding, SocketBinding};
pub use engine::{Engine, EngineConfig};
pub use error::{Error, ErrorKind, Result};
pub use machine::{Machine, MachineInfo};
pub use state::{MachineState, Status, WaitResult};
pub use tunnel::{GuestBinding, PublishedPort};
pub use vmm::Vmm;
