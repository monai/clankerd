//! Own the native interface, its callbacks, and the socket that wakes packet I/O.
use clankerd_network_ipc::Settings;
use std::{
    ffi::{CStr, CString, c_char, c_void},
    io,
    net::Ipv6Addr,
    os::{fd::AsRawFd, unix::net::UnixDatagram},
    sync::Arc,
};

unsafe extern "C" {
    fn clankerd_vmnet_create(
        identity: *const c_char,
        mode: *const c_char,
        start: *const c_char,
        end: *const c_char,
        mask: *const c_char,
        prefix: *const c_char,
        shared: *const c_char,
        actual_prefix: *mut *mut c_char,
    ) -> *mut c_void;
    fn clankerd_vmnet_stop(handle: *mut c_void);
    fn clankerd_vmnet_watch(handle: *mut c_void, fd: i32) -> i32;
    fn clankerd_vmnet_read(
        handle: *mut c_void,
        buffers: *mut u8,
        stride: usize,
        sizes: *mut usize,
        capacity: i32,
    ) -> i32;
    fn clankerd_vmnet_write(handle: *mut c_void, buffer: *mut u8, size: usize) -> i32;
    fn clankerd_vmnet_list() -> *mut c_char;
}
pub struct Interface {
    handle: *mut c_void,
    // Kept alive until vmnet_stop has drained the callback queue.
    wake: Arc<UnixDatagram>,
    notifications: UnixDatagram,
}
// The worker owns packet I/O; dispatch callbacks only signal the wake socket.
unsafe impl Send for Interface {}
impl Drop for Interface {
    fn drop(&mut self) {
        unsafe { clankerd_vmnet_stop(self.handle) };
    }
}
impl Interface {
    pub fn create(settings: &Settings) -> Result<(Self, Option<Ipv6Addr>), String> {
        let expected = settings
            .nat66_prefix
            .as_deref()
            .map(str::parse::<Ipv6Addr>)
            .transpose()
            .map_err(|e| e.to_string())?;
        let (notifications, wake) = UnixDatagram::pair().map_err(|e| e.to_string())?;
        notifications
            .set_nonblocking(true)
            .map_err(|e| e.to_string())?;
        wake.set_nonblocking(true).map_err(|e| e.to_string())?;
        let wake = Arc::new(wake);
        let strings = [
            Some(settings.interface_id.as_str()),
            Some(settings.operation_mode.as_str()),
            settings.start_address.as_deref(),
            settings.end_address.as_deref(),
            settings.subnet_mask.as_deref(),
            settings.nat66_prefix.as_deref(),
            settings.shared_interface.as_deref(),
        ]
        .map(|s| s.map(CString::new).transpose())
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
        let p: Vec<_> = strings
            .iter()
            .map(|s| s.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()))
            .collect();
        let mut prefix = std::ptr::null_mut();
        let handle =
            unsafe { clankerd_vmnet_create(p[0], p[1], p[2], p[3], p[4], p[5], p[6], &mut prefix) };
        if handle.is_null() {
            return Err("vmnet interface creation failed".into());
        }
        let interface = Self {
            handle,
            wake,
            notifications,
        };
        let actual = if prefix.is_null() {
            None
        } else {
            let value = unsafe { CStr::from_ptr(prefix) }
                .to_str()
                .map_err(|e| e.to_string())
                .and_then(|value| value.parse::<Ipv6Addr>().map_err(|e| e.to_string()));
            unsafe { libc::free(prefix.cast()) };
            Some(value?)
        };
        if expected != actual {
            return Err("vmnet did not acknowledge the requested IPv6 prefix".into());
        }
        if unsafe { clankerd_vmnet_watch(handle, interface.wake.as_raw_fd()) } != 0 {
            return Err("cannot subscribe to vmnet packet events".into());
        }
        Ok((interface, actual))
    }
    pub fn wake(&self) -> Arc<UnixDatagram> {
        self.wake.clone()
    }
}
impl crate::packets::PacketIo for Interface {
    fn notifications(&self) -> &UnixDatagram {
        &self.notifications
    }
    fn read(&mut self, batch: &mut crate::packets::Batch) -> io::Result<usize> {
        let count = unsafe {
            clankerd_vmnet_read(
                self.handle,
                batch.data.as_mut_ptr(),
                crate::packets::FRAME_SIZE,
                batch.sizes.as_mut_ptr(),
                crate::packets::BATCH_SIZE as i32,
            )
        };
        if count < 0 {
            Err(io::Error::other("vmnet read failed"))
        } else {
            Ok(count as usize)
        }
    }
    fn write(&mut self, frame: &[u8]) -> io::Result<()> {
        match unsafe { clankerd_vmnet_write(self.handle, frame.as_ptr().cast_mut(), frame.len()) } {
            0 => Ok(()),
            1 => Err(io::ErrorKind::WouldBlock.into()),
            _ => Err(io::Error::other("vmnet write failed")),
        }
    }
}

pub fn list() -> Result<Vec<String>, String> {
    let pointer = unsafe { clankerd_vmnet_list() };
    if pointer.is_null() {
        return Err("cannot list vmnet interfaces".into());
    }
    let names = unsafe { CStr::from_ptr(pointer) }
        .to_string_lossy()
        .lines()
        .map(str::to_owned)
        .collect();
    unsafe { libc::free(pointer.cast()) };
    Ok(names)
}
