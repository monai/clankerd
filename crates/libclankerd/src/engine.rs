//! `Engine`: the library's entry point.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use crate::config::{HostConfig, MachineConfig, PullPolicy};
use crate::error::{Error, Result};
use crate::events::EventAction;
use crate::events::MachineEvent;
use crate::events::{EventBus, Events};
use crate::image_config::ImageConfig;
use crate::images::{ImageInfo, ImageStore};
use crate::machine::{Machine, MachineInfo};
use crate::rootdisk::{DiskPopulator, clone_file, ensure_base};
use crate::state::MachineState;
use crate::store::{Record, Store};
use crate::tunnel::{MachineTunnels, validate_host_config};
use crate::vmm::{UnavailableVmm, Vmm};
use crate::volumes::{VolumeInfo, VolumeStore};

/// File name of a machine's root disk inside its state directory.
pub const ROOT_DISK: &str = "root.ext4";

pub struct EngineConfig {
    /// Persistent per-machine config and state.
    pub state_dir: PathBuf,
    /// Short-lived sockets. Kept apart because unix socket paths are length-limited.
    pub runtime_dir: PathBuf,
    pub vmm: Arc<dyn Vmm>,
    /// How long `Machine::start` waits for the guest's ready event.
    pub start_timeout: Duration,
    /// Image cache (blobs, image records, base root disks). When `None`, images
    /// are not resolved: machines are created without a pinned digest or root disk.
    pub cache_dir: Option<PathBuf>,
    /// Builds base root disks; required to create machines when `cache_dir` is set.
    pub populator: Option<Arc<dyn DiskPopulator>>,
    /// Delay before the first restart under a restart policy; it doubles with
    /// each quick failure (Docker starts at 100 ms).
    pub restart_delay: Duration,
    /// Registries (`host:port`) reached over plain HTTP, like Docker's
    /// `insecure-registries`.
    pub insecure_registries: Vec<String>,
}

impl EngineConfig {
    /// A configuration with no usable VMM; set [`EngineConfig::vmm`].
    pub fn new(state_dir: impl Into<PathBuf>, runtime_dir: impl Into<PathBuf>) -> Self {
        EngineConfig {
            state_dir: state_dir.into(),
            runtime_dir: runtime_dir.into(),
            vmm: Arc::new(UnavailableVmm),
            start_timeout: Duration::from_secs(30),
            restart_delay: Duration::from_millis(100),
            cache_dir: None,
            populator: None,
            insecure_registries: Vec::new(),
        }
    }

    /// Per-user default image cache location (`~/Library/Caches/clankerd` on macOS).
    pub fn default_cache_dir() -> PathBuf {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        if cfg!(target_os = "macos") {
            home.join("Library/Caches/clankerd")
        } else {
            std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".cache"))
                .join("clankerd")
        }
    }

    /// Per-user default locations, all named `clankerd`.
    pub fn for_user() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let state = if cfg!(target_os = "macos") {
            home.join("Library/Application Support/clankerd")
        } else {
            std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local/share"))
                .join("clankerd")
        };
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(|d| PathBuf::from(d).join("clankerd"))
            // SAFETY: getuid has no preconditions.
            .unwrap_or_else(|| {
                std::env::temp_dir().join(format!("clankerd-{}", unsafe { libc::getuid() }))
            });
        Self::new(state, runtime)
    }
}

/// State shared by `Engine` and every `Machine` handle (and monitor threads).
pub(crate) struct Inner {
    pub store: Store,
    pub runtime_dir: PathBuf,
    pub vmm: Arc<dyn Vmm>,
    pub start_timeout: Duration,
    pub images: Option<ImageStore>,
    pub populator: Option<Arc<dyn DiskPopulator>>,
    /// Serialises state read-modify-write; holds ids whose start is in flight.
    pub guarded: Mutex<HashSet<String>>,
    /// Signalled on every state change.
    pub changed: Condvar,
    /// Machines whose VMM was killed on purpose (their end is exit code 137, not `dead`).
    pub forced: Mutex<HashSet<String>>,
    /// Machines stopped through the API: their policy does not restart them.
    pub stopped: Mutex<HashSet<String>>,
    pub restart_delay: Duration,
    /// Tunnel resources of running machines, by id.
    pub tunnels: Mutex<HashMap<String, Arc<MachineTunnels>>>,
    pub events: EventBus,
}

impl Inner {
    pub fn lock(&self) -> MutexGuard<'_, HashSet<String>> {
        self.guarded.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Atomically modifies a machine's persisted state and wakes waiters.
    pub fn update_state(&self, id: &str, f: impl FnOnce(&mut MachineState)) -> Result<()> {
        let _g = self.lock();
        let mut state = self.store.load_state(id)?;
        f(&mut state);
        self.store.save_state(id, &state)?;
        self.changed.notify_all();
        Ok(())
    }

    pub fn forced(&self) -> MutexGuard<'_, HashSet<String>> {
        self.forced.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn stopped(&self) -> MutexGuard<'_, HashSet<String>> {
        self.stopped.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn tunnels(&self) -> MutexGuard<'_, HashMap<String, Arc<MachineTunnels>>> {
        self.tunnels.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Closes the host side of a machine's tunnels (listeners, host endpoint).
    /// Emits an event for a machine (best effort: a machine already gone is skipped).
    pub fn emit(&self, id: &str, action: EventAction, exit_code: Option<i32>) {
        let name = match self.store.load(id) {
            Ok((record, _)) => record.name,
            Err(_) => return,
        };
        self.emit_named(id, name, action, exit_code);
    }

    pub fn emit_named(&self, id: &str, name: String, action: EventAction, exit_code: Option<i32>) {
        self.events.emit(MachineEvent {
            action,
            machine_id: id.to_owned(),
            machine_name: name,
            time: SystemTime::now(),
            exit_code,
        });
    }

    pub fn drop_tunnels(&self, id: &str) {
        let removed = self.tunnels().remove(id);
        drop(removed);
    }

    /// Where the guest dials the host (stands in for a vsock port).
    pub fn host_socket_path(&self, id: &str) -> PathBuf {
        self.runtime_dir.join(format!("{}.host", &id[..12]))
    }

    pub fn socket_path(&self, id: &str) -> PathBuf {
        self.runtime_dir.join(format!("{}.sock", &id[..12]))
    }

    /// The cached image for `reference` per `policy`, and its cached base root disk.
    pub fn resolve_image(
        &self,
        reference: &str,
        policy: PullPolicy,
    ) -> Result<(ImageInfo, ImageConfig, PathBuf)> {
        let store = self
            .images
            .as_ref()
            .ok_or_else(|| Error::unavailable("no image cache configured"))?;
        let image = match (policy, store.find(reference)?) {
            (PullPolicy::Missing, Some(image)) | (PullPolicy::Never, Some(image)) => image,
            (PullPolicy::Never, None) => {
                return Err(Error::not_found(format!(
                    "no such image: {reference} (pull policy is never)"
                )));
            }
            (PullPolicy::Missing, None) | (PullPolicy::Always, _) => store.pull(reference)?,
        };
        let defaults = ImageConfig::parse(&image.config)?;
        let populator = self
            .populator
            .as_deref()
            .ok_or_else(|| Error::unavailable("no root-disk populator configured"))?;
        let (base, _) = ensure_base(store, populator, &image)?;
        Ok((image, defaults, base))
    }

    /// Finds a machine by exact name, exact id or unique id prefix.
    pub fn resolve(&self, key: &str) -> Result<Record> {
        let _g = self.lock();
        self.resolve_locked(key)
    }

    fn resolve_locked(&self, key: &str) -> Result<Record> {
        let not_found = || Error::not_found(format!("no such machine: {key}"));
        if key.is_empty() {
            return Err(not_found());
        }
        let mut by_prefix = Vec::new();
        for id in self.store.ids()? {
            let (record, _) = self.store.load(&id)?;
            if record.name == key || record.id == key {
                return Ok(record);
            }
            if record.id.starts_with(key) {
                by_prefix.push(record);
            }
        }
        match by_prefix.len() {
            0 => Err(not_found()),
            1 => Ok(by_prefix.remove(0)),
            _ => Err(Error::invalid_parameter(format!(
                "multiple machines match id prefix {key}"
            ))),
        }
    }
}

#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

impl Engine {
    /// Opens (creating if needed) the state and reattaches to machines left
    /// running by a previous process, recording the exit of those that ended.
    pub fn new(config: EngineConfig) -> Result<Self> {
        fs::create_dir_all(&config.runtime_dir)?;
        let inner = Arc::new(Inner {
            store: Store::open(&config.state_dir)?,
            runtime_dir: config.runtime_dir,
            vmm: config.vmm,
            start_timeout: config.start_timeout,
            images: match &config.cache_dir {
                Some(dir) => Some(ImageStore::open(dir, config.insecure_registries)?),
                None => None,
            },
            populator: config.populator,
            guarded: Mutex::default(),
            changed: Condvar::new(),
            forced: Mutex::default(),
            stopped: Mutex::default(),
            restart_delay: config.restart_delay,
            tunnels: Mutex::default(),
            events: EventBus::default(),
        });
        for id in inner.store.ids()? {
            crate::machine::reattach(&inner, &id);
        }
        Ok(Engine { inner })
    }

    /// Creates a machine in state `created`.
    pub fn create(
        &self,
        name: Option<&str>,
        config: MachineConfig,
        host_config: HostConfig,
    ) -> Result<Machine> {
        if config.image.is_empty() {
            return Err(Error::invalid_parameter("image is required"));
        }
        // With an image cache the image may supply the command (merged below).
        if self.inner.images.is_none() && config.entrypoint.is_empty() && config.cmd.is_empty() {
            return Err(Error::invalid_parameter("no command specified"));
        }
        validate_host_config(&host_config)?;
        crate::mount::validate(&host_config.mounts)?;
        if let Some(name) = name {
            validate_name(name)?;
        }
        let id = random_id()?;
        let name = name.map_or_else(|| format!("machine-{}", &id[..8]), str::to_owned);

        // Resolve and pin the image, and build its base disk, before taking the
        // lock: pulling and building can take minutes.
        let requested = config.clone();
        let mut config = config;
        let mut image_config = None;
        let base = if self.inner.images.is_some() {
            let (image, defaults, base) = self
                .inner
                .resolve_image(&config.image, host_config.pull_policy)?;
            // What runs is decided now, from the pinned image: the stored
            // configuration is the merged one, as in Docker's inspect.
            config = defaults.merge(&config)?;
            image_config = Some(defaults);
            Some((image.id, base))
        } else {
            None
        };

        let mut new_volume = None;
        {
            let _g = self.inner.lock();
            for existing in self.inner.store.ids()? {
                let (record, _) = self.inner.store.load(&existing)?;
                if record.name == name {
                    return Err(Error::conflict(format!(
                        "machine name \"{name}\" is already in use by {}",
                        &record.id[..12]
                    )));
                }
            }
            let volumes = self.volume_store();
            if crate::mount::ensure_volume(&volumes, &host_config.mounts)? {
                new_volume = crate::mount::volume_name(&host_config.mounts).map(str::to_owned);
            }
            let record = Record {
                id: id.clone(),
                name,
                created: SystemTime::now(),
                config,
                requested: Some(requested),
                host_config,
                image_id: base.as_ref().map(|(id, _)| id.clone()).unwrap_or_default(),
                image_config,
            };
            if let Err(e) = self.inner.store.create(&record, &MachineState::created()) {
                if let Some(v) = &new_volume {
                    let _ = volumes.remove(v);
                }
                return Err(e);
            }
        }
        if let Some((_, base)) = base {
            // The machine's root disk: `root.ext4` in its directory, a clone of the
            // cached base. A VMM attaches it as the guest's root block device.
            let disk = self.inner.store.machine_dir(&id).join(ROOT_DISK);
            if let Err(e) = clone_file(&base, &disk) {
                let _ = self.inner.store.remove(&id);
                if let Some(v) = &new_volume {
                    let _ = self.volume_store().remove(v);
                }
                return Err(e);
            }
        }
        self.inner.emit(&id, EventAction::Created, None);
        Ok(Machine::new(self.inner.clone(), id))
    }

    /// Subscribes to machine events: created, started, exited and so on, in
    /// the order they happen. Sees events from this point on.
    pub fn events(&self) -> Events {
        self.inner.events.subscribe()
    }

    /// Pulls `reference` into the image cache (always contacting the registry)
    /// and returns the cached image.
    pub fn pull(&self, reference: &str) -> Result<ImageInfo> {
        self.image_store()?.pull(reference)
    }

    /// Lists cached images, newest pull first.
    pub fn images(&self) -> Result<Vec<ImageInfo>> {
        self.image_store()?.list()
    }

    fn image_store(&self) -> Result<&ImageStore> {
        self.inner
            .images
            .as_ref()
            .ok_or_else(|| Error::unavailable("no image cache configured"))
    }

    fn volume_store(&self) -> VolumeStore {
        VolumeStore::new(self.inner.store.volumes_dir())
    }

    /// Lists named volumes by name.
    pub fn volumes(&self) -> Result<Vec<VolumeInfo>> {
        self.volume_store().list()
    }

    /// Inspects a named volume.
    pub fn volume(&self, name: &str) -> Result<VolumeInfo> {
        self.volume_store().get(name)
    }

    /// Creates a sparse volume (`size` bytes, default 16 GiB). Machines also
    /// create theirs on demand; this is for volumes made ahead of time.
    pub fn create_volume(&self, name: &str, size: Option<u64>) -> Result<VolumeInfo> {
        let _g = self.inner.lock();
        self.volume_store().create(name, size)
    }

    /// Deletes a volume and its data. Refused while a machine mounts it.
    pub fn remove_volume(&self, name: &str) -> Result<()> {
        let _g = self.inner.lock();
        let store = self.volume_store();
        store.get(name)?;
        if let Some(user) = crate::volumes::users_of(&self.inner, name, None)?.first() {
            return Err(Error::conflict(format!(
                "volume \"{name}\" is in use by machine {user}"
            )));
        }
        store.remove(name)
    }

    /// Looks a machine up by name, id or unique id prefix.
    pub fn get(&self, key: &str) -> Result<Machine> {
        let record = self.inner.resolve(key)?;
        Ok(Machine::new(self.inner.clone(), record.id))
    }

    /// Lists machines, newest first; only running ones unless `all`.
    pub fn list(&self, all: bool) -> Result<Vec<MachineInfo>> {
        let mut out = Vec::new();
        for id in self.inner.store.ids()? {
            // A machine removed since `ids()` is simply skipped.
            if let Ok(info) = Machine::new(self.inner.clone(), id).inspect()
                && (all || info.state.status == crate::Status::Running)
            {
                out.push(info);
            }
        }
        out.sort_by_key(|i| std::cmp::Reverse(i.created));
        Ok(out)
    }
}

fn validate_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    let ok = chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if ok {
        Ok(())
    } else {
        Err(Error::invalid_parameter(format!(
            "invalid machine name \"{name}\": must match [a-zA-Z0-9][a-zA-Z0-9_.-]*"
        )))
    }
}

fn random_id() -> Result<String> {
    use std::io::Read;
    let mut bytes = [0u8; 32];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
