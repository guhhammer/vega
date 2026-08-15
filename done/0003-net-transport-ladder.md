# 0003 — The transport ladder

`feat(net): the transport ladder over libp2p`

`vega-net`. Moves opaque envelopes it has no way to open. 21 unit tests plus 5
integration tests that spawn real nodes on real sockets.

## The ladder

For each contact, a set of scored candidate paths, raced concurrently. The first
to complete a handshake wins. Cheapest and most private first, because every
step down adds a party who learns something about the conversation.

| Tier | Mechanism | Built from |
|---|---|---|
| T0 | same LAN | `mdns` + `quic` |
| T1 | direct across the internet | IPv6, `upnp`, `autonat`, `dcutr` |
| T2 | rendezvous | `kad` on `/vega/kad/1.0.0` |
| T3 | relayed | `relay` (Circuit Relay v2) |
| T4 | recipient offline | mailbox over `/vega/msg/1.0.0` |

**IPv6 is listed first in the default config on purpose.** Many mobile carriers
hand out a globally routable v6 address, which sidesteps NAT entirely. It is the
cheapest win on the whole ladder and routinely overlooked.

## A private DHT, not a corner of the public one

Kademlia runs under `/vega/kad/1.0.0`. A node that does not speak it is never
routed to, so we inherit neither IPFS's record pressure nor its observability.
The cost is a bootstrap seed list; anyone can run a seed and none of them can
read anything.

Record TTL matches the rendezvous epoch, so stale addresses age out on their own
rather than being chased.

Only peers that actually speak our Kademlia protocol are added to the routing
table — otherwise it fills with strangers who happen to have connected.

## The libp2p PeerId is not the account identity

It is a fresh keypair every start. A stable network identity would let every LAN
and every DHT node this device ever touches link its sessions together. Who we
are is established afterwards, inside the encrypted session.

This is the same mistake the prototype made by broadcasting a stable instance
name to the whole LAN, just at internet scale.

## Desktop and mobile are not symmetric

`NodeConfig::default()` describes a desktop: reachable, willing to relay, willing
to hold mail. `NodeConfig::mobile()` describes a phone: a leaf node that takes
from the network without carrying anyone else's traffic, with UPnP off because
carrier NAT ignores it and it costs battery.

That asymmetry is deliberate and explicit rather than emergent. A phone should
never relay a stranger's traffic on a metered connection.

## Mailboxes

An open "store this for me" endpoint is a free disk-filling service, so every
limit exists to make abuse boring rather than impossible: a hard cap per tag, a
hard cap overall, a maximum lifetime, and eviction of the oldest entry when a
tag fills. A mailbox peer cannot tell who sent an envelope, who it is for, or
what it says — which is also why it cannot filter on any of that.

Held in memory on purpose. Mail that does not survive a restart is a weaker
promise, but it also means a relay never accumulates other people's ciphertext
on disk. Persisting it is a decision to make deliberately, not a default.

Collection requires a token derived from the pairwise secret — see 0005.

## The swarm runs on one task

`Swarm` is neither `Sync` nor cheap to move, so it lives on a single task and
callers talk to it over channels. That keeps it off the UI thread and out of the
app's type signatures entirely.

One subtlety cost a compile cycle: the event emitter held `&self` across an
await while never actually awaiting, which made the whole future non-`Send`
because `Swarm` is not `Sync`. Making that path synchronous fixed it.

Events are dropped rather than blocking when the channel is full. A stalled
consumer must not stall the network task.

## Size limits are explicit

The request/response codec's limits are set rather than left to the default.
This is the one place a stranger's bytes reach our allocator before we have
decided we want them.

## What the integration tests prove

Not that the code compiles — that it works over sockets:

- an envelope crosses the wire intact;
- two accounts exchange an encrypted message and **the account id never appears
  in the wire bytes**;
- a third party on the same network can neither decrypt nor recognise the tag;
- parked mail is collected exactly once, and not by someone holding only the tag;
- a rendezvous record round-trips through the DHT and is unreadable without the
  pairwise key.

The park/collect test earned its place immediately: it caught a hand-written tag
serializer that wrote CBOR byte strings and read back arrays, so every park and
collect failed over the wire. The unit tests could not see it because they never
crossed a codec. Deleted in favour of serde's own `[u8; 16]` impls.
