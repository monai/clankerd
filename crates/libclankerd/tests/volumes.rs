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
fn volume_capacity_and_data_survive_discard_and_restart() {
    use std::io::{Read, Write};
    for discarded in [1, 4096, 65536, MIB] {
        let env = Env::new();
        let (engine, _) = recording(&env);
        let capacity = 32 * MIB;
        engine.create_volume("data", Some(capacity)).unwrap();
        let machine = engine
            .create(
                Some("a"),
                sh("sleep 60"),
                with_mounts(vec![Mount::volume("data", "/storage")]),
            )
            .unwrap();
        let path = volume_file(&env, "data");
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .write_all(b"marker")
            .unwrap();
        machine.start().unwrap();
        for _ in 0..2 {
            machine.stop(std::time::Duration::from_secs(10)).unwrap();
            std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap()
                .set_len(capacity - discarded)
                .unwrap();
            assert_eq!(engine.volume("data").unwrap().size, capacity);
            machine.start().unwrap();
            assert_eq!(std::fs::metadata(&path).unwrap().len(), capacity);
            let mut marker = [0; 6];
            std::fs::File::open(&path)
                .unwrap()
                .read_exact(&mut marker)
                .unwrap();
            assert_eq!(&marker, b"marker");
        }
        machine.stop(std::time::Duration::from_secs(10)).unwrap();
    }
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
fn volume_boot_mappings_preserve_order_targets_and_sizes_with_or_without_a_root() {
    use common::registry::{Registry, layer};
    const DEVICES: [&str; 28] = [
        "/dev/vda",
        "/dev/vdb",
        "/dev/vdc",
        "/dev/vdd",
        "/dev/vde",
        "/dev/vdf",
        "/dev/vdg",
        "/dev/vdh",
        "/dev/vdi",
        "/dev/vdj",
        "/dev/vdk",
        "/dev/vdl",
        "/dev/vdm",
        "/dev/vdn",
        "/dev/vdo",
        "/dev/vdp",
        "/dev/vdq",
        "/dev/vdr",
        "/dev/vds",
        "/dev/vdt",
        "/dev/vdu",
        "/dev/vdv",
        "/dev/vdw",
        "/dev/vdx",
        "/dev/vdy",
        "/dev/vdz",
        "/dev/vdaa",
        "/dev/vdab",
    ];
    for has_root in [false, true] {
        for count in [0, 1, 2, 27] {
            let env = Env::new();
            let vmm = Arc::new(Recording {
                inner: LocalProcessVmm::new(guestd_path()),
                specs: Mutex::default(),
            });
            let mut cfg = env.config(vmm.clone());
            let registry = Registry::start();
            let mut config = sh("true");
            if has_root {
                registry.push("test/app", "v1", &[layer(&[("example", b"foo")])]);
                cfg.cache_dir = Some(env.root().join("cache"));
                cfg.populator = Some(Arc::new(TarCapture::default()));
                cfg.insecure_registries = vec![registry.addr.clone()];
                config.image = format!("{}/test/app:v1", registry.addr);
            }
            let engine = Engine::new(cfg).unwrap();
            let mut mounts = Vec::new();
            let mut disks = Vec::new();
            let mut expected = Vec::new();
            for (index, device) in DEVICES[usize::from(has_root)..][..count].iter().enumerate() {
                let name = format!("volume{index}");
                let target = format!("/data/{index}");
                let size = (index as u64 + 2) * MIB;
                mounts.push(Mount::volume(&name, &target).with_size(size));
                disks.push(volume_file(&env, &name));
                expected.push(GuestMount::Volume {
                    device: (*device).into(),
                    target,
                    size,
                });
            }
            // A bind interleaved with volumes must not consume a block index.
            mounts.insert(count.min(1), Mount::bind(env.root(), "/shared"));
            expected.insert(
                count.min(1),
                GuestMount::Bind {
                    tag: "bind0".into(),
                    target: "/shared".into(),
                    read_only: false,
                },
            );
            let machine = engine
                .create(Some("example"), config, with_mounts(mounts))
                .unwrap();
            machine.start().unwrap();
            common::finish_workload_and_stop(&machine).unwrap();
            let specs = vmm.specs.lock().unwrap();
            assert_eq!(specs[0].root_disk.is_some(), has_root);
            assert_eq!(
                specs[0].volume_disks, disks,
                "root={has_root}, count={count}"
            );
            assert_eq!(
                specs[0].workload.mounts, expected,
                "root={has_root}, count={count}"
            );
        }
    }
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
    common::finish_workload_and_stop(&m).unwrap();
    // The "guest" writes into the volume: here, directly into the file.
    let file = volume_file(&env, "data");
    write_marker(&file);

    m.start().unwrap();
    common::finish_workload_and_stop(&m).unwrap();
    assert_eq!(read_marker(&file), b"kept");

    m.remove(false).unwrap();
    assert_eq!(read_marker(&file), b"kept");
    assert_eq!(engine.volumes().unwrap().len(), 1);

    // A new machine mounts the same volume without recreating it.
    let b = engine
        .create(Some("b"), sh("true"), with_mounts(vec![mount]))
        .unwrap();
    b.start().unwrap();
    common::finish_workload_and_stop(&b).unwrap();
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
fn remove_with_volumes_deletes_every_attached_volume() {
    for count in [0, 1, 2] {
        let env = Env::new();
        let (engine, _) = recording(&env);
        let mounts = (0..count)
            .map(|index| {
                Mount::volume(format!("volume{index}"), format!("/data/{index}")).with_size(MIB)
            })
            .collect();
        let machine = engine
            .create(Some("example"), sh("true"), with_mounts(mounts))
            .unwrap();
        machine.remove_with_volumes(false).unwrap();
        assert!(engine.volumes().unwrap().is_empty());
        assert_eq!(
            engine.get("example").err().unwrap().kind(),
            ErrorKind::NotFound
        );
        for index in 0..count {
            assert!(!volume_file(&env, &format!("volume{index}")).exists());
        }
    }
}

#[test]
fn remove_with_volumes_checks_every_other_user_before_deleting_anything() {
    for shared in ["first", "second"] {
        let env = Env::new();
        let (engine, _) = recording(&env);
        let machine = engine
            .create(
                Some("example"),
                sh("true"),
                with_mounts(vec![
                    Mount::volume("first", "/first").with_size(MIB),
                    Mount::volume("second", "/second").with_size(MIB),
                ]),
            )
            .unwrap();
        engine
            .create(
                Some("other"),
                sh("true"),
                with_mounts(vec![Mount::volume(shared, "/data")]),
            )
            .unwrap();
        for name in ["first", "second"] {
            write_marker(&volume_file(&env, name));
        }
        assert_eq!(
            machine.remove_with_volumes(false).unwrap_err().kind(),
            ErrorKind::Conflict
        );
        assert!(engine.get("example").is_ok());
        for name in ["first", "second"] {
            assert_eq!(read_marker(&volume_file(&env, name)), b"kept");
        }
    }
}

#[test]
fn volume_removal_rejects_references_at_every_attachment_position() {
    let env = Env::new();
    let (engine, _) = recording(&env);
    engine
        .create(
            Some("example"),
            sh("true"),
            with_mounts(vec![
                Mount::volume("first", "/first").with_size(MIB),
                Mount::volume("second", "/second").with_size(MIB),
            ]),
        )
        .unwrap();
    for name in ["first", "second"] {
        assert_eq!(
            engine.remove_volume(name).unwrap_err().kind(),
            ErrorKind::Conflict
        );
        assert!(engine.volume(name).is_ok());
    }
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
    common::finish_workload_and_stop(&a).unwrap();
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
    common::finish_workload_and_stop(&b).unwrap();

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
    common::finish_workload_and_stop(&b).unwrap();
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
fn starting_checks_every_volume_in_both_machines_for_exclusive_use() {
    for shared in ["first", "second"] {
        for candidate_position in [0, 1] {
            let env = Env::new();
            let (engine, _) = recording(&env);
            let owner = engine
                .create(
                    Some("owner"),
                    sh("sleep 60"),
                    with_mounts(vec![
                        Mount::volume("first", "/first").with_size(MIB),
                        Mount::volume("second", "/second").with_size(MIB),
                    ]),
                )
                .unwrap();
            owner.start().unwrap();
            let mut mounts = vec![Mount::volume("spare", "/spare").with_size(MIB)];
            mounts.insert(candidate_position, Mount::volume(shared, "/data"));
            let candidate = engine
                .create(Some("candidate"), sh("true"), with_mounts(mounts))
                .unwrap();
            assert_eq!(candidate.start().unwrap_err().kind(), ErrorKind::Conflict);
            owner.stop(std::time::Duration::from_secs(10)).unwrap();
            candidate.start().unwrap();
            common::finish_workload_and_stop(&candidate).unwrap();
        }
    }
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
    common::finish_workload_and_stop(&m).unwrap();

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
    assert!(specs[0].volume_disks.is_empty());
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
            "duplicate volume",
            vec![Mount::volume("a", "/a"), Mount::volume("a", "/b")],
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

#[test]
fn failed_collection_creation_removes_all_new_volumes_and_preserves_existing_data() {
    for new_count in [0, 1, 2, 3] {
        let env = Env::new();
        let (engine, _) = recording(&env);
        engine.create_volume("existing", Some(MIB)).unwrap();
        write_marker(&volume_file(&env, "existing"));
        std::fs::create_dir(env.root().join("state/volumes/blocked")).unwrap();
        let mut mounts = vec![Mount::volume("existing", "/existing")];
        for index in 0..new_count {
            mounts
                .push(Mount::volume(format!("new{index}"), format!("/new/{index}")).with_size(MIB));
        }
        mounts.push(Mount::volume("blocked", "/blocked"));
        let result = engine.create(Some("example"), sh("true"), with_mounts(mounts));
        assert_eq!(result.err().unwrap().kind(), ErrorKind::Conflict);
        assert_eq!(read_marker(&volume_file(&env, "existing")), b"kept");
        for index in 0..new_count {
            let name = format!("new{index}");
            assert_eq!(
                engine.volume(&name).unwrap_err().kind(),
                ErrorKind::NotFound
            );
            assert!(!volume_file(&env, &name).parent().unwrap().exists());
        }
        assert!(engine.list(true).unwrap().is_empty());
    }
}

#[test]
fn failed_update_persistence_rolls_back_all_new_volumes_and_preserves_saved_configuration() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;
    if common::is_root() {
        // Run this permission-denial contract without root's DAC override.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "failed_update_persistence_rolls_back_all_new_volumes_and_preserves_saved_configuration", "--nocapture"])
            .gid(65534).uid(65534).output().unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    for new_count in [1, 2, 3] {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::new(libclankerd::EngineConfig::new(
            dir.path().join("state"),
            dir.path().join("run"),
        ))
        .unwrap();
        let machine = engine
            .create(
                Some("example"),
                sh("true"),
                with_mounts(vec![Mount::volume("existing", "/data").with_size(MIB)]),
            )
            .unwrap();
        let existing = dir.path().join("state/volumes/existing/data.ext4");
        write_marker(&existing);
        let before = machine.inspect().unwrap();
        // Establish the lifecycle lock before making record persistence fail.
        machine
            .update(&before.config, before.host_config.clone())
            .unwrap();
        let machine_dir = dir.path().join("state/machines").join(machine.id());
        let permissions = std::fs::metadata(&machine_dir).unwrap().permissions();
        std::fs::set_permissions(&machine_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let mut host = before.host_config.clone();
        host.memory = Some(128 * MIB);
        for index in 0..new_count {
            host.mounts
                .push(Mount::volume(format!("new{index}"), format!("/new/{index}")).with_size(MIB));
        }
        let result = machine.update(&before.config, host);
        std::fs::set_permissions(&machine_dir, permissions).unwrap();
        assert_eq!(result.unwrap_err().kind(), ErrorKind::System);
        assert_eq!(
            serde_json::to_value(machine.inspect().unwrap()).unwrap(),
            serde_json::to_value(before).unwrap()
        );
        assert_eq!(read_marker(&existing), b"kept");
        for index in 0..new_count {
            let name = format!("new{index}");
            assert_eq!(
                engine.volume(&name).unwrap_err().kind(),
                ErrorKind::NotFound
            );
            assert!(!dir.path().join("state/volumes").join(name).exists());
        }
    }
}
