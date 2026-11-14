use crate::{
    Peer, Reply, Request, Response, Server, codec, ffi,
    xpc::{Object, Ref},
};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    io,
    os::fd::{AsFd, BorrowedFd},
};

const WIRE_VERSION: u64 = 1;

pub struct Connection(Object);
// A libXPC connection may be moved between threads; it is not exposed as Sync.
unsafe impl Send for Connection {}
impl Connection {
    pub fn connect() -> io::Result<Self> {
        let connection = unsafe { Object::from_owned(ffi::clankerd_xpc_connect()) }?;
        // XPC connections are lazy; request() reports service availability.
        Ok(Self(connection))
    }
    pub fn request(&self, request: &Request) -> io::Result<Reply> {
        let message = Object::dictionary()?;
        unsafe { write_message(message.borrow(), request, None) }?;
        let reply = unsafe {
            Object::from_owned(ffi::xpc_connection_send_message_with_reply_sync(
                self.0.as_raw(),
                message.as_raw(),
            ))
        }?;
        let result: Result<Response, String> = read_message(reply.borrow())?;
        Ok(Reply {
            response: result.map_err(io::Error::other)?,
            packet: reply.borrow().receive_fd(c"packet_fd")?,
        })
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        unsafe { ffi::xpc_connection_cancel(self.0.as_raw()) }
    }
}

/// The dictionary must have no outstanding borrowed descendants while it is filled.
unsafe fn write_message<T: Serialize + ?Sized>(
    message: Ref<'_>,
    body: &T,
    packet: Option<BorrowedFd<'_>>,
) -> Result<(), codec::Error> {
    let version = codec::to_object(&WIRE_VERSION)?;
    let body = codec::to_object(body)?;
    unsafe { message.set(c"version", &version) }?;
    unsafe { message.set(c"body", &body) }?;
    if let Some(packet) = packet {
        unsafe { message.attach_fd(c"packet_fd", packet) }?;
    }
    Ok(())
}
fn read_message<T: DeserializeOwned>(message: Ref<'_>) -> io::Result<T> {
    if message.is(&raw const ffi::_xpc_type_error) {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "network daemon unavailable; install-network-helper first; restart VMs after daemon failure",
        ));
    }
    let version = message
        .get(c"version")?
        .ok_or_else(|| codec::Error("missing network IPC version".into()))?;
    version.require(
        &raw const ffi::_xpc_type_uint64,
        "protocol version (uint64)",
    )?;
    if unsafe { ffi::xpc_uint64_get_value(version.as_raw()) } != WIRE_VERSION {
        return Err(codec::Error("unsupported network IPC version".into()).into());
    }
    let body = message
        .get(c"body")?
        .ok_or_else(|| codec::Error("missing network IPC body".into()))?;
    Ok(codec::from_object(body)?)
}

pub fn serve(server: Server) -> ! {
    // dispatch_main never returns; the immutable callback table lives on this stack.
    unsafe {
        ffi::clankerd_xpc_serve(
            (&server as *const Server).cast_mut().cast(),
            request,
            disconnected,
            shutdown,
        )
    }
}
extern "C" fn request(
    message: ffi::Raw,
    reply: ffi::Raw,
    uid: u32,
    pid: i32,
    connection: u64,
    context: ffi::Raw,
) -> bool {
    let server = unsafe { &*context.cast::<Server>() };
    let message = unsafe { Ref::from_raw(message) }.expect("libXPC supplied a null request");
    let reply = unsafe { Ref::from_raw(reply) }.expect("libXPC supplied a null reply");
    let outcome = read_message::<Request>(message)
        .map_err(|e| e.to_string())
        .and_then(|request| {
            (server.request)(
                request,
                Peer {
                    uid,
                    pid,
                    connection,
                },
            )
        });
    let (result, packet) = match outcome {
        Ok(reply) => (Ok(reply.response), reply.packet),
        Err(error) => (Err(error), None),
    };
    // XPC duplicates a borrowed FD; its Rust owner drops after the reply is filled.
    match unsafe { write_message(reply, &result, packet.as_ref().map(AsFd::as_fd)) } {
        Ok(()) => true,
        Err(error) => {
            eprintln!("cannot encode network reply: {error}");
            false // The C adapter cancels the connection; no fabricated fallback response.
        }
    }
}
extern "C" fn disconnected(connection: u64, context: ffi::Raw) {
    let server = unsafe { &*context.cast::<Server>() };
    (server.disconnected)(connection);
}
extern "C" fn shutdown(context: ffi::Raw) {
    let server = unsafe { &*context.cast::<Server>() };
    (server.shutdown)();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(request: fn(Request, Peer) -> Result<Reply, String>) -> Server {
        Server {
            request,
            disconnected: |_| {},
            shutdown: || {},
        }
    }
    fn dispatch(message: &Object, server: &Server) -> Object {
        let reply = Object::dictionary().unwrap();
        assert!(request(
            message.as_raw(),
            reply.as_raw(),
            501,
            1234,
            7,
            (server as *const Server).cast_mut().cast()
        ));
        reply
    }

    #[test]
    fn malformed_requests_receive_a_complete_typed_error_reply() {
        let server = service(|_, _| panic!("malformed request reached domain handler"));
        let reply = dispatch(&Object::dictionary().unwrap(), &server);
        let result: Result<Response, String> = read_message(reply.borrow()).unwrap();
        assert!(result.unwrap_err().contains("version"));
    }

    #[test]
    fn domain_errors_are_returned_to_the_client() {
        let message = Object::dictionary().unwrap();
        unsafe { write_message(message.borrow(), &Request::List {}, None) }.unwrap();
        let server = service(|_, peer| {
            assert_eq!((peer.uid, peer.pid, peer.connection), (501, 1234, 7));
            Err("network is already attached".into())
        });
        let reply = dispatch(&message, &server);
        let result: Result<Response, String> = read_message(reply.borrow()).unwrap();
        assert_eq!(result.unwrap_err(), "network is already attached");
    }

    #[test]
    fn incompatible_versions_are_rejected_before_dispatch() {
        let mut message = Object::dictionary().unwrap();
        unsafe { write_message(message.borrow(), &Request::List {}, None) }.unwrap();
        message
            .set(c"version", codec::to_object(&(WIRE_VERSION + 1)).unwrap())
            .unwrap();
        let server = service(|_, _| panic!("incompatible request reached domain handler"));
        let reply = dispatch(&message, &server);
        let result: Result<Response, String> = read_message(reply.borrow()).unwrap();
        assert!(result.unwrap_err().contains("unsupported"));
    }
}
