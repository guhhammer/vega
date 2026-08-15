//! Vega's transport ladder.
//!
//! For each contact the app maintains a set of candidate paths and races the
//! viable ones; the first to complete a handshake wins. The tiers, cheapest and
//! most private first:
//!
//! | Tier | Mechanism | Built from |
//! |------|-----------|------------|
//! | T0 | same LAN | `mdns` + `quic` |
//! | T1 | direct across the internet | IPv6, `upnp`, `autonat`, `dcutr` |
//! | T2 | rendezvous | `kad` on `/vega/kad/1.0.0` |
//! | T3 | relayed | `relay` (Circuit Relay v2) |
//! | T4 | recipient offline | [`mailbox`] over `/vega/msg/1.0.0` |
//!
//! Nothing in this crate can read a message. It moves opaque envelopes that
//! only [`vega_core`] can open, which is what makes it safe for a stranger's
//! node to carry them.

pub mod behaviour;
pub mod config;
pub mod error;
pub mod mailbox;
pub mod node;
pub mod protocol;
pub mod rendezvous;

pub use config::NodeConfig;
pub use error::{Error, Result};
pub use mailbox::{Mailbox, ParkOutcome};
pub use node::{peer_id_of, with_peer, NetEvent, Node, NodeHandle, Source};
pub use protocol::{Request, Response};
pub use rendezvous::{open_record, seal_record, AddressRecord};

pub use libp2p::{Multiaddr, PeerId};
