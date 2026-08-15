//! The swarm task, and the handle the rest of the app drives it through.
//!
//! Everything libp2p happens on one task; callers talk to it over a channel and
//! get events back over another. That keeps `Swarm` — which is neither `Sync`
//! nor cheap to move — off the UI thread and out of the app's type signatures.

use crate::behaviour::{Vega, VegaEvent};
use crate::config::NodeConfig;
use crate::error::{Error, Result};
use crate::mailbox::{Mailbox, ParkOutcome};
use crate::protocol::{Request, Response, MAX_ENVELOPE_BYTES};
use futures::StreamExt;
use libp2p::core::multiaddr::Protocol;
use libp2p::kad::{self, QueryId};
use libp2p::request_response::{Message as RrMessage, OutboundRequestId, ResponseChannel};
use libp2p::swarm::SwarmEvent;
use libp2p::{identity, mdns, noise, relay, tcp, yamux, Multiaddr, PeerId, Swarm};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use vega_core::tag::{CollectToken, Tag};

/// What the network task reports back.
#[derive(Debug, Clone)]
pub enum NetEvent {
    Listening(Multiaddr),
    /// An address other peers can reach us on — confirmed by AutoNAT or UPnP.
    ExternalAddress(Multiaddr),
    PeerDiscovered {
        peer: PeerId,
        addrs: Vec<Multiaddr>,
        source: Source,
    },
    PeerExpired(PeerId),
    /// An envelope arrived. Opaque here — only `vega-core` can open it.
    Envelope {
        from: PeerId,
        bytes: Vec<u8>,
    },
    /// A relay accepted our reservation, so we now have a dialable address.
    RelayReserved(PeerId),
    /// A relayed connection was upgraded to a direct one.
    HolePunched(PeerId),
    /// A peer we can hand envelopes to right now.
    PeerConnected(PeerId),
    PeerDisconnected(PeerId),
    Warning(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// T0 — found on the local network.
    Mdns,
    /// T2 — learned from the DHT.
    Dht,
    /// Told to us by a peer we were already talking to.
    Identify,
}

enum Command {
    Dial {
        addr: Multiaddr,
        reply: oneshot::Sender<Result<()>>,
    },
    Deliver {
        peer: PeerId,
        envelope: Vec<u8>,
        reply: oneshot::Sender<Result<()>>,
    },
    Park {
        peer: PeerId,
        tag: Tag,
        token: CollectToken,
        envelope: Vec<u8>,
        expires_at: u64,
        reply: oneshot::Sender<Result<()>>,
    },
    Collect {
        peer: PeerId,
        claims: Vec<(Tag, CollectToken)>,
        reply: oneshot::Sender<Result<Vec<Vec<u8>>>>,
    },
    Publish {
        key: [u8; 32],
        value: Vec<u8>,
        reply: oneshot::Sender<Result<()>>,
    },
    Lookup {
        key: [u8; 32],
        reply: oneshot::Sender<Result<Vec<Vec<u8>>>>,
    },
    ReserveRelay {
        relay: Multiaddr,
        reply: oneshot::Sender<Result<()>>,
    },
    Listeners {
        reply: oneshot::Sender<Vec<Multiaddr>>,
    },
    LocalPeerId {
        reply: oneshot::Sender<PeerId>,
    },
    Shutdown,
}

/// Cheap, cloneable handle to the running node.
#[derive(Clone, Debug)]
pub struct NodeHandle {
    tx: mpsc::Sender<Command>,
}

impl NodeHandle {
    async fn ask<T>(&self, make: impl FnOnce(oneshot::Sender<T>) -> Command) -> Result<T> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(make(reply)).await?;
        Ok(rx.await?)
    }

    pub async fn local_peer_id(&self) -> Result<PeerId> {
        self.ask(|reply| Command::LocalPeerId { reply }).await
    }

    pub async fn listeners(&self) -> Result<Vec<Multiaddr>> {
        self.ask(|reply| Command::Listeners { reply }).await
    }

    pub async fn dial(&self, addr: Multiaddr) -> Result<()> {
        self.ask(|reply| Command::Dial { addr, reply }).await?
    }

    /// Tier 0/1/3 — hand an envelope to a peer we can currently reach.
    pub async fn deliver(&self, peer: PeerId, envelope: Vec<u8>) -> Result<()> {
        self.ask(|reply| Command::Deliver {
            peer,
            envelope,
            reply,
        })
        .await?
    }

    /// Tier 4 — ask a peer to hold an envelope until its recipient collects it.
    pub async fn park(
        &self,
        peer: PeerId,
        tag: Tag,
        token: CollectToken,
        envelope: Vec<u8>,
        expires_at: u64,
    ) -> Result<()> {
        self.ask(|reply| Command::Park {
            peer,
            tag,
            token,
            envelope,
            expires_at,
            reply,
        })
        .await?
    }

    /// Tier 4 — collect anything parked for us, proving we may have it.
    pub async fn collect(
        &self,
        peer: PeerId,
        claims: Vec<(Tag, CollectToken)>,
    ) -> Result<Vec<Vec<u8>>> {
        self.ask(|reply| Command::Collect {
            peer,
            claims,
            reply,
        })
        .await?
    }

    /// Tier 2 — publish where we can be reached, under a contact-derived key.
    pub async fn publish(&self, key: [u8; 32], value: Vec<u8>) -> Result<()> {
        self.ask(|reply| Command::Publish { key, value, reply })
            .await?
    }

    /// Tier 2 — look up where a contact says they are.
    pub async fn lookup(&self, key: [u8; 32]) -> Result<Vec<Vec<u8>>> {
        self.ask(|reply| Command::Lookup { key, reply }).await?
    }

    /// Tier 3 — ask a relay for a reservation so unreachable peers can find us.
    pub async fn reserve_relay(&self, relay: Multiaddr) -> Result<()> {
        self.ask(|reply| Command::ReserveRelay { relay, reply })
            .await?
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.tx.send(Command::Shutdown).await?;
        Ok(())
    }
}

/// In-flight request/response calls, waiting on a peer to answer.
enum Pending {
    Deliver(oneshot::Sender<Result<()>>),
    Park(oneshot::Sender<Result<()>>),
    Collect(oneshot::Sender<Result<Vec<Vec<u8>>>>),
}

/// In-flight DHT queries.
enum Query {
    Put(oneshot::Sender<Result<()>>),
    Get {
        reply: oneshot::Sender<Result<Vec<Vec<u8>>>>,
        found: Vec<Vec<u8>>,
    },
}

pub struct Node {
    swarm: Swarm<Vega>,
    config: NodeConfig,
    mailbox: Mailbox,
    commands: mpsc::Receiver<Command>,
    events: mpsc::Sender<NetEvent>,
    pending: HashMap<OutboundRequestId, Pending>,
    queries: HashMap<QueryId, Query>,
}

impl std::fmt::Debug for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `Swarm` has no Debug, and printing every in-flight query would be
        // noise anyway. Counts are what a stuck node looks like.
        f.debug_struct("Node")
            .field("peer_id", self.swarm.local_peer_id())
            .field("mailbox", &self.mailbox.len())
            .field("pending_requests", &self.pending.len())
            .field("pending_queries", &self.queries.len())
            .finish()
    }
}

impl Node {
    /// Build the swarm and start it on the current tokio runtime.
    ///
    /// The libp2p keypair is generated fresh every start and is *not* derived
    /// from the account key. A stable network identity would let every LAN and
    /// every DHT node this device ever touches link its sessions together; who
    /// we are is established later, inside the encrypted session.
    pub fn spawn(config: NodeConfig) -> Result<(NodeHandle, mpsc::Receiver<NetEvent>)> {
        let keypair = identity::Keypair::generate_ed25519();
        Self::spawn_with_keypair(config, keypair)
    }

    pub fn spawn_with_keypair(
        config: NodeConfig,
        keypair: identity::Keypair,
    ) -> Result<(NodeHandle, mpsc::Receiver<NetEvent>)> {
        let cfg = config.clone();
        let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_tcp(
                tcp::Config::default().nodelay(true),
                noise::Config::new,
                yamux::Config::default,
            )
            .map_err(|e| Error::Transport(e.to_string()))?
            .with_quic()
            .with_dns()
            .map_err(|e| Error::Transport(e.to_string()))?
            .with_relay_client(noise::Config::new, yamux::Config::default)
            .map_err(|e| Error::Transport(e.to_string()))?
            .with_behaviour(move |key, relay_client| {
                Vega::new(key, relay_client, &cfg)
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
            })
            .map_err(|e| Error::Transport(e.to_string()))?
            // Connections are kept alive well past the last stream: re-dialling
            // through a NAT is far more expensive than holding an idle socket.
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(120)))
            .build();

        for addr in &config.listen {
            if let Err(e) = swarm.listen_on(addr.clone()) {
                // One address failing (no IPv6 on this host, say) must not stop
                // the others from coming up.
                tracing::warn!(%addr, error = %e, "could not listen");
            }
        }

        for addr in &config.bootstrap {
            if let Some(peer) = peer_id_of(addr) {
                swarm.behaviour_mut().kad.add_address(&peer, addr.clone());
            }
            if let Err(e) = swarm.dial(addr.clone()) {
                tracing::warn!(%addr, error = %e, "could not dial bootstrap peer");
            }
        }
        if !config.bootstrap.is_empty() {
            let _ = swarm.behaviour_mut().kad.bootstrap();
        }

        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let (evt_tx, evt_rx) = mpsc::channel(256);

        let node = Node {
            swarm,
            config,
            mailbox: Mailbox::new(),
            commands: cmd_rx,
            events: evt_tx,
            pending: HashMap::new(),
            queries: HashMap::new(),
        };

        tokio::spawn(node.run());
        Ok((NodeHandle { tx: cmd_tx }, evt_rx))
    }

    async fn run(mut self) {
        let mut sweep = tokio::time::interval(Duration::from_secs(60));
        loop {
            tokio::select! {
                event = self.swarm.select_next_some() => self.on_swarm_event(event),
                command = self.commands.recv() => match command {
                    Some(Command::Shutdown) | None => break,
                    Some(cmd) => self.on_command(cmd),
                },
                _ = sweep.tick() => self.mailbox.expire(vega_core::now()),
            }
        }
        tracing::info!("network task stopped");
    }

    fn on_command(&mut self, cmd: Command) {
        match cmd {
            Command::Shutdown => {}

            Command::LocalPeerId { reply } => {
                let _ = reply.send(*self.swarm.local_peer_id());
            }

            Command::Listeners { reply } => {
                let _ = reply.send(self.swarm.listeners().cloned().collect());
            }

            Command::Dial { addr, reply } => {
                let result = self
                    .swarm
                    .dial(addr)
                    .map_err(|e| Error::Transport(e.to_string()));
                let _ = reply.send(result);
            }

            Command::Deliver {
                peer,
                envelope,
                reply,
            } => {
                if envelope.len() > MAX_ENVELOPE_BYTES {
                    let _ = reply.send(Err(Error::Protocol("envelope too large".into())));
                    return;
                }
                let id = self
                    .swarm
                    .behaviour_mut()
                    .messaging
                    .send_request(&peer, Request::Deliver { envelope });
                self.pending.insert(id, Pending::Deliver(reply));
            }

            Command::Park {
                peer,
                tag,
                token,
                envelope,
                expires_at,
                reply,
            } => {
                let id = self.swarm.behaviour_mut().messaging.send_request(
                    &peer,
                    Request::Park {
                        tag,
                        token,
                        envelope,
                        expires_at,
                    },
                );
                self.pending.insert(id, Pending::Park(reply));
            }

            Command::Collect {
                peer,
                claims,
                reply,
            } => {
                let id = self
                    .swarm
                    .behaviour_mut()
                    .messaging
                    .send_request(&peer, Request::Collect { claims });
                self.pending.insert(id, Pending::Collect(reply));
            }

            Command::Publish { key, value, reply } => {
                let record = kad::Record::new(kad::RecordKey::new(&key), value);
                match self
                    .swarm
                    .behaviour_mut()
                    .kad
                    .put_record(record, kad::Quorum::One)
                {
                    Ok(id) => {
                        self.queries.insert(id, Query::Put(reply));
                    }
                    Err(e) => {
                        let _ = reply.send(Err(Error::Protocol(e.to_string())));
                    }
                }
            }

            Command::Lookup { key, reply } => {
                let id = self
                    .swarm
                    .behaviour_mut()
                    .kad
                    .get_record(kad::RecordKey::new(&key));
                self.queries.insert(
                    id,
                    Query::Get {
                        reply,
                        found: Vec::new(),
                    },
                );
            }

            Command::ReserveRelay { relay, reply } => {
                // Listening on `<relay>/p2p-circuit` is how a reservation is
                // requested; the relay then advertises that address for us.
                let circuit = relay.with(Protocol::P2pCircuit);
                let result = self
                    .swarm
                    .listen_on(circuit)
                    .map(|_| ())
                    .map_err(|e| Error::Transport(e.to_string()));
                let _ = reply.send(result);
            }
        }
    }

    fn emit(&self, event: NetEvent) {
        // A full channel means the app is not draining events; dropping is
        // better than stalling the whole network task behind it.
        if self.events.try_send(event).is_err() {
            tracing::debug!("event channel full, dropping event");
        }
    }

    fn on_swarm_event(&mut self, event: SwarmEvent<VegaEvent>) {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                self.emit(NetEvent::Listening(address));
            }

            SwarmEvent::ExternalAddrConfirmed { address } => {
                self.emit(NetEvent::ExternalAddress(address));
            }

            SwarmEvent::Behaviour(VegaEvent::Mdns(mdns::Event::Discovered(peers))) => {
                let mut grouped: HashMap<PeerId, Vec<Multiaddr>> = HashMap::new();
                for (peer, addr) in peers {
                    self.swarm
                        .behaviour_mut()
                        .kad
                        .add_address(&peer, addr.clone());
                    grouped.entry(peer).or_default().push(addr);
                }
                for (peer, addrs) in grouped {
                    self.emit(NetEvent::PeerDiscovered {
                        peer,
                        addrs,
                        source: Source::Mdns,
                    });
                }
            }

            SwarmEvent::Behaviour(VegaEvent::Mdns(mdns::Event::Expired(peers))) => {
                for (peer, _) in peers {
                    self.emit(NetEvent::PeerExpired(peer));
                }
            }

            SwarmEvent::Behaviour(VegaEvent::Identify(libp2p::identify::Event::Received {
                peer_id,
                info,
                ..
            })) => {
                // Only feed the routing table peers that actually speak our
                // Kademlia protocol — otherwise the table fills with strangers.
                if info.protocols.contains(&crate::protocol::KAD_PROTOCOL) {
                    for addr in &info.listen_addrs {
                        self.swarm
                            .behaviour_mut()
                            .kad
                            .add_address(&peer_id, addr.clone());
                    }
                    self.emit(NetEvent::PeerDiscovered {
                        peer: peer_id,
                        addrs: info.listen_addrs,
                        source: Source::Identify,
                    });
                }
            }

            SwarmEvent::Behaviour(VegaEvent::RelayClient(
                relay::client::Event::ReservationReqAccepted { relay_peer_id, .. },
            )) => {
                self.emit(NetEvent::RelayReserved(relay_peer_id));
            }

            SwarmEvent::Behaviour(VegaEvent::Dcutr(libp2p::dcutr::Event {
                remote_peer_id,
                result,
            })) => match result {
                Ok(_) => self.emit(NetEvent::HolePunched(remote_peer_id)),
                Err(e) => {
                    // Expected on symmetric NAT and most carrier networks; the
                    // relayed path stays up, so this is information, not failure.
                    tracing::debug!(peer = %remote_peer_id, error = %e, "hole punch failed, staying relayed");
                }
            },

            SwarmEvent::Behaviour(VegaEvent::Messaging(
                libp2p::request_response::Event::Message { peer, message, .. },
            )) => self.on_message(peer, message),

            SwarmEvent::Behaviour(VegaEvent::Messaging(
                libp2p::request_response::Event::OutboundFailure {
                    request_id, error, ..
                },
            )) => {
                self.fail_pending(request_id, Error::Transport(error.to_string()));
            }

            SwarmEvent::Behaviour(VegaEvent::Kad(kad::Event::OutboundQueryProgressed {
                id,
                result,
                step,
                ..
            })) => self.on_kad_result(id, result, step.last),

            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                self.emit(NetEvent::PeerConnected(peer_id));
            }

            SwarmEvent::ConnectionClosed {
                peer_id,
                num_established,
                ..
            } => {
                // Only report the peer as gone when the *last* connection to it
                // closes; a multiaddr swap tears one down while another lives.
                if num_established == 0 {
                    self.emit(NetEvent::PeerDisconnected(peer_id));
                }
            }

            SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                tracing::debug!(?peer_id, %error, "dial failed");
            }

            _ => {}
        }
    }

    fn on_message(&mut self, peer: PeerId, message: RrMessage<Request, Response>) {
        match message {
            RrMessage::Request {
                request, channel, ..
            } => self.on_request(peer, request, channel),

            RrMessage::Response {
                request_id,
                response,
            } => self.resolve_pending(request_id, response),
        }
    }

    fn on_request(&mut self, peer: PeerId, request: Request, channel: ResponseChannel<Response>) {
        let response = match request {
            Request::Deliver { envelope } => {
                if envelope.len() > MAX_ENVELOPE_BYTES {
                    Response::refused("envelope too large")
                } else {
                    self.emit(NetEvent::Envelope {
                        from: peer,
                        bytes: envelope,
                    });
                    Response::Accepted
                }
            }

            Request::Park {
                tag,
                token,
                envelope,
                expires_at,
            } => {
                if !self.config.act_as_mailbox {
                    Response::refused("this node does not hold mail")
                } else {
                    match self.mailbox.park(
                        tag,
                        token,
                        envelope,
                        expires_at,
                        vega_core::now(),
                        MAX_ENVELOPE_BYTES,
                    ) {
                        ParkOutcome::Accepted => Response::Accepted,
                        ParkOutcome::Full => Response::refused("mailbox full"),
                        ParkOutcome::TooLarge => Response::refused("envelope too large"),
                        ParkOutcome::BadExpiry => Response::refused("unacceptable expiry"),
                    }
                }
            }

            Request::Collect { claims } => {
                if !self.config.act_as_mailbox {
                    Response::Collected { envelopes: vec![] }
                } else {
                    Response::Collected {
                        envelopes: self.mailbox.collect(&claims, vega_core::now()),
                    }
                }
            }
        };

        let _ = self
            .swarm
            .behaviour_mut()
            .messaging
            .send_response(channel, response);
    }

    fn resolve_pending(&mut self, id: OutboundRequestId, response: Response) {
        let Some(pending) = self.pending.remove(&id) else {
            return;
        };
        match (pending, response) {
            (Pending::Deliver(reply), Response::Accepted) => {
                let _ = reply.send(Ok(()));
            }
            (Pending::Park(reply), Response::Accepted) => {
                let _ = reply.send(Ok(()));
            }
            (Pending::Collect(reply), Response::Collected { envelopes }) => {
                let _ = reply.send(Ok(envelopes));
            }
            (Pending::Deliver(reply) | Pending::Park(reply), Response::Refused { reason }) => {
                let _ = reply.send(Err(Error::Refused("peer".into(), reason)));
            }
            (Pending::Collect(reply), Response::Refused { reason }) => {
                let _ = reply.send(Err(Error::Refused("peer".into(), reason)));
            }
            (pending, other) => {
                let e = Error::Protocol(format!("unexpected response: {other:?}"));
                match pending {
                    Pending::Deliver(r) | Pending::Park(r) => {
                        let _ = r.send(Err(e));
                    }
                    Pending::Collect(r) => {
                        let _ = r.send(Err(e));
                    }
                }
            }
        }
    }

    fn fail_pending(&mut self, id: OutboundRequestId, error: Error) {
        match self.pending.remove(&id) {
            Some(Pending::Deliver(r)) | Some(Pending::Park(r)) => {
                let _ = r.send(Err(error));
            }
            Some(Pending::Collect(r)) => {
                let _ = r.send(Err(error));
            }
            None => {}
        }
    }

    fn on_kad_result(&mut self, id: QueryId, result: kad::QueryResult, last: bool) {
        match result {
            kad::QueryResult::PutRecord(res) => {
                if let Some(Query::Put(reply)) = self.queries.remove(&id) {
                    let _ = reply.send(
                        res.map(|_| ())
                            .map_err(|e| Error::Protocol(format!("{e:?}"))),
                    );
                }
            }

            kad::QueryResult::GetRecord(Ok(kad::GetRecordOk::FoundRecord(peer_record))) => {
                if let Some(Query::Get { found, .. }) = self.queries.get_mut(&id) {
                    found.push(peer_record.record.value);
                }
                if last {
                    self.finish_get(id);
                }
            }

            kad::QueryResult::GetRecord(Ok(kad::GetRecordOk::FinishedWithNoAdditionalRecord {
                ..
            })) => self.finish_get(id),

            kad::QueryResult::GetRecord(Err(_)) => {
                if let Some(Query::Get { reply, found }) = self.queries.remove(&id) {
                    // A "failed" query that still returned records is a success:
                    // the DHT could not reach quorum, but we have what we need.
                    let _ = reply.send(if found.is_empty() {
                        Err(Error::NotFound)
                    } else {
                        Ok(found)
                    });
                }
            }

            _ => {}
        }
    }

    fn finish_get(&mut self, id: QueryId) {
        if let Some(Query::Get { reply, found }) = self.queries.remove(&id) {
            let _ = reply.send(if found.is_empty() {
                Err(Error::NotFound)
            } else {
                Ok(found)
            });
        }
    }
}

/// Append `/p2p/<id>` to an address, which is what makes it dialable as a
/// specific peer rather than as whoever happens to answer on that port.
pub fn with_peer(addr: Multiaddr, peer: PeerId) -> Multiaddr {
    if peer_id_of(&addr).is_some() {
        addr
    } else {
        addr.with(Protocol::P2p(peer))
    }
}

/// Pull the `/p2p/<id>` component out of a multiaddr, if it has one.
pub fn peer_id_of(addr: &Multiaddr) -> Option<PeerId> {
    addr.iter().find_map(|p| match p {
        Protocol::P2p(id) => Some(id),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_id_is_extracted_from_a_bootstrap_address() {
        let peer = PeerId::random();
        let addr: Multiaddr = format!("/ip4/1.2.3.4/udp/4001/quic-v1/p2p/{peer}")
            .parse()
            .unwrap();
        assert_eq!(peer_id_of(&addr), Some(peer));
    }

    #[test]
    fn with_peer_is_idempotent() {
        let peer = PeerId::random();
        let bare: Multiaddr = "/ip4/1.2.3.4/udp/4001/quic-v1".parse().unwrap();
        let once = with_peer(bare, peer);
        assert_eq!(with_peer(once.clone(), peer), once);
        assert_eq!(peer_id_of(&once), Some(peer));
    }

    #[test]
    fn an_address_without_a_peer_id_yields_none() {
        let addr: Multiaddr = "/ip4/1.2.3.4/udp/4001/quic-v1".parse().unwrap();
        assert_eq!(peer_id_of(&addr), None);
    }
}
