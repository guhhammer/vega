# Roadmap

The phases come from [the design](.documentation/design.md#10--build-order).
Each ends with something demonstrable, and nothing is built on an unproven layer.

Marked against what actually exists, not what is planned.

## P1 — Identity and crypto, no network ✅

Sigchain, device keys, vodozemac sessions, sealed sender, local storage.

**Proves:** two simulated devices exchange forward-secret messages, and a third
device is added and verified offline. 59 tests.

## P2 — T0, LAN, desktop ✅

libp2p swarm with mDNS, QUIC and Noise. Rotating discovery tags. Outbox with
retry.

**Proves:** two machines on one network exchange an encrypted message over real
sockets, and the account id never appears in the wire bytes.

## P3 — T2 + T1, DHT and direct connection ✅ / ⏳

Private Kademlia network, secret-derived rendezvous records, AutoNAT, UPnP,
DCUtR are implemented and the rendezvous round-trip is tested over real sockets.

**Still unproven:** two machines on two genuinely different networks connecting
directly. That needs hardware in two places and cannot be faked on loopback. It
is the single most valuable thing anyone could contribute right now.

## P4 — T3, relay ⏳

Circuit Relay v2 is wired and a client asks each configured seed for a
reservation. Relay selection is first-come rather than scored, and there is no
bandwidth budget for peers that agree to relay.

**Needs:** a publicly reachable seed, and a client behind carrier NAT.

## P5 — Android ⏳

The Rust cross-compiles; the targets are installed. What is missing is native:
a foreground service, a multicast lock, and battery behaviour that does not lie
to the user about when messages arrive.

See [`.documentation/android.md`](.documentation/android.md), which says exactly
what each plugin has to do.

## P6 — T4, mailboxes ✅ / ⏳

Park and collect work over the wire, with per-tag and node-wide caps, TTLs, and
a collection token so that seeing a routing tag is not enough to take someone's
mail.

**Still open:** admission control. Proof-of-work is unfriendly to phone
batteries; contact-scoped storage is friendlier but far more limited. The
mailbox is also in memory, so mail does not survive a restart — deliberate for
now, since it means a relay never accumulates other people's ciphertext on disk.

## P7 — Groups, then T5 ❌

Not started. Megolm pairs naturally with the vodozemac choice; MLS (openmls) is
the modern answer with better properties at larger sizes. The decision only
matters once one-to-one is solid.

T5 — Bluetooth LE and Wi-Fi Direct, for when there is no internet at all — is
the most interesting tier and the least urgent.

---

## Outside the phases

Things that are missing and do not belong to a phase.

| | Status | Note |
|---|---|---|
| **Device linking** | ❌ | The crypto exists — `Identity::adopt`, and the sigchain accepts device-signed additions. There is no pairing flow, so an account is one device today. SPAKE2 over a six-word code is the plan. |
| **Delivery receipts** | ⏳ | `Body::Receipt` is handled on arrival but nothing sends one, so the outbox clears on network acceptance rather than on the recipient reading. |
| **Platform keystore** | ⏳ | The database key is a 0600 file. The interface is two functions wide so it can be swapped. |
| **Message history index** | ⏳ | `conversation()` scans every stored message. Fine now, not fine with a real history. |
| **iOS** | ❌ | Blocked on a decision, not on work. See the platform section of the design. |
| **Audit** | ❌ | vodozemac has been audited. Nothing else here has. |

## What would help most

In order:

1. **Two machines on two real networks.** P3 and P4 cannot be proven any other
   way, and everything above them assumes they work.
2. **A seed node somebody actually runs.** `./make node`. It makes the network
   joinable and costs a port.
3. **The Android foreground service plugin.** It is the difference between a
   messenger and a demo on the platform most people would use it on.
4. **Adversarial review of `crates/vega-core`.** The last pass found a sender
   impersonation break that all the unit tests missed. There is likely more.
