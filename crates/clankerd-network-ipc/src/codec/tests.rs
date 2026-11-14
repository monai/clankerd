use super::{from_object, to_object};
use crate::{Request, Response, Settings, ffi, xpc::Object};
use std::{
    fmt::Debug,
    os::{
        fd::{AsFd, AsRawFd},
        unix::net::UnixDatagram,
    },
};

fn round_trip<T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + Debug>(value: T) {
    let object = to_object(&value).unwrap();
    assert_eq!(from_object::<T>(object.borrow()).unwrap(), value);
}

#[test]
fn protocol_models_round_trip_through_native_objects() {
    let settings = Settings {
        interface_id: "interface-1".into(),
        operation_mode: "host".into(),
        start_address: Some("192.168.231.1".into()),
        end_address: Some("192.168.231.254".into()),
        subnet_mask: Some("255.255.255.0".into()),
        nat66_prefix: None,
        shared_interface: None,
        network_id: None,
        subnet: "192.168.231.0/24".into(),
        firewall: false,
    };
    for request in [
        Request::Create {
            key: "/tmp/example".into(),
            settings,
        },
        Request::Attach {
            key: "/tmp/example".into(),
            generation: "generation-1".into(),
        },
        Request::Stop {
            key: "/tmp/example".into(),
            generation: "generation-1".into(),
        },
        Request::Status {
            key: "/tmp/example".into(),
            generation: "generation-1".into(),
        },
        Request::List {},
    ] {
        round_trip(request);
    }
    round_trip(Ok::<_, String>(Response {
        connected: true,
        interfaces: vec!["bridge1".into()],
        ..Response::default()
    }));
    round_trip(Err::<Response, _>("network is already attached".to_owned()));
    round_trip((i64::MIN, u64::MAX, vec![Some(true), None]));
}

#[test]
fn fields_use_native_types_and_preserve_full_width_integers() {
    #[derive(serde::Serialize)]
    struct Fields {
        signed: i64,
        unsigned: u64,
        enabled: bool,
    }
    let object = to_object(&Fields {
        signed: i64::MIN,
        unsigned: u64::MAX,
        enabled: true,
    })
    .unwrap();
    for (key, ty) in [
        (c"signed", &raw const ffi::_xpc_type_int64),
        (c"unsigned", &raw const ffi::_xpc_type_uint64),
        (c"enabled", &raw const ffi::_xpc_type_bool),
    ] {
        assert!(object.borrow().get(key).unwrap().unwrap().is(ty));
    }
    assert!(to_object(&u128::MAX).is_err());
    assert!(to_object("embedded\0nul").is_err());
}

#[test]
fn malformed_requests_reject_missing_unknown_and_wrong_type_fields() {
    let mut request = to_object(&Request::Attach {
        key: "/tmp/example".into(),
        generation: "generation-1".into(),
    })
    .unwrap();
    request.set(c"key", to_object(&true).unwrap()).unwrap();
    assert!(from_object::<Request>(request.borrow()).is_err());
    let mut request = to_object(&Request::List {}).unwrap();
    request
        .set(c"unexpected", to_object(&true).unwrap())
        .unwrap();
    assert!(from_object::<Request>(request.borrow()).is_err());
    #[derive(serde::Serialize)]
    struct Incomplete {
        action: &'static str,
        key: &'static str,
    }
    let request = to_object(&Incomplete {
        action: "attach",
        key: "/tmp/example",
    })
    .unwrap();
    assert!(
        from_object::<Request>(request.borrow())
            .unwrap_err()
            .to_string()
            .contains("generation")
    );
}

#[test]
fn supported_payloads_round_trip_without_codec_size_quotas() {
    round_trip("foo".repeat(30_000));
    round_trip(vec![7u64; 10_000]);
    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    enum Node {
        Leaf,
        Branch(Box<Node>),
    }
    let mut value = Node::Leaf;
    for _ in 0..64 {
        value = Node::Branch(Box::new(value));
    }
    round_trip(value);
}

#[test]
fn options_preserve_presence_or_fail_when_null_would_erase_it() {
    for value in [None, Some("".to_owned()), Some("foo".to_owned())] {
        round_trip(value);
    }
    round_trip(Some(Some("foo")).map(|value| value.map(str::to_owned)));
    assert!(to_object(&Some(())).is_err());
    assert!(to_object(&Some(None::<String>)).is_err());
    assert!(to_object(&Some(Some(()))).is_err());
}

#[test]
fn containers_own_their_children_after_insertion() {
    let mut child = Object::dictionary().unwrap();
    child.set(c"text", to_object("foo").unwrap()).unwrap();
    let mut array = Object::array().unwrap();
    array.append(child).unwrap();
    let mut parent = Object::dictionary().unwrap();
    parent.set(c"children", array).unwrap();
    let text = parent
        .borrow()
        .get(c"children")
        .unwrap()
        .unwrap()
        .at(0)
        .unwrap()
        .get(c"text")
        .unwrap()
        .unwrap()
        .string()
        .unwrap();
    assert_eq!(text, "foo");
}

#[test]
fn descriptors_are_owned_capabilities_outside_serde() {
    let (sender, original) = UnixDatagram::pair().unwrap();
    let envelope = Object::dictionary().unwrap();
    unsafe { envelope.borrow().attach_fd(c"packet_fd", original.as_fd()) }.unwrap();
    // Ordinary decoding, including ignored fields, never duplicates a capability.
    assert!(from_object::<serde::de::IgnoredAny>(envelope.borrow()).is_err());
    let received = envelope.borrow().receive_fd(c"packet_fd").unwrap().unwrap();
    assert_ne!(
        unsafe { libc::fcntl(received.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
        0
    );
    drop(original);
    drop(envelope);
    let receiver = UnixDatagram::from(received);
    receiver
        .set_read_timeout(Some(std::time::Duration::from_secs(1)))
        .unwrap();
    sender.send(b"packet").unwrap();
    let mut bytes = [0; 6];
    assert_eq!(receiver.recv(&mut bytes).unwrap(), 6);
    assert_eq!(&bytes, b"packet");
}
