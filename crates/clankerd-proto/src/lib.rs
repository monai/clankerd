//! Host<->guest protocol shared by libclankerd and clankerd-guestd.
//!
//! * [`varlink`]: the varlink wire format (NUL-terminated JSON messages).
//! * [`guest`]: the `io.clankerd.Guest` interface (types and method names).
//! * [`frame`]: the frame codec for raw streams after a varlink `upgrade`
//!   (exec now, tunnels later).

pub mod frame;
pub mod guest;
pub mod varlink;
