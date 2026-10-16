//! The serialized host/helper protocol, independent of VM lifecycle fixtures.
use clankerd_proto::spawn::SpawnSpec;
use serde_json::{Value, json};
use std::path::PathBuf;

fn required_fields() -> Value {
    json!({
        "boot_dir": "/example/boot", "console_log": "/example/console",
        "exit_file": "/example/exit", "vsock_socket": "/example/guest.sock",
        "host_socket": "/example/host.sock", "cpus": 1, "memory_mib": 128,
    })
}

#[test]
fn volume_disk_decoding_accepts_absence_collections_and_legacy_specs() {
    for (field, value, expected) in [
        (None, Value::Null, vec![]),
        (Some("volume_disks"), json!([]), vec![]),
        (
            Some("volume_disks"),
            json!(["/example/first.ext4"]),
            vec!["/example/first.ext4"],
        ),
        (
            Some("volume_disks"),
            json!(["/example/first.ext4", "/example/second.ext4"]),
            vec!["/example/first.ext4", "/example/second.ext4"],
        ),
        (Some("volume_disk"), Value::Null, vec![]),
        (
            Some("volume_disk"),
            json!("/example/legacy.ext4"),
            vec!["/example/legacy.ext4"],
        ),
    ] {
        let mut input = required_fields();
        if let Some(field) = field {
            input[field] = value;
        }
        let spec: SpawnSpec = serde_json::from_value(input.clone()).unwrap();
        let expected: Vec<PathBuf> = expected.into_iter().map(PathBuf::from).collect();
        assert_eq!(spec.volume_disks, expected, "{input}");
    }
}

#[test]
fn volume_disks_are_written_as_an_ordered_collection() {
    let mut spec: SpawnSpec = serde_json::from_value(required_fields()).unwrap();
    for disks in [
        vec![],
        vec!["/example/first.ext4"],
        vec!["/example/first.ext4", "/example/second.ext4"],
    ] {
        spec.volume_disks = disks.iter().map(PathBuf::from).collect();
        let encoded = serde_json::to_value(&spec).unwrap();
        assert_eq!(encoded["volume_disks"], json!(disks));
        assert!(encoded.get("volume_disk").is_none());
    }
}
