mod common;

use common::*;
use libclankerd::Status;

#[test]
fn run_returns_exit_code_zero() {
    let env = Env::new();
    let engine = env.engine();
    let m = create(&engine, "ok", "exit 0");
    m.start().unwrap();
    assert_eq!(m.wait().unwrap().exit_code, 0);
    assert_eq!(m.inspect().unwrap().state.status, Status::Exited);
}
