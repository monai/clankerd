use socket2::{Domain, SockAddr, Socket, Type};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

pub(super) struct DeadlineStream {
    stream: UnixStream,
    deadline: Instant,
}

impl DeadlineStream {
    pub(super) fn connect(path: &Path, deadline: Instant) -> io::Result<Self> {
        let address = SockAddr::unix(path)?;
        let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
        socket.set_nonblocking(true)?;
        loop {
            remaining(deadline)?;
            match socket.connect(&address) {
                Ok(()) => break,
                Err(error) if error.raw_os_error() == Some(libc::EINPROGRESS) => {
                    poll(socket.as_raw_fd(), libc::POLLOUT, deadline)?;
                    if let Some(error) = socket.take_error()? {
                        return Err(error);
                    }
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(remaining(deadline)?.min(Duration::from_millis(1)));
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(Self {
            stream: socket.into(),
            deadline,
        })
    }

    fn operation<T>(
        &mut self,
        events: i16,
        mut operation: impl FnMut(&mut UnixStream) -> io::Result<T>,
    ) -> io::Result<T> {
        loop {
            remaining(self.deadline)?;
            match operation(&mut self.stream) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    poll(self.stream.as_raw_fd(), events, self.deadline)?
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                result => return result,
            }
        }
    }
}

impl Read for DeadlineStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.operation(libc::POLLIN, |stream| stream.read(bytes))
    }
}

impl Write for DeadlineStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.operation(libc::POLLOUT, |stream| stream.write(bytes))
    }
    fn flush(&mut self) -> io::Result<()> {
        self.operation(libc::POLLOUT, Write::flush)
    }
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "guest RPC deadline expired"))
}

fn poll(fd: i32, events: i16, deadline: Instant) -> io::Result<()> {
    loop {
        let duration = remaining(deadline)?;
        let millis = duration.as_millis().saturating_add(1).min(i32::MAX as u128) as i32;
        let mut descriptor = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut descriptor, 1, millis) };
        if result > 0 {
            return Ok(());
        }
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn prompt_round_trip_preserves_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("socket");
        let listener = UnixListener::bind(&path).unwrap();
        let mut stream =
            DeadlineStream::connect(&path, Instant::now() + Duration::from_secs(2)).unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        stream.write_all(b"request").unwrap();
        let mut bytes = [0; 7];
        peer.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"request");
        peer.write_all(b"example").unwrap();
        stream.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"example");
    }

    #[test]
    fn sustained_partial_replies_do_not_restart_the_budget() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("socket");
        let listener = UnixListener::bind(&path).unwrap();
        let start = Instant::now();
        let mut stream =
            DeadlineStream::connect(&path, start + Duration::from_millis(150)).unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        let writer = std::thread::spawn(move || {
            peer.set_write_timeout(Some(Duration::from_millis(100)))
                .unwrap();
            for _ in 0..100 {
                if peer.write_all(b"x").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let mut bytes = Vec::new();
        assert_eq!(
            stream.read_to_end(&mut bytes).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(!bytes.is_empty(), "partial replies were received");
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "original request budget bounds partial replies"
        );
        drop(stream);
        writer.join().unwrap();
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn saturated_connection_backlog_expires() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("socket");
        let listener = Socket::new(Domain::UNIX, Type::STREAM, None).unwrap();
        let address = SockAddr::unix(&path).unwrap();
        listener.bind(&address).unwrap();
        listener.listen(1).unwrap();
        let mut clients = Vec::new();
        for _ in 0..128 {
            let client = Socket::new(Domain::UNIX, Type::STREAM, None).unwrap();
            client.set_nonblocking(true).unwrap();
            match client.connect(&address) {
                Ok(()) => clients.push(client),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("fill owned backlog: {error}"),
            }
        }
        let start = Instant::now();
        let error = match DeadlineStream::connect(&path, start + Duration::from_millis(100)) {
            Ok(_) => panic!("full listener backlog unexpectedly accepted another connection"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn partial_progress_does_not_extend_the_deadline() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("socket");
        let listener = UnixListener::bind(&path).unwrap();
        let deadline = Instant::now() + Duration::from_millis(100);
        let mut stream = DeadlineStream::connect(&path, deadline).unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        peer.write_all(b"example").unwrap();
        let mut bytes = [0; 7];
        stream.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"example");
        assert_eq!(
            stream.read(&mut bytes).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[test]
    fn blocked_writes_expire() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("socket");
        let listener = UnixListener::bind(&path).unwrap();
        let mut stream =
            DeadlineStream::connect(&path, Instant::now() + Duration::from_millis(100)).unwrap();
        let (_peer, _) = listener.accept().unwrap();
        assert_eq!(
            stream
                .write_all(&vec![0; 16 * 1024 * 1024])
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
    }
}
