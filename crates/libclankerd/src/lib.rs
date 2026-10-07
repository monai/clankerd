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
//! * `image_config`: the OCI image config and Docker's merge rules over it;
//!   `images`: the image cache (pull, list, resolve); `merge`: layers to one tar;
//!   `rootdisk`: base root disk building and per-machine clones.
//! * `events`: the events stream ([`Events`], [`MachineEvent`]).
//! * `store`: atomic on-disk persistence of config and state (private).
//! * `exec`: [`Exec`] lifecycle (create, start, resize, kill, inspect) and its streams.
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
mod events;
mod exec;
mod guest;
mod image_config;
mod images;
mod machine;
mod merge;
mod mount;
mod rootdisk;
mod state;
mod store;
mod tunnel;
pub mod vmm;
mod vmspawn_populator;
mod volumes;

pub use config::{HostConfig, MachineConfig, PortBinding, RestartPolicy, SocketBinding};
pub use engine::{Engine, EngineConfig, ROOT_DISK};
pub use error::{Error, ErrorKind, Result};
pub use events::{EventAction, Events, MachineEvent};
pub use exec::{Exec, ExecConfig, ExecInfo, ExecOutput, ExecStatus, ExecStdin, ExecStreams};
pub use image_config::ImageConfig;
pub use images::{ImageInfo, LayerInfo};
pub use machine::{Machine, MachineInfo};
pub use mount::{Mount, parse_size};
pub use rootdisk::{DiskPopulator, LocalGuestdPopulator};
pub use state::{MachineState, Status, WaitResult};
pub use tunnel::{GuestBinding, PublishedPort};
pub use vmm::Vmm;
pub use vmspawn_populator::VmspawnPopulator;
pub use volumes::{DEFAULT_SIZE as DEFAULT_VOLUME_SIZE, VolumeInfo};
