//! One packet boundary between libkrun and vmnet. Congestion retains the current
//! batch and pauses upstream reads; the opposite direction remains serviceable.
use crate::egress::Policy;
use std::{
    io,
    os::{fd::AsRawFd, unix::net::UnixDatagram},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub const FRAME_SIZE: usize = 65536;
pub const BATCH_SIZE: usize = 64;
// Darwin Unix datagrams can return ENOBUFS while reporting POLLOUT. Retrying
// only after a deadline avoids spinning on that readiness indication.
const RETRY: Duration = Duration::from_millis(1);

pub struct Batch {
    pub data: Box<[u8]>,
    pub sizes: [usize; BATCH_SIZE],
}
impl Batch {
    fn new() -> Self {
        Self {
            data: vec![0; FRAME_SIZE * BATCH_SIZE].into_boxed_slice(),
            sizes: [0; BATCH_SIZE],
        }
    }
    fn frame(&self, index: usize) -> &[u8] {
        &self.data[index * FRAME_SIZE..index * FRAME_SIZE + self.sizes[index]]
    }
}

/// Configure both endpoints before transferring one to libkrun. Its direct-FD
/// constructor deliberately leaves socket buffers to the caller.
pub fn socket_pair() -> io::Result<(UnixDatagram, UnixDatagram)> {
    let pair = UnixDatagram::pair()?;
    for socket in [&pair.0, &pair.1] {
        for (option, size) in [
            (libc::SO_SNDBUF, FRAME_SIZE as libc::c_int),
            (libc::SO_RCVBUF, 4 * 1024 * 1024),
        ] {
            if unsafe {
                libc::setsockopt(
                    socket.as_raw_fd(),
                    libc::SOL_SOCKET,
                    option,
                    (&size as *const libc::c_int).cast(),
                    size_of_val(&size) as _,
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        socket.set_nonblocking(true)?;
    }
    Ok(pair)
}

pub trait PacketIo {
    fn notifications(&self) -> &UnixDatagram;
    fn read(&mut self, batch: &mut Batch) -> io::Result<usize>;
    fn write(&mut self, frame: &[u8]) -> io::Result<()>;
}

#[derive(Debug, Default)]
pub struct Counters {
    pub to_guest: u64,
    pub from_guest: u64,
    pub congested: u64,
    pub rejected: u64,
}

fn congestion(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::WouldBlock || error.raw_os_error() == Some(libc::ENOBUFS)
}
fn due(deadline: Option<Instant>) -> bool {
    deadline.is_none_or(|deadline| Instant::now() >= deadline)
}
fn timeout(ready: bool, deadlines: [Option<Instant>; 2]) -> i32 {
    if ready {
        return 0;
    }
    deadlines
        .into_iter()
        .flatten()
        .min()
        .map_or(-1, |deadline| {
            deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .saturating_add(1) as i32
        })
}

pub fn forward(
    mut interface: impl PacketIo,
    socket: UnixDatagram,
    active: Arc<AtomicBool>,
    policy: Policy,
) -> io::Result<Counters> {
    let mut counters = Counters::default();
    let mut batch = Batch::new();
    let (mut next, mut count) = (0, 0);
    let mut vmnet_ready = false;
    let mut to_guest_retry = None;
    let mut from_guest_retry = None;
    let mut outgoing = vec![0; FRAME_SIZE];
    let mut pending = 0;
    while active.load(Ordering::Relaxed) {
        let mut descriptors = [
            libc::pollfd {
                fd: socket.as_raw_fd(),
                events: if pending == 0 { libc::POLLIN } else { 0 },
                revents: 0,
            },
            libc::pollfd {
                fd: interface.notifications().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let wait = timeout(
            vmnet_ready && count == 0,
            [to_guest_retry, from_guest_retry],
        );
        if unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, wait) } < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if !active.load(Ordering::Relaxed) {
            break;
        }
        if descriptors
            .iter()
            .any(|p| p.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0)
        {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "packet socket closed",
            ));
        }
        if descriptors[1].revents & libc::POLLIN != 0 {
            for _ in 0..BATCH_SIZE {
                match interface.notifications().recv(&mut [0]) {
                    Ok(_) => vmnet_ready = true,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e),
                }
            }
        }
        // Keep reading guest traffic while host-to-guest delivery is stalled.
        if pending != 0 && due(from_guest_retry) {
            match interface.write(&outgoing[..pending]) {
                Ok(()) => {
                    pending = 0;
                    from_guest_retry = None;
                    counters.from_guest += 1;
                }
                Err(e) if congestion(&e) => {
                    from_guest_retry = Some(Instant::now() + RETRY);
                    counters.congested += 1;
                }
                Err(e) => return Err(e),
            }
        }
        if pending == 0 && descriptors[0].revents & libc::POLLIN != 0 {
            for _ in 0..BATCH_SIZE {
                let size = match socket.recv(&mut outgoing) {
                    Ok(size) => size,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e),
                };
                if !policy.permits(&outgoing[..size]) {
                    counters.rejected += 1;
                    continue;
                }
                match interface.write(&outgoing[..size]) {
                    Ok(()) => counters.from_guest += 1,
                    Err(e) if congestion(&e) => {
                        pending = size;
                        from_guest_retry = Some(Instant::now() + RETRY);
                        counters.congested += 1;
                        break;
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        if count == 0 && vmnet_ready {
            count = interface.read(&mut batch)?;
            next = 0;
            if count == 0 {
                vmnet_ready = false;
            }
        }
        if count != 0 && due(to_guest_retry) {
            while next < count {
                match socket.send(batch.frame(next)) {
                    Ok(_) => {
                        next += 1;
                        counters.to_guest += 1;
                    }
                    Err(e) if congestion(&e) => {
                        to_guest_retry = Some(Instant::now() + RETRY);
                        counters.congested += 1;
                        break;
                    }
                    Err(e) => return Err(e),
                }
            }
            if next == count {
                count = 0;
                to_guest_retry = None;
            }
        }
    }
    Ok(counters)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clankerd_network_ipc::Settings;
    use std::{
        collections::VecDeque,
        sync::{Mutex, atomic::AtomicUsize, mpsc},
        thread::JoinHandle,
    };

    struct TestInterface {
        notifications: UnixDatagram,
        incoming: Arc<Mutex<VecDeque<Vec<u8>>>>,
        outgoing: mpsc::Sender<Vec<u8>>,
        reads: Arc<AtomicUsize>,
        blocked: Arc<AtomicBool>,
        writes: Arc<AtomicUsize>,
    }
    impl PacketIo for TestInterface {
        fn notifications(&self) -> &UnixDatagram {
            &self.notifications
        }
        fn read(&mut self, batch: &mut Batch) -> io::Result<usize> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            let mut incoming = self.incoming.lock().unwrap();
            let mut count = 0;
            while count < BATCH_SIZE {
                let Some(frame) = incoming.pop_front() else {
                    break;
                };
                batch.data[count * FRAME_SIZE..count * FRAME_SIZE + frame.len()]
                    .copy_from_slice(&frame);
                batch.sizes[count] = frame.len();
                count += 1;
            }
            Ok(count)
        }
        fn write(&mut self, frame: &[u8]) -> io::Result<()> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            if self.blocked.load(Ordering::SeqCst) {
                return Err(io::Error::from_raw_os_error(libc::ENOBUFS));
            }
            self.outgoing.send(frame.to_vec()).unwrap();
            Ok(())
        }
    }
    struct Harness {
        guest: UnixDatagram,
        wake: Arc<UnixDatagram>,
        active: Arc<AtomicBool>,
        reads: Arc<AtomicUsize>,
        writes: Arc<AtomicUsize>,
        blocked: Arc<AtomicBool>,
        outgoing: mpsc::Receiver<Vec<u8>>,
        worker: Option<JoinHandle<io::Result<Counters>>>,
    }
    impl Harness {
        fn new(incoming: Vec<Vec<u8>>, blocked: bool) -> Self {
            let (socket, guest) = socket_pair().unwrap();
            // Force real downstream congestion, independently of host defaults.
            let size: libc::c_int = 4096;
            assert_eq!(
                unsafe {
                    libc::setsockopt(
                        guest.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_RCVBUF,
                        (&size as *const libc::c_int).cast(),
                        size_of_val(&size) as _,
                    )
                },
                0
            );
            guest.set_nonblocking(false).unwrap();
            guest
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let (notifications, wake) = UnixDatagram::pair().unwrap();
            notifications.set_nonblocking(true).unwrap();
            wake.set_nonblocking(true).unwrap();
            let wake = Arc::new(wake);
            let active = Arc::new(AtomicBool::new(true));
            let reads = Arc::new(AtomicUsize::new(0));
            let writes = Arc::new(AtomicUsize::new(0));
            let blocked = Arc::new(AtomicBool::new(blocked));
            let (tx, outgoing) = mpsc::channel();
            let interface = TestInterface {
                notifications,
                incoming: Arc::new(Mutex::new(incoming.into())),
                outgoing: tx,
                reads: reads.clone(),
                writes: writes.clone(),
                blocked: blocked.clone(),
            };
            let settings = Settings {
                interface_id: String::new(),
                operation_mode: "host".into(),
                start_address: None,
                end_address: None,
                subnet_mask: None,
                nat66_prefix: None,
                shared_interface: None,
                network_id: None,
                subnet: "192.168.105.0/24".into(),
                firewall: false,
            };
            let policy = Policy::new(&settings).unwrap();
            let running = active.clone();
            let worker = std::thread::spawn(move || forward(interface, socket, running, policy));
            wake.send(&[0]).unwrap();
            Self {
                guest,
                wake,
                active,
                reads,
                writes,
                blocked,
                outgoing,
                worker: Some(worker),
            }
        }
        fn stop(&mut self) -> Counters {
            self.active.store(false, Ordering::Relaxed);
            self.wake.send(&[0]).unwrap();
            self.worker.take().unwrap().join().unwrap().unwrap()
        }
    }
    impl Drop for Harness {
        fn drop(&mut self) {
            if self.worker.is_some() {
                self.stop();
            }
        }
    }
    fn frame(sequence: u64) -> Vec<u8> {
        let mut frame = vec![0; 1500];
        frame[..8].copy_from_slice(&sequence.to_be_bytes());
        frame[12..14].copy_from_slice(&0x0806u16.to_be_bytes());
        frame
    }
    fn wait_for(counter: &AtomicUsize) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while counter.load(Ordering::SeqCst) == 0 {
            assert!(
                Instant::now() < deadline,
                "packet worker did not make progress"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn full_guest_queue_preserves_batches_order_and_reverse_traffic() {
        let frames: Vec<_> = (0..200).map(frame).collect();
        let mut h = Harness::new(frames.clone(), false);
        wait_for(&h.reads);
        // Let the tiny receive queue fill before consuming any download frames.
        std::thread::sleep(Duration::from_millis(20));
        h.guest.send(&frame(999)).unwrap();
        assert_eq!(
            h.outgoing.recv_timeout(Duration::from_secs(2)).unwrap(),
            frame(999)
        );
        assert_eq!(
            h.reads.load(Ordering::SeqCst),
            1,
            "must not read another batch while downstream is stalled"
        );
        let mut received = vec![0; FRAME_SIZE];
        for expected in frames {
            let size = h.guest.recv(&mut received).unwrap();
            assert_eq!(&received[..size], expected);
        }
        let counters = h.stop();
        assert_eq!(counters.to_guest, 200);
        assert_eq!(counters.from_guest, 1);
        assert!(counters.congested > 0);
    }

    #[test]
    fn full_vmnet_queue_retains_guest_frames_without_blocking_download() {
        let mut h = Harness::new(vec![frame(999)], true);
        for sequence in 0..3 {
            h.guest.send(&frame(sequence)).unwrap();
        }
        wait_for(&h.writes);
        let mut received = vec![0; FRAME_SIZE];
        let size = h.guest.recv(&mut received).unwrap();
        assert_eq!(&received[..size], frame(999));
        h.blocked.store(false, Ordering::SeqCst);
        for sequence in 0..3 {
            assert_eq!(
                h.outgoing.recv_timeout(Duration::from_secs(2)).unwrap(),
                frame(sequence)
            );
        }
        let counters = h.stop();
        assert_eq!(counters.from_guest, 3);
        assert!(counters.congested > 0);
    }
}
