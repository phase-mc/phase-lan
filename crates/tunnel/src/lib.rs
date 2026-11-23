//! Phase: the network core.
//!
//! How it works:
//! - The **host** accepts encrypted QUIC connections (iroh) only from allowed
//!   devices and forwards each game connection strictly to its own local game port
//!   `127.0.0.1:<port>`. A destination address from the guest is never accepted.
//! - The **guest** runs a local "twin" of the server on `127.0.0.1` and carries each
//!   game TCP connection as a separate stream inside one QUIC connection to the host.
//!
//! The connection tries to go direct (NAT hole punching), and if that fails, it goes through
//! a relay server. Traffic is encrypted device to device in both cases.

pub mod access;
pub mod echo;
pub mod endpoint;
pub mod events;
pub mod forward;
pub mod fsutil;
pub mod guest;
pub mod host;
pub mod identity;
pub mod metrics;
pub mod mods;
mod sockets;
pub mod voice;
pub mod wire;

pub use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayUrl, SecretKey, Signature, endpoint::Connection,
};
pub use iroh_tickets::endpoint::EndpointTicket;
