//! Host<->guest protocol shared by libclankerd and clankerd-guestd.
//!
//! * [`varlink`]: the varlink wire format (NUL-terminated JSON messages).
//! * [`guest`]: the `io.clankerd.Guest` interface (types and method names).
//! * [`tunnel`]: tunnel targets and minimal framing (the exec codec lands separately).
//! * [`listen_fds`]: the `LISTEN_FDS` socket handoff convention.

pub mod guest;
pub mod listen_fds;
pub mod tunnel;
pub mod varlink;
