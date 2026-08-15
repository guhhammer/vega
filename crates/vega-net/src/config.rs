//! How this node participates in the network.
//!
//! The defaults describe a desktop: reachable, willing to relay, willing to hold
//! mail. [`NodeConfig::mobile`] describes a phone: a leaf node that takes from
//! the network without being asked to carry anyone else's traffic. That
//! asymmetry is deliberate — see the platform section of the design.

use libp2p::Multiaddr;

#[derive(Debug, Clone)]
pub struct NodeConfig {
    /// Addresses to listen on. QUIC first; TCP is the fallback for networks
    /// that block or throttle UDP.
    pub listen: Vec<Multiaddr>,

    /// Where to find the DHT the first time. Any peer works; these are just the
    /// ones we happen to know at build time.
    pub bootstrap: Vec<Multiaddr>,

    pub enable_mdns: bool,
    pub enable_upnp: bool,

    /// Serve as a Circuit Relay v2 for other peers.
    pub act_as_relay: bool,

    /// Hold parked mail for other peers.
    pub act_as_mailbox: bool,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            listen: vec![
                // IPv6 first, and unspecified so we bind every interface: many
                // mobile carriers hand out a globally routable v6 address, which
                // sidesteps NAT entirely and is the cheapest win on the ladder.
                "/ip6/::/udp/0/quic-v1".parse().expect("static multiaddr"),
                "/ip4/0.0.0.0/udp/0/quic-v1"
                    .parse()
                    .expect("static multiaddr"),
                "/ip6/::/tcp/0".parse().expect("static multiaddr"),
                "/ip4/0.0.0.0/tcp/0".parse().expect("static multiaddr"),
            ],
            bootstrap: Vec::new(),
            enable_mdns: true,
            enable_upnp: true,
            act_as_relay: true,
            act_as_mailbox: true,
        }
    }
}

impl NodeConfig {
    /// A phone: discovers and receives, but never carries traffic for strangers
    /// and never asks the carrier's NAT for a port mapping it will not get.
    pub fn mobile() -> Self {
        Self {
            enable_upnp: false,
            act_as_relay: false,
            act_as_mailbox: false,
            ..Default::default()
        }
    }

    pub fn with_bootstrap(mut self, addrs: Vec<Multiaddr>) -> Self {
        self.bootstrap = addrs;
        self
    }

    pub fn with_listen(mut self, addrs: Vec<Multiaddr>) -> Self {
        self.listen = addrs;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_listen_addresses_parse() {
        assert_eq!(NodeConfig::default().listen.len(), 4);
    }

    #[test]
    fn mobile_does_not_volunteer_for_other_peoples_traffic() {
        let m = NodeConfig::mobile();
        assert!(!m.act_as_relay);
        assert!(!m.act_as_mailbox);
        assert!(!m.enable_upnp);
        // But it still finds peers on the local network.
        assert!(m.enable_mdns);
    }
}
