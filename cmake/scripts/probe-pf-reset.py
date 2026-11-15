#!/usr/bin/env python3
"""Run production PF reset logic with a fake PF command and isolated state path."""
import pathlib, tempfile, subprocess, json
repo = pathlib.Path(__file__).resolve().parents[2]
root = pathlib.Path(tempfile.mkdtemp(prefix='clankerd-pf-reset-'))
source = (repo/'crates/clankerd-network/src/pf.rs').read_text()
# Only replace external effects: command execution and the state directory.
start = source.index('fn command(')
end = source.index('\nfn rules(', start)
source = source[:start] + '''fn command(args: &[&str], _: Option<&str>) -> Result<String> {
    if args.contains(&"-X") { return Err("injected PF token release failure".into()); }
    Ok(String::new())
}
''' + source[end:]
source = source.replace('const STATE: &str = "/var/run/clankerd-network-pf";',
                        'const STATE: &str = '+json.dumps(str(root/'state'))+';')
source += '''
#[cfg(test)]
mod recovery_probe {
    use super::*;
    #[test]
    fn failed_release_must_preserve_recovery_token() {
        fs::create_dir_all(STATE).unwrap();
        let initial = State { records: BTreeMap::new(), token: Some("12345".into()) };
        persist(&initial).unwrap();
        let outcome = reset();
        let after: State = serde_json::from_slice(&fs::read(Path::new(STATE).join("state.json")).unwrap()).unwrap();
        eprintln!("reset_reported_success={} token_after={:?}", outcome.is_ok(), after.token);
        assert!(outcome.is_err() && after.token.as_deref() == Some("12345"),
            "failed token release must be reported and its recovery token retained");
    }
}
'''
(root/'src').mkdir()
(root/'src/lib.rs').write_text(source)
(root/'Cargo.toml').write_text('''[package]
name="clankerd-pf-reset-probe"
version="0.0.0"
edition="2024"
[dependencies]
serde={version="1",features=["derive"]}
serde_json="1"
sysinfo={version="0.39.6",default-features=false,features=["network"]}
''')
print('Isolated probe:', root, flush=True)
r = subprocess.run(['cargo','test','--offline','--manifest-path',str(root/'Cargo.toml'),
                    'recovery_probe::','--','--nocapture'], capture_output=True, text=True)
(root/'output.txt').write_text(r.stdout+r.stderr)
print(r.stdout+r.stderr)
print('Exit:',r.returncode,'Evidence:',root)
raise SystemExit(r.returncode)
