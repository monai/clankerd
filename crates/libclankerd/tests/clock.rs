//! Seam A: the guest clock can be resynced from the host (after the host slept).

mod common;

use common::*;
use libclankerd::ErrorKind;

#[test]
fn sync_clock_reaches_the_guest_of_a_running_machine() {
    let env = Env::new();
    let engine = env.engine();
    let m = create(&engine, "tick", "sleep 60");
    assert_eq!(m.sync_clock().unwrap_err().kind(), ErrorKind::Conflict);
    m.start().unwrap();
    m.sync_clock().unwrap();
    m.remove(true).unwrap();
}
