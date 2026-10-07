//! Seam A: the events stream reports each state change, in order.

mod common;

use std::time::Duration;

use common::*;
use libclankerd::{EventAction, Events, MachineEvent};

fn drain(events: &mut Events, n: usize) -> Vec<MachineEvent> {
    (0..n)
        .map(|i| {
            events
                .next_timeout(Duration::from_secs(10))
                .unwrap_or_else(|| panic!("event {i} never arrived"))
        })
        .collect()
}

#[test]
fn a_machine_lifecycle_emits_its_events_in_order() {
    let env = Env::new();
    let engine = env.engine();
    let mut events = engine.events();

    let m = create(&engine, "evt", "exit 3");
    m.start().unwrap();
    m.wait().unwrap();
    m.remove(false).unwrap();

    let got = drain(&mut events, 4);
    let actions: Vec<_> = got.iter().map(|e| e.action).collect();
    assert_eq!(
        actions,
        [
            EventAction::Created,
            EventAction::Started,
            EventAction::Exited,
            EventAction::Removed
        ]
    );
    assert!(
        got.iter()
            .all(|e| e.machine_name == "evt" && e.machine_id == m.id())
    );
    assert_eq!(got[2].exit_code, Some(3));
    assert!(events.next_timeout(Duration::from_millis(200)).is_none());
}

#[test]
fn a_kill_is_reported_before_the_exit() {
    let env = Env::new();
    let engine = env.engine();
    let m = create(&engine, "killed", "sleep 60");
    m.start().unwrap();
    let mut events = engine.events();
    m.kill(libc::SIGKILL).unwrap();

    let got = drain(&mut events, 2);
    assert_eq!(got[0].action, EventAction::Killed);
    assert_eq!(got[1].action, EventAction::Exited);
    assert_eq!(got[1].exit_code, Some(137));
}

#[test]
fn each_subscriber_sees_every_event() {
    let env = Env::new();
    let engine = env.engine();
    let mut a = engine.events();
    let mut b = engine.events();
    create(&engine, "both", "exit 0");
    assert_eq!(drain(&mut a, 1)[0].action, EventAction::Created);
    assert_eq!(drain(&mut b, 1)[0].action, EventAction::Created);
}
