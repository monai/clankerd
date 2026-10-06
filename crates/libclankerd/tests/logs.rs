//! Seam A: console and workload output through `Machine::logs`.

mod common;

use common::*;

#[test]
fn logs_return_what_the_workload_printed() {
    let env = Env::new();
    let engine = env.engine();
    let m = create(&engine, "talker", "echo hello-console; echo oops >&2");
    m.start().unwrap();
    m.wait().unwrap();
    let logs = String::from_utf8(m.logs().unwrap()).unwrap();
    assert!(logs.contains("hello-console"), "{logs}");
    assert!(logs.contains("oops"), "{logs}");
}

#[test]
fn logs_of_a_machine_that_never_started_are_empty() {
    let env = Env::new();
    let engine = env.engine();
    let m = create(&engine, "quiet", "exit 0");
    assert!(m.logs().unwrap().is_empty());
}
