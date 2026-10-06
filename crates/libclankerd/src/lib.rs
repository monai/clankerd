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
//! * `images`: the image cache (pull, list, resolve); `merge`: layers to one tar;
//!   `rootdisk`: base root disk building and per-machine clones.
//! * `store`: atomic on-disk persistence of config and state (private).
//! * `guest`: host-side varlink client for clankerd-guestd (private).
//! * [`vmm`]: the [`Vmm`] trait and its implementations; the only seam tests fake.
//! * `error`: [`Error`] and [`ErrorKind`] (Docker's kinds).
//!
//! Machines outlive the process that started them: state lives on disk, the
//! VMM runs in its own process group, and [`Engine::new`] reattaches.

mod config;
mod engine;
mod error;
mod guest;
mod images;
mod machine;
mod merge;
mod rootdisk;
mod state;
mod store;
pub mod vmm;

pub use config::{HostConfig, MachineConfig};
pub use engine::{Engine, EngineConfig, ROOT_DISK};
pub use error::{Error, ErrorKind, Result};
pub use images::{ImageInfo, LayerInfo};
pub use machine::{Machine, MachineInfo};
pub use rootdisk::{DiskPopulator, LocalGuestdPopulator};
pub use state::{MachineState, Status, WaitResult};
pub use vmm::Vmm;
