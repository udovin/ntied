//! Relay-server implementation + relay wire format shared between the
//! server-side accept loop and the client-side `RelayConnection`.
//!
//! - [`control`] -- control-channel messages (hole-punch signalling).
//! - [`tunnel`] -- tunnel-message header used by both relay-pump and
//!   the client-side multiplex.
//!
//! The actual server implementation (`RelayNode`, accept loop, pumps)
//! will live in this module as well.

pub mod control;
mod server;
pub mod tunnel;

pub use control::ControlMsg;
pub use server::RelayNode;
pub use tunnel::TUNNEL_HEADER_SIZE;
