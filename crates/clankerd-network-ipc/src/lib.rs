use std::io;
use std::os::fd::OwnedFd;

/// Datagram congestion drops a packet; it does not invalidate the connection.
pub fn is_packet_congestion(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::WouldBlock || error.raw_os_error() == Some(libc::ENOBUFS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_packet_queue_congestion_is_nonfatal() {
        for (code, expected) in [
            (libc::EAGAIN, true),
            (libc::ENOBUFS, true),
            (libc::ECONNRESET, false),
            (libc::ENOTCONN, false),
            (libc::EDESTADDRREQ, false),
            (libc::EMSGSIZE, false),
            (libc::EBADF, false),
        ] {
            assert_eq!(
                is_packet_congestion(&io::Error::from_raw_os_error(code)),
                expected,
                "errno={code}"
            );
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub interface_id: String,
    pub operation_mode: String,
    pub start_address: Option<String>,
    pub end_address: Option<String>,
    pub subnet_mask: Option<String>,
    pub nat66_prefix: Option<String>,
    pub shared_interface: Option<String>,
    pub network_id: Option<String>,
    pub subnet: String,
    pub firewall: bool,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Create { key: String, settings: Settings },
    Attach { key: String, generation: String },
    Stop { key: String, generation: String },
    Status { key: String, generation: String },
    List,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Response {
    pub error: Option<String>,
    pub generation: Option<String>,
    pub prefix: Option<String>,
    pub connected: bool,
    pub interfaces: Vec<String>,
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use std::ffi::{CStr, CString, c_char, c_void};
    use std::os::fd::FromRawFd;
    unsafe extern "C" {
        fn clankerd_xpc_connect() -> *mut c_void;
        fn clankerd_xpc_close(connection: *mut c_void);
        fn clankerd_xpc_request(
            connection: *mut c_void,
            json: *const c_char,
            output: *mut *mut c_char,
            fd: *mut i32,
        ) -> i32;
        fn clankerd_xpc_serve(handler: Handler, disconnected: Disconnected, shutdown: Shutdown);
    }
    pub type Handler =
        unsafe extern "C" fn(*const c_char, u32, i32, u64, *mut *mut c_char, *mut i32);
    pub type Disconnected = unsafe extern "C" fn(u64);
    pub type Shutdown = unsafe extern "C" fn();
    pub struct Connection(*mut c_void);
    // XPC connections support requests from arbitrary threads.
    unsafe impl Send for Connection {}
    impl Connection {
        pub fn connect() -> io::Result<Self> {
            let pointer = unsafe { clankerd_xpc_connect() };
            if pointer.is_null() {
                return Err(io::Error::other(
                    "cannot connect to io.clankerd.network; install-network-helper first",
                ));
            }
            Ok(Self(pointer))
        }
        pub fn request(&self, request: &Request) -> io::Result<(Response, Option<OwnedFd>)> {
            let json = CString::new(serde_json::to_vec(request)?)?;
            let mut output = std::ptr::null_mut();
            let mut fd = -1;
            let status =
                unsafe { clankerd_xpc_request(self.0, json.as_ptr(), &mut output, &mut fd) };
            let descriptor = (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) });
            if status != 0 {
                return Err(io::Error::other(
                    "network daemon unavailable; install-network-helper first; restart VMs after daemon failure",
                ));
            }
            let parsed = serde_json::from_slice(unsafe { CStr::from_ptr(output).to_bytes() });
            unsafe { libc::free(output.cast()) };
            let response: Response = parsed?;
            if let Some(error) = &response.error {
                return Err(io::Error::other(error.clone()));
            }
            Ok((response, descriptor))
        }
    }
    impl Drop for Connection {
        fn drop(&mut self) {
            unsafe { clankerd_xpc_close(self.0) };
        }
    }
    /// # Safety
    /// Callbacks must use the C ABI and remain valid until process exit.
    pub unsafe fn serve(handler: Handler, disconnected: Disconnected, shutdown: Shutdown) -> ! {
        unsafe { clankerd_xpc_serve(handler, disconnected, shutdown) };
        unreachable!()
    }
}
#[cfg(target_os = "macos")]
pub use native::*;

#[cfg(not(target_os = "macos"))]
pub struct Connection;
#[cfg(not(target_os = "macos"))]
impl Connection {
    pub fn connect() -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "network daemon requires macOS",
        ))
    }
    pub fn request(&self, _: &Request) -> io::Result<(Response, Option<OwnedFd>)> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "network daemon requires macOS",
        ))
    }
}
