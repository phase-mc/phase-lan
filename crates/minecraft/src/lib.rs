//! Phase: everything about Minecraft Java Edition itself.
//!
//! The network tunnel ([`phase_tunnel`](../phase_tunnel/index.html)) knows nothing
//! about the game: it forwards bytes. This crate holds what depends on how the game works.
//!
//! - [`lan`]: LAN announcements (the "Local network" list);
//! - [`status`]: server status query (version, name, players);
//! - [`voice`]: finding voice mod ports on the host PC;
//! - [`nickname`]: the user's in-game nickname on this PC (running game, launchers);
//! - [`mods`]: the `mods` folder: lookup, `.jar` list with hashes, backup of the guest's mods;
//! - [`handshake`]: the start of the game stream: which nickname the game joins the world with;
//! - [`world`]: the host's active world (`saves/<world>`, currently open in the game);
//! - [`palette`]: the twelve Phase colors and their in-game counterparts;
//! - [`datapack`]: the hidden datapack: header, nickname colors, prefixes and highlight in the game.

pub mod datapack;
mod game_process;
pub mod handshake;
pub mod lan;
pub mod mods;
pub mod nickname;
pub mod palette;
pub mod status;
pub mod voice;
pub mod world;
