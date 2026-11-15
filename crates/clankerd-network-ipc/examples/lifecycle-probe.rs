//! Small subprocess client for cmake/scripts/probe-network-lifecycle.py.
use clankerd_network_ipc::{Connection, Request, Settings};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let action = args.get(1).ok_or("missing action")?;
    let key = args.get(2).ok_or("missing key")?.clone();
    let request = match action.as_str() {
        "create" => Request::Create {
            key,
            settings: Settings {
                interface_id: args.get(3).ok_or("missing UUID")?.clone(),
                operation_mode: "host".into(),
                start_address: Some("192.168.232.1".into()),
                end_address: Some("192.168.232.254".into()),
                subnet_mask: Some("255.255.255.0".into()),
                nat66_prefix: None,
                shared_interface: None,
                network_id: Some(args.get(4).ok_or("missing network ID")?.clone()),
                subnet: "192.168.232.0/24".into(),
                firewall: true,
            },
        },
        "status" => Request::Status {
            key,
            generation: args.get(3).ok_or("missing generation")?.clone(),
        },
        "stop" => Request::Stop {
            key,
            generation: args.get(3).ok_or("missing generation")?.clone(),
        },
        _ => return Err("unknown action".into()),
    };
    let reply = Connection::connect()?.request(&request)?.response;
    if action == "create" {
        println!("{}", reply.generation.ok_or("missing generation")?);
    } else {
        println!("connected={}", reply.connected);
    }
    Ok(())
}
