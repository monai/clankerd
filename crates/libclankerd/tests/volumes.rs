//! Seam A: named volumes and bind mounts. The stand-in VMM does not mount
//! anything (that is guestd's `--boot` job, verified on the M2); these tests
//! cover what the library owns: the sparse file on the host, its lifetime, the
//! size it asks the guest for, and the mounts handed to the VMM.

mod common;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use clankerd_proto::guest::GuestMount;
use common::*;
use libclankerd::vmm::{BootHandle, BootSpec, LocalProcessVmm};
use libclankerd::{Engine, ErrorKind, HostConfig, Mount, Vmm};

const MIB: u64 = 1 << 20;

/// Wraps the local-process VMM and remembers every `BootSpec` it was given.
struct Recording {
    inner: LocalProcessVmm,
    specs: Mutex<Vec<BootSpec>>,
}

impl Vmm for Recording {
    fn boot(&self, spec: &BootSpec) -> libclankerd::Result<BootHandle> {
        self.specs.lock().unwrap().push(spec.clone());
        self.inner.boot(spec)
    }
}

fn recording(env: &Env) -> (Engine, Arc<Recording>) {
    let vmm = Arc::new(Recording {
        inner: LocalProcessVmm::new(guestd_path()),
        specs: Mutex::default(),
    });
    let engine = Engine::new(env.config(vmm.clone())).unwrap();
    (engine, vmm)
}

fn with_mounts(mounts: Vec<Mount>) -> HostConfig {
    HostConfig {
        mounts,
        ..Default::default()
    }
}

fn volume_file(env: &Env, name: &str) -> PathBuf {
    env.root()
        .join("state/volumes")
        .join(name)
        .join("data.ext4")
}

#[test]
fn a_new_volume_is_a_sparse_file_of_the_requested_size() {
    let env = Env::new();
    let (engine, _) = recording(&env);
    let mount = Mount::volume("data", "/storage").with_size(64 * MIB);
    engine
        .create(Some("a"), sh("true"), with_mounts(vec![mount]))
        .unwrap();

    let file = volume_file(&env, "data");
    let meta = std::fs::metadata(&file).unwrap();
    assert_eq!(meta.len(), 64 * MIB);
    // Sparse: nothing but the hole was allocated (no fs work on the host).
    assert_eq!(std::os::unix::fs::MetadataExt::blocks(&meta), 0);
    assert!(std::fs::read(&file).unwrap().iter().all(|&b| b == 0));

    let info = engine.volume("data").unwrap();
    assert_eq!(info.name, "data");
    assert_eq!(info.size, 64 * MIB);
    assert_eq!(info.path, file);
    let names: Vec<_> = engine
        .volumes()
        .unwrap()
        .into_iter()
        .map(|v| v.name)
        .collect();
    assert_eq!(names, ["data"]);
}

#[test]
fn the_vmm_gets_the_volume_disk_and_the_guest_gets_the_mount() {
    let env = Env::new();
    let (engine, vmm) = recording(&env);
    let mount = Mount::volume("data", "/storage").with_size(32 * MIB);
    let m = engine
        .create(Some("a"), sh("true"), with_mounts(vec![mount]))
        .unwrap();
    m.start().unwrap();
    m.wait().unwrap();

    let specs = vmm.specs.lock().unwrap();
    assert_eq!(
        specs[0].volume_disk.as_deref(),
        Some(volume_file(&env, "data").as_path())
    );
    assert_eq!(
        specs[0].workload.mounts,
        [GuestMount::Volume {
            // No root disk in the stand-in, so the volume is the first device.
            device: "/dev/vda".into(),
            target: "/storage".into(),
            size: 32 * MIB
        }]
    );
}

#[test]
fn a_volume_survives_stop_start_and_rm_and_is_reused_by_a_new_machine() {
    let env = Env::new();
    let (engine, _) = recording(&env);
    let mount = Mount::volume("data", "/storage").with_size(16 * MIB);
    let m = engine
        .create(Some("a"), sh("true"), with_mounts(vec![mount.clone()]))
        .unwrap();
    m.start().unwrap();
    m.wait().unwrap();
    // The "guest" writes into the volume: here, directly into the file.
    let file = volume_file(&env, "data");
    write_marker(&file);

    m.start().unwrap();
    m.wait().unwrap();
    assert_eq!(read_marker(&file), b"kept");

    m.remove(false).unwrap();
    assert_eq!(read_marker(&file), b"kept");
    assert_eq!(engine.volumes().unwrap().len(), 1);

    // A new machine mounts the same volume without recreating it.
    let b = engine
        .create(Some("b"), sh("true"), with_mounts(vec![mount]))
        .unwrap();
    b.start().unwrap();
    b.wait().unwrap();
    assert_eq!(read_marker(&file), b"kept");
}

fn write_marker(file: &std::path::Path) {
    use std::io::{Seek, SeekFrom, Write};
    let mut f = std::fs::OpenOptions::new().write(true).open(file).unwrap();
    f.seek(SeekFrom::Start(4096)).unwrap();
    f.write_all(b"kept").unwrap();
}

fn read_marker(file: &std::path::Path) -> Vec<u8> {
    std::fs::read(file).unwrap()[4096..4100].to_vec()
}

#[test]
fn remove_with_volumes_deletes_the_machines_volume() {
    let env = Env::new();
    let (engine, _) = recording(&env);
    let m = engine
        .create(
            Some("a"),
            sh("true"),
            with_mounts(vec![Mount::volume("data", "/storage")]),
        )
        .unwrap();
    m.remove_with_volumes(false).unwrap();
    assert!(!volume_file(&env, "data").exists());
    assert!(engine.volumes().unwrap().is_empty());
    assert_eq!(
        engine.volume("data").unwrap_err().kind(),
        ErrorKind::NotFound
    );
}

#[test]
fn remove_with_volumes_refuses_a_volume_another_machine_uses() {
    let env = Env::new();
    let (engine, _) = recording(&env);
    let host = || with_mounts(vec![Mount::volume("data", "/storage")]);
    let a = engine.create(Some("a"), sh("true"), host()).unwrap();
    engine.create(Some("b"), sh("true"), host()).unwrap();
    let err = a.remove_with_volumes(false).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Conflict);
    // Nothing was removed.
    assert!(volume_file(&env, "data").exists());
    engine.get("a").unwrap();
}

#[test]
fn a_larger_size_grows_the_file_and_is_passed_to_the_guest_at_the_next_start() {
    let env = Env::new();
    let (engine, vmm) = recording(&env);
    let small = Mount::volume("data", "/storage").with_size(16 * MIB);
    let a = engine
        .create(Some("a"), sh("true"), with_mounts(vec![small]))
        .unwrap();
    a.start().unwrap();
    a.wait().unwrap();
    a.remove(false).unwrap();

    let big = Mount::volume("data", "/storage").with_size(48 * MIB);
    let b = engine
        .create(Some("b"), sh("true"), with_mounts(vec![big]))
        .unwrap();
    // Takes effect at start, not at create.
    assert_eq!(
        std::fs::metadata(volume_file(&env, "data")).unwrap().len(),
        16 * MIB
    );
    b.start().unwrap();
    b.wait().unwrap();

    assert_eq!(
        std::fs::metadata(volume_file(&env, "data")).unwrap().len(),
        48 * MIB
    );
    assert_eq!(engine.volume("data").unwrap().size, 48 * MIB);
    let specs = vmm.specs.lock().unwrap();
    assert!(matches!(
        specs[1].workload.mounts[0],
        GuestMount::Volume { size, .. } if size == 48 * MIB
    ));
}

#[test]
fn a_smaller_size_never_shrinks_the_volume() {
    let env = Env::new();
    let (engine, vmm) = recording(&env);
    let big = Mount::volume("data", "/storage").with_size(32 * MIB);
    engine
        .create(Some("a"), sh("true"), with_mounts(vec![big]))
        .unwrap();
    let small = Mount::volume("data", "/storage").with_size(8 * MIB);
    let b = engine
        .create(Some("b"), sh("true"), with_mounts(vec![small]))
        .unwrap();
    b.start().unwrap();
    b.wait().unwrap();
    assert_eq!(
        std::fs::metadata(volume_file(&env, "data")).unwrap().len(),
        32 * MIB
    );
    let specs = vmm.specs.lock().unwrap();
    assert!(matches!(
        specs[0].workload.mounts[0],
        GuestMount::Volume { size, .. } if size == 32 * MIB
    ));
}

#[test]
fn size_comes_from_driver_opts_and_accepts_suffixes() {
    let env = Env::new();
    let (engine, _) = recording(&env);
    let mount = Mount::Volume {
        name: "data".into(),
        target: "/storage".into(),
        driver_opts: [("size".to_string(), "2M".to_string())].into(),
    };
    engine
        .create(Some("a"), sh("true"), with_mounts(vec![mount]))
        .unwrap();
    assert_eq!(engine.volume("data").unwrap().size, 2 * MIB);

    let bad = Mount::Volume {
        name: "other".into(),
        target: "/o".into(),
        driver_opts: [("size".to_string(), "lots".to_string())].into(),
    };
    let err = engine
        .create(Some("b"), sh("true"), with_mounts(vec![bad]))
        .map(|_| ())
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidParameter);
}

#[test]
fn a_machine_cannot_start_while_another_running_machine_holds_its_volume() {
    let env = Env::new();
    let (engine, _) = recording(&env);
    let host = || with_mounts(vec![Mount::volume("data", "/storage")]);
    let a = engine.create(Some("a"), sh("sleep 30"), host()).unwrap();
    a.start().unwrap();
    let b = engine.create(Some("b"), sh("true"), host()).unwrap();
    assert_eq!(b.start().unwrap_err().kind(), ErrorKind::Conflict);
    a.kill(libc::SIGKILL).unwrap();
    b.start().unwrap();
}

#[test]
fn bind_mounts_reach_the_vmm_as_shares_and_the_guest_as_mounts() {
    let env = Env::new();
    let (engine, vmm) = recording(&env);
    let src = env.root().join("project");
    std::fs::create_dir(&src).unwrap();
    let mounts = vec![
        Mount::bind(&src, "/workspace"),
        Mount::Bind {
            source: src.clone(),
            target: "/ro".into(),
            read_only: true,
        },
    ];
    let m = engine
        .create(Some("a"), sh("true"), with_mounts(mounts))
        .unwrap();
    m.start().unwrap();
    m.wait().unwrap();

    let specs = vmm.specs.lock().unwrap();
    let tags: Vec<_> = specs[0].shares.iter().map(|s| s.tag.clone()).collect();
    assert_eq!(tags, ["bind0", "bind1"]);
    assert!(specs[0].shares.iter().all(|s| s.path == src));
    assert_eq!(
        specs[0].workload.mounts,
        [
            GuestMount::Bind {
                tag: "bind0".into(),
                target: "/workspace".into(),
                read_only: false
            },
            GuestMount::Bind {
                tag: "bind1".into(),
                target: "/ro".into(),
                read_only: true
            },
        ]
    );
    assert!(specs[0].volume_disk.is_none());
}

#[test]
fn invalid_mounts_are_rejected_at_create() {
    let env = Env::new();
    let (engine, _) = recording(&env);
    let dir = env.root().join("d");
    std::fs::create_dir(&dir).unwrap();
    let cases: Vec<(&str, Vec<Mount>)> = vec![
        ("relative target", vec![Mount::volume("data", "storage")]),
        ("dotdot target", vec![Mount::volume("data", "/a/../b")]),
        ("bad volume name", vec![Mount::volume("../x", "/s")]),
        (
            "two volumes",
            vec![Mount::volume("a", "/a"), Mount::volume("b", "/b")],
        ),
        (
            "duplicate target",
            vec![Mount::volume("a", "/a"), Mount::bind(&dir, "/a")],
        ),
        (
            "missing bind source",
            vec![Mount::bind(env.root().join("nope"), "/x")],
        ),
        ("relative bind source", vec![Mount::bind("rel", "/x")]),
    ];
    for (what, mounts) in cases {
        let err = engine
            .create(None, sh("true"), with_mounts(mounts))
            .map(|_| ())
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidParameter, "{what}: {err}");
    }
    assert!(engine.list(true).unwrap().is_empty());
    assert!(
        engine.volumes().unwrap().is_empty(),
        "no volume left behind"
    );
}

#[test]
fn volumes_can_be_created_and_removed_on_their_own() {
    let env = Env::new();
    let (engine, _) = recording(&env);
    let v = engine.create_volume("scratch", Some(4 * MIB)).unwrap();
    assert_eq!(v.size, 4 * MIB);
    assert_eq!(
        engine.create_volume("scratch", None).unwrap_err().kind(),
        ErrorKind::Conflict
    );
    engine.remove_volume("scratch").unwrap();
    assert_eq!(
        engine.remove_volume("scratch").unwrap_err().kind(),
        ErrorKind::NotFound
    );

    // In use: refuse.
    engine
        .create(
            Some("a"),
            sh("true"),
            with_mounts(vec![Mount::volume("data", "/storage")]),
        )
        .unwrap();
    assert_eq!(
        engine.remove_volume("data").unwrap_err().kind(),
        ErrorKind::Conflict
    );
}
