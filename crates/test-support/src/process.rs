use std::fs::File;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum CommandOutcome {
    Exited(ExitStatus),
    TimedOut(ExitStatus),
}

pub fn run_bounded(
    command: &mut Command,
    evidence_prefix: &Path,
    timeout: Duration,
) -> io::Result<CommandOutcome> {
    command.stdout(Stdio::from(File::create(
        evidence_prefix.with_extension("stdout"),
    )?));
    command.stderr(Stdio::from(File::create(
        evidence_prefix.with_extension("stderr"),
    )?));
    command.process_group(0);
    let mut child = command.spawn()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(CommandOutcome::Exited(status)),
            Ok(None) => {}
            Err(error) => {
                unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
                let _ = child.wait();
                return Err(error);
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let group = -(child.id() as i32);
    unsafe { libc::kill(group, libc::SIGTERM) };
    thread::sleep(Duration::from_millis(100));
    unsafe { libc::kill(group, libc::SIGKILL) };
    Ok(CommandOutcome::TimedOut(child.wait()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(name: &str) -> std::path::PathBuf {
        let directory =
            std::env::temp_dir().join(format!("clankerd-process-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        directory.join("command")
    }

    #[test]
    fn retains_streams_and_nonzero_status() {
        let prefix = evidence("streams");
        let result = run_bounded(
            Command::new("sh").args(["-c", "printf foo; printf bar >&2; exit 7"]),
            &prefix,
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(matches!(result, CommandOutcome::Exited(status) if status.code() == Some(7)));
        assert_eq!(
            std::fs::read(prefix.with_extension("stdout")).unwrap(),
            b"foo"
        );
        assert_eq!(
            std::fs::read(prefix.with_extension("stderr")).unwrap(),
            b"bar"
        );
    }

    #[test]
    fn output_is_visible_before_completion() {
        let prefix = evidence("live");
        let observed = prefix.clone();
        let worker = thread::spawn(move || {
            run_bounded(
                Command::new("sh").args(["-c", "printf foo; sleep 1"]),
                &prefix,
                Duration::from_secs(5),
            )
            .unwrap()
        });
        let deadline = Instant::now() + Duration::from_millis(800);
        while std::fs::read(observed.with_extension("stdout")).unwrap_or_default() != b"foo" {
            assert!(
                Instant::now() < deadline,
                "partial output must be readable while running"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!worker.is_finished());
        assert!(
            matches!(worker.join().unwrap(), CommandOutcome::Exited(status) if status.success())
        );
    }

    #[test]
    fn timeout_retains_partial_output() {
        let prefix = evidence("timeout");
        let start = Instant::now();
        let result = run_bounded(
            Command::new("sh").args(["-c", "printf foo; sleep 30 & wait"]),
            &prefix,
            Duration::from_millis(200),
        )
        .unwrap();
        assert!(matches!(result, CommandOutcome::TimedOut(_)));
        assert!(start.elapsed() < Duration::from_secs(5));
        assert_eq!(
            std::fs::read(prefix.with_extension("stdout")).unwrap(),
            b"foo"
        );
    }
    #[test]
    fn timeout_signals_descendants_without_signalling_the_caller() {
        let prefix = evidence("descendants");
        let result = run_bounded(
            Command::new("sh").args([
                "-c",
                "sh -c 'trap \"printf terminated; exit 0\" TERM; printf ready; while :; do sleep 1; done' & wait",
            ]),
            &prefix,
            Duration::from_secs(2),
        )
        .unwrap();
        assert!(matches!(result, CommandOutcome::TimedOut(_)));
        let output = std::fs::read_to_string(prefix.with_extension("stdout")).unwrap();
        assert!(output.contains("ready"));
        assert!(
            output.contains("terminated"),
            "descendant receives termination"
        );
    }
}
