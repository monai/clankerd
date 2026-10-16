//! Guest init owns machine lifetime independently of the OCI command.
mod common;
use common::*;
use libclankerd::Status;
use std::time::Duration;

#[test]
fn completed_commands_leave_the_machine_available_until_explicit_stop() {
    let env = Env::new();
    let engine = env.engine();
    for (code, script) in [
        (0, "exit 0"),
        (7, "exit 7"),
        (0, "exec /bin/sh"),
        (127, "exec /no/such/binary"),
    ] {
        let machine = create(&engine, "member", script);
        machine.start().unwrap();
        assert_eq!(machine.wait_workload().unwrap().exit_code, code);
        assert_eq!(machine.inspect().unwrap().state.status, Status::Running);
        let exec = machine
            .exec_create(libclankerd::ExecConfig {
                cmd: vec!["/bin/true".into()],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(exec.start().unwrap().wait().unwrap().exit_code, 0);
        machine.stop(Duration::from_secs(5)).unwrap();
        assert_eq!(machine.inspect().unwrap().state.status, Status::Exited);
        machine.start().unwrap();
        assert_eq!(machine.inspect().unwrap().state.status, Status::Running);
        machine.stop(Duration::from_secs(5)).unwrap();
        machine.remove(false).unwrap();
    }
}
