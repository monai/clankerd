mod common;

use std::io::Cursor;
use std::time::{Duration, Instant};

use clankerd_proto::rootdisk::{
    ERROR_INVALID_PARAMETER, METHOD_FINISH_POPULATION, METHOD_POPULATE_DISK, PopulateDisk,
};
use clankerd_proto::{frame, varlink};

#[test]
fn framed_population_replies_without_a_socket_half_close() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("root");
    std::fs::create_dir(&target).unwrap();
    let guest = common::Guestd::spawn_lenient(dir.path());
    let content = vec![b'x'; frame::MAX_PAYLOAD + 123];
    let mut archive = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(content.len() as u64);
    header.set_mode(0o644);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(1_700_000_000);
    archive
        .append_data(&mut header, "file", content.as_slice())
        .unwrap();
    let tar = archive.into_inner().unwrap();

    let mut conn = guest.connect();
    varlink::write(
        &mut conn,
        &varlink::Call {
            method: METHOD_POPULATE_DISK.into(),
            parameters: serde_json::to_value(PopulateDisk {
                device: target.to_string_lossy().into_owned(),
                size: None,
            })
            .unwrap(),
            more: false,
            upgrade: false,
        },
    )
    .unwrap();
    frame::write_chunks(&mut conn, frame::CHANNEL_STDIN, &tar).unwrap();
    frame::write_frame(&mut conn, &frame::Frame::close(frame::CHANNEL_STDIN)).unwrap();
    let reply: varlink::Reply = varlink::read(&mut std::io::BufReader::new(&mut conn))
        .unwrap()
        .expect("guest replied with the socket still open for writes");
    assert_eq!(reply.error, None, "{:?}", reply.parameters);
    assert_eq!(std::fs::read(target.join("file")).unwrap(), content);
}

#[test]
fn population_waits_for_host_acknowledgment_before_exiting() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("root");
    std::fs::create_dir(&target).unwrap();
    let mut guest = common::Guestd::spawn_lenient(dir.path());
    let archive = tar::Builder::new(Vec::new()).into_inner().unwrap();
    let reply = guest.call_with_stream(
        METHOD_POPULATE_DISK,
        &PopulateDisk {
            device: target.to_string_lossy().into_owned(),
            size: None,
        },
        &mut Cursor::new(archive),
    );
    assert_eq!(reply.error, None, "{:?}", reply.parameters);
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        guest.is_running(),
        "guest shut down before the host acknowledged the reply"
    );

    guest.send(METHOD_FINISH_POPULATION);
    let deadline = Instant::now() + Duration::from_secs(2);
    while guest.is_running() {
        assert!(
            Instant::now() < deadline,
            "guest ignored the acknowledgment"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn population_cannot_be_finished_before_a_successful_population() {
    let dir = tempfile::tempdir().unwrap();
    let mut guest = common::Guestd::spawn_lenient(dir.path());
    let reply = guest.call(METHOD_FINISH_POPULATION, &serde_json::json!({}));
    assert_eq!(reply.error.as_deref(), Some(ERROR_INVALID_PARAMETER));
    assert!(guest.is_running());

    let reply = guest.call_with_stream(
        METHOD_POPULATE_DISK,
        &PopulateDisk {
            device: dir.path().join("missing").to_string_lossy().into_owned(),
            size: None,
        },
        &mut std::io::empty(),
    );
    assert!(reply.error.is_some());
    let reply = guest.call(METHOD_FINISH_POPULATION, &serde_json::json!({}));
    assert_eq!(reply.error.as_deref(), Some(ERROR_INVALID_PARAMETER));
    assert!(guest.is_running());
}

#[test]
fn a_socket_eof_cannot_replace_the_tar_close_frame() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("root");
    std::fs::create_dir(&target).unwrap();
    let mut guest = common::Guestd::spawn_lenient(dir.path());
    let mut conn = guest.connect();
    varlink::write(
        &mut conn,
        &varlink::Call {
            method: METHOD_POPULATE_DISK.into(),
            parameters: serde_json::to_value(PopulateDisk {
                device: target.to_string_lossy().into_owned(),
                size: None,
            })
            .unwrap(),
            more: false,
            upgrade: false,
        },
    )
    .unwrap();
    let tar = tar::Builder::new(Vec::new()).into_inner().unwrap();
    frame::write_chunks(&mut conn, frame::CHANNEL_STDIN, &tar).unwrap();
    conn.shutdown(std::net::Shutdown::Write).unwrap();
    let reply: varlink::Reply = varlink::read(&mut std::io::BufReader::new(conn))
        .unwrap()
        .unwrap();
    assert!(reply.error.is_some());
    assert!(
        reply.parameters["message"]
            .as_str()
            .unwrap()
            .contains("close frame")
    );
    let reply = guest.call(METHOD_FINISH_POPULATION, &serde_json::json!({}));
    assert_eq!(reply.error.as_deref(), Some(ERROR_INVALID_PARAMETER));
    assert!(guest.is_running());
}
