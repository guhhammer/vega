//! The composed libp2p behaviour — one struct per tier of the ladder.

use crate::config::NodeConfig;
use crate::protocol::{Request, Response, IDENTIFY_PROTOCOL, KAD_PROTOCOL, MESSAGE_PROTOCOL};
use libp2p::swarm::behaviour::toggle::Toggle;
use libp2p::{
    autonat, dcutr, identify, identity, kad, mdns, ping, relay, request_response,
    swarm::NetworkBehaviour, upnp, PeerId, StreamProtocol,
};
use std::time::Duration;

// The derive generates the event enum; Debug on the behaviour itself would mean
// printing every sub-behaviour's internal state, which is neither useful nor
// small. A short summary is what anyone actually wants in a log.
#[derive(NetworkBehaviour)]
pub struct Vega {
    /// T0 — same LAN. The only discovery that works with no internet at all.
    pub mdns: Toggle<mdns::tokio::Behaviour>,

    /// T2 — rendezvous. Namespaced so this is our own DHT, not the public one.
    pub kad: kad::Behaviour<kad::store::MemoryStore>,

    /// T1 — is this node publicly reachable? Decides whether we need a relay
    /// reservation, and whether we are able to be one for others.
    pub autonat: autonat::Behaviour,

    /// T1 — ask the router for a port mapping. Desktop only: on a phone this is
    /// pointless (carrier NAT ignores it) and costs battery.
    pub upnp: Toggle<upnp::tokio::Behaviour>,

    /// T1 — upgrade a relayed connection to a direct one by hole punching.
    pub dcutr: dcutr::Behaviour,

    /// T3 — using someone else's relay.
    pub relay_client: relay::client::Behaviour,

    /// T3 — being a relay for someone else. Off by default: a phone should
    /// never carry a stranger's traffic on a metered connection.
    pub relay: Toggle<relay::Behaviour>,

    /// Envelope delivery, parking, and collection.
    pub messaging: request_response::cbor::Behaviour<Request, Response>,

    /// Housekeeping: learn peers' addresses and protocols, keep NAT bindings warm.
    pub identify: identify::Behaviour,
    pub ping: ping::Behaviour,
}

impl Vega {
    pub fn new(
        key: &identity::Keypair,
        relay_client: relay::client::Behaviour,
        config: &NodeConfig,
    ) -> std::io::Result<Self> {
        let peer_id = PeerId::from(key.public());

        let mdns = if config.enable_mdns {
            Toggle::from(Some(mdns::tokio::Behaviour::new(
                mdns::Config::default(),
                peer_id,
            )?))
        } else {
            Toggle::from(None)
        };

        // A private protocol id keeps this network separate from the public
        // IPFS DHT; the record TTL matches the rendezvous epoch, so stale
        // addresses age out on their own rather than being chased.
        let mut kad_config = kad::Config::new(KAD_PROTOCOL);
        kad_config
            .set_record_ttl(Some(Duration::from_secs(
                crate::rendezvous::RECORD_TTL_SECS,
            )))
            .set_publication_interval(Some(Duration::from_secs(
                crate::rendezvous::RECORD_TTL_SECS / 2,
            )))
            .set_query_timeout(Duration::from_secs(30));

        let mut kad =
            kad::Behaviour::with_config(peer_id, kad::store::MemoryStore::new(peer_id), kad_config);
        // Whether this node answers queries and stores records for others. A
        // network of pure clients has nowhere to put anything, so the desktop
        // default is `Server`; a phone opts out, because serving means taking
        // connections from peers it never chose and handing each of them an
        // address. See [`NodeConfig::kad_mode`].
        kad.set_mode(Some(config.kad_mode));

        // Size limits are set explicitly rather than left to the codec default:
        // this is the one place a stranger's bytes reach our allocator before we
        // have decided we want them.
        let codec = request_response::cbor::codec::Codec::default()
            .set_request_size_maximum(crate::protocol::MAX_REQUEST_BYTES as u64)
            .set_response_size_maximum(crate::protocol::MAX_RESPONSE_BYTES as u64);

        let messaging = request_response::Behaviour::with_codec(
            codec,
            [(MESSAGE_PROTOCOL, request_response::ProtocolSupport::Full)],
            request_response::Config::default().with_request_timeout(Duration::from_secs(30)),
        );

        let relay = if config.act_as_relay {
            Toggle::from(Some(relay::Behaviour::new(
                peer_id,
                relay::Config::default(),
            )))
        } else {
            Toggle::from(None)
        };

        let upnp = if config.enable_upnp {
            Toggle::from(Some(upnp::tokio::Behaviour::default()))
        } else {
            Toggle::from(None)
        };

        Ok(Self {
            mdns,
            kad,
            autonat: autonat::Behaviour::new(peer_id, autonat::Config::default()),
            upnp,
            dcutr: dcutr::Behaviour::new(peer_id),
            relay_client,
            relay,
            messaging,
            identify: identify::Behaviour::new(
                identify::Config::new(IDENTIFY_PROTOCOL.to_string(), key.public())
                    .with_push_listen_addr_updates(true),
            ),
            ping: ping::Behaviour::default(),
        })
    }
}

impl std::fmt::Debug for Vega {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vega")
            .field("mdns", &self.mdns.is_enabled())
            .field("relay_server", &self.relay.is_enabled())
            .field("upnp", &self.upnp.is_enabled())
            .finish_non_exhaustive()
    }
}

/// The protocols this build speaks, for diagnostics.
pub fn protocols() -> Vec<StreamProtocol> {
    vec![KAD_PROTOCOL, MESSAGE_PROTOCOL]
}
