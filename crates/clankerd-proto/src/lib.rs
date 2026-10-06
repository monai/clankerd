//! Host<->guest protocol shared by libclankerd and clankerd-guestd.
//!
//! * [`varlink`]: the varlink wire format (NUL-terminated JSON messages).
//! * [`guest`]: the `io.clankerd.Guest` interface (types and method names).
//!
//! The exec/tunnel frame codec lands here in later tickets.

pub mod guest;
pub mod varlink;
