//! Image cache and root-disk tests through the public `Engine` API, against an
//! in-process registry. The only double is the `DiskPopulator` (it stands in
//! for the population boot; the real one is exercised by seam B tests), and it
//! stores the merged tar it is given so tests can inspect it.

mod common;

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::registry::{Registry, layer};
use common::{Env, guestd_path, sh};
use libclankerd::vmm::LocalProcessVmm;
use libclankerd::{DiskPopulator, Engine, ErrorKind, HostConfig, ROOT_DISK};

/// "Populates" a disk by storing the tar it was handed, and counts its runs.
#[derive(Default)]
struct TarCapture {
    runs: AtomicUsize,
}

impl DiskPopulator for TarCapture {
    fn populate(&self, disk: &Path, size: u64, tar: &mut dyn Read) -> libclankerd::Result<()> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        assert!(size >= 1 << 30, "root disks leave headroom: {size}");
        let mut out = std::fs::File::create(disk)?;
        std::io::copy(tar, &mut out)?;
        Ok(())
    }
}

struct Fixture {
    env: Env,
    registry: Registry,
    populator: Arc<TarCapture>,
    engine: Engine,
}

fn fixture() -> Fixture {
    let env = Env::new();
    let registry = Registry::start();
    let populator = Arc::new(TarCapture::default());
    let mut cfg = env.config(Arc::new(LocalProcessVmm::new(guestd_path())));
    cfg.cache_dir = Some(env.root().join("cache"));
    cfg.populator = Some(populator.clone());
    cfg.insecure_registries = vec![registry.addr.clone()];
    let engine = Engine::new(cfg).unwrap();
    Fixture {
        env,
        registry,
        populator,
        engine,
    }
}

/// Paths and contents of the regular files in a tar.
fn tar_files(path: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut ar = tar::Archive::new(std::fs::File::open(path).unwrap());
    ar.entries()
        .unwrap()
        .map(|e| {
            let mut e = e.unwrap();
            let name = e
                .path()
                .unwrap()
                .to_string_lossy()
                .trim_end_matches('/')
                .to_owned();
            let mut data = Vec::new();
            e.read_to_end(&mut data).unwrap();
            (name, data)
        })
        .collect()
}

#[test]
fn pull_caches_the_image_and_images_lists_it_with_its_digest() {
    let f = fixture();
    let digest = f
        .registry
        .push("test/app", "v1", &[layer(&[("hello", b"hi")])]);
    let reference = format!("{}/test/app:v1", f.registry.addr);

    let info = f.engine.pull(&reference).unwrap();
    assert_eq!(info.id, digest);
    assert_eq!(info.references, vec![reference.clone()]);
    assert_eq!(info.layers.len(), 1);

    let listed = f.engine.images().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, digest);
}

#[test]
fn pulling_an_unknown_image_is_unavailable() {
    let f = fixture();
    let err = f
        .engine
        .pull(&format!("{}/test/missing:v1", f.registry.addr))
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Unavailable);
}

#[test]
fn pulling_an_invalid_reference_is_an_invalid_parameter() {
    let f = fixture();
    let err = f.engine.pull("Not A Reference!").unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidParameter);
}

#[test]
fn create_pins_the_digest_and_builds_the_root_disk_from_the_merged_layers() {
    let f = fixture();
    let digest = f.registry.push(
        "test/app",
        "v1",
        &[
            layer(&[("a/keep", b"1"), ("a/gone", b"2"), ("c", b"base")]),
            // A whiteout removes a/gone; c is overwritten.
            layer(&[("a/.wh.gone", b""), ("c", b"new"), ("b", b"3")]),
        ],
    );
    let reference = format!("{}/test/app:v1", f.registry.addr);

    // Not pulled beforehand: create pulls what is missing.
    let m = f
        .engine
        .create(Some("one"), image_config(&reference), HostConfig::default())
        .unwrap();
    let info = m.inspect().unwrap();
    assert_eq!(info.image_id, digest);
    assert_eq!(info.config.image, reference);

    let disk = f
        .env
        .root()
        .join("state/machines")
        .join(m.id())
        .join(ROOT_DISK);
    let files = tar_files(&disk);
    assert_eq!(files.get("a/keep").map(Vec::as_slice), Some(&b"1"[..]));
    assert_eq!(files.get("c").map(Vec::as_slice), Some(&b"new"[..]));
    assert_eq!(files.get("b").map(Vec::as_slice), Some(&b"3"[..]));
    assert!(!files.contains_key("a/gone"), "whiteout was not applied");
    assert!(!files.keys().any(|k| k.contains(".wh.")));
}

#[test]
fn a_second_machine_from_the_same_image_reuses_the_cached_base() {
    let f = fixture();
    f.registry.push("test/app", "v1", &[layer(&[("x", b"y")])]);
    let reference = format!("{}/test/app:v1", f.registry.addr);

    let a = f
        .engine
        .create(Some("a"), image_config(&reference), HostConfig::default())
        .unwrap();
    let b = f
        .engine
        .create(Some("b"), image_config(&reference), HostConfig::default())
        .unwrap();

    assert_eq!(f.populator.runs.load(Ordering::SeqCst), 1);
    assert_eq!(a.inspect().unwrap().image_id, b.inspect().unwrap().image_id);
    let state = f.env.root().join("state/machines");
    let (da, db) = (
        state.join(a.id()).join(ROOT_DISK),
        state.join(b.id()).join(ROOT_DISK),
    );
    assert_eq!(std::fs::read(&da).unwrap(), std::fs::read(&db).unwrap());
    // Each machine owns its disk: writing one leaves the other untouched.
    std::fs::write(&da, b"changed").unwrap();
    assert_ne!(std::fs::read(&da).unwrap(), std::fs::read(&db).unwrap());
}

#[test]
fn machines_stay_pinned_when_the_tag_moves() {
    let f = fixture();
    let first = f
        .registry
        .push("test/app", "latest", &[layer(&[("v", b"1")])]);
    let reference = format!("{}/test/app:latest", f.registry.addr);
    let old = f
        .engine
        .create(Some("old"), image_config(&reference), HostConfig::default())
        .unwrap();

    let second = f
        .registry
        .push("test/app", "latest", &[layer(&[("v", b"2")])]);
    assert_ne!(first, second);
    f.engine.pull(&reference).unwrap();
    let new = f
        .engine
        .create(Some("new"), image_config(&reference), HostConfig::default())
        .unwrap();

    assert_eq!(old.inspect().unwrap().image_id, first);
    assert_eq!(new.inspect().unwrap().image_id, second);
    assert_eq!(f.populator.runs.load(Ordering::SeqCst), 2);
    let refs: Vec<_> = f
        .engine
        .images()
        .unwrap()
        .into_iter()
        .map(|i| (i.id, i.references.len()))
        .collect();
    // The moved tag names only the new image.
    assert!(refs.contains(&(second, 1)));
    assert!(refs.contains(&(first, 0)));
}

#[test]
fn create_without_an_image_cache_keeps_working_unpinned() {
    let env = Env::new();
    let engine = env.engine();
    let m = engine
        .create(Some("plain"), sh("true"), HostConfig::default())
        .unwrap();
    assert_eq!(m.inspect().unwrap().image_id, "");
    assert!(
        !env.root()
            .join("state/machines")
            .join(m.id())
            .join(ROOT_DISK)
            .exists()
    );
}

fn image_config(reference: &str) -> libclankerd::MachineConfig {
    let mut c = sh("true");
    c.image = reference.to_owned();
    c
}
