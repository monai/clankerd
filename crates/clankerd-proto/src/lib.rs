//! Host<->guest protocol shared by libclankerd and clankerd-guestd.
//!
//! * [`varlink`]: the varlink wire format (NUL-terminated JSON messages).
//! * [`guest`]: the `io.clankerd.Guest` interface (types and method names).
//! * [`frame`]: the frame codec for raw streams after a varlink `upgrade`
//!   (exec; reusable by other upgraded streams).
//! * [`tunnel`]: tunnel targets and its own framing.
//! * [`listen_fds`]: the `LISTEN_FDS` socket handoff convention.

pub mod frame;
pub mod guest;
pub mod listen_fds;
pub mod spawn;
pub mod tunnel;
pub mod varlink;
