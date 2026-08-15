//! Wire protocols Vega speaks, and the identifiers that keep its network its own.
//!
//! Every protocol id is namespaced under `/vega/`. That is what makes this a
//! private DHT rather than a corner of the public IPFS one: a node that does
//! not speak `/vega/kad/1.0.0` is never routed to, so we inherit neither that
//! network's record pressure nor its observability.

use libp2p::StreamProtocol;
use serde::{Deserialize, Serialize};
use vega_core::tag::{CollectToken, Tag};

pub const KAD_PROTOCOL: StreamProtocol = StreamProtocol::new("/vega/kad/1.0.0");
pub const IDENTIFY_PROTOCOL: &str = "/vega/id/1.0.0";
pub const MESSAGE_PROTOCOL: StreamProtocol = StreamProtocol::new("/vega/msg/1.0.0");

/// Refuse anything larger than this outright. A relay or mailbox peer is doing
/// someone else a favour; it should never be possible to exhaust its memory by
/// asking politely.
pub const MAX_ENVELOPE_BYTES: usize = 256 * 1024;

/// Ceiling on a whole request, envelope plus framing. Anything larger is
/// refused before it is parsed.
pub const MAX_REQUEST_BYTES: usize = MAX_ENVELOPE_BYTES + 16 * 1024;

/// A collect response can carry a full mailbox, so it is allowed to be larger.
pub const MAX_RESPONSE_BYTES: usize = MAX_PARKED_PER_TAG * MAX_ENVELOPE_BYTES;

/// How long a peer will hold parked mail before dropping it.
pub const MAX_PARK_SECS: u64 = 7 * 24 * 3600;

/// Per-tag cap on parked envelopes, so one sender cannot fill a mailbox.
pub const MAX_PARKED_PER_TAG: usize = 32;

/// Total envelopes one node will hold for everyone combined.
pub const MAX_PARKED_TOTAL: usize = 4096;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    /// Hand an envelope straight to the device it belongs to.
    Deliver { envelope: Vec<u8> },

    /// Ask this peer to hold an envelope until its recipient collects it.
    /// Tier 4 — the recipient is offline.
    Park {
        tag: Tag,
        /// Derived from the sender/recipient pairwise secret. The mailbox keeps
        /// it and hands the envelope only to someone who can reproduce it.
        token: CollectToken,
        envelope: Vec<u8>,
        expires_at: u64,
    },

    /// Collect anything parked under these tags. Tags rotate hourly, so a client
    /// asks for the current epoch and one either side of it.
    ///
    /// The token is what makes this safe: the tag alone is visible to every peer
    /// that relayed a message, and without a second secret any of them could
    /// collect — and so destroy — mail meant for someone else.
    Collect { claims: Vec<(Tag, CollectToken)> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    Accepted,
    Collected { envelopes: Vec<Vec<u8>> },
    Refused { reason: String },
}

impl Response {
    pub fn refused(reason: impl Into<String>) -> Self {
        Response::Refused {
            reason: reason.into(),
        }
    }
}
