# Vega — Protocol Design

An encrypted messenger with no server: peers find each other, carry each other's
ciphertext, and hold each other's mail.

| | |
|---|---|
| **Status** | Draft, written before implementation |
| **Date** | 2026-08-15 |
| **Supersedes** | the `z_tests` prototype, now `.references/proto/` |
| **Stack** | Rust · libp2p · Tauri v2 |

> This is the original design draft, kept as written. It is the reasoning the
> code was built from, not a description of the code as it stands — for that,
> see the [README](../README.md) and [`done/`](../done/). Where the two differ,
> the code is right and [`done/0005-security-review.md`](../done/0005-security-review.md)
> explains what changed and why.
>
> Also published as an [artifact](https://claude.ai/code/artifact/55208dd3-3049-42ef-a923-da62b1a9f61a).

---

## 00 · What "serverless" can actually mean

Two phones behind carrier NAT cannot find each other with zero prior
infrastructure. That isn't a matter of effort — there is no way to learn a peer's
current IP and port without something telling you. Any design claiming otherwise
is hiding a server somewhere.

So the goal needs a sharper definition. Vega is serverless in this sense:

- **No service you operate.** Nothing to pay for, nothing to keep running.
- **No node that can read messages.** Every intermediary handles ciphertext it
  cannot open.
- **No node whose disappearance kills the network.** Every role is played by
  ordinary peers, and any peer can play it.

That is achievable. The work is in replacing each thing a server used to do with
something a swarm of peers does instead.

| The server used to | Replacement | What it costs |
|---|---|---|
| Hold public keys | Self-certifying identity — the account ID *is* the hash of the public key, so there is nothing to look up | Contact exchange must happen out of band (QR, link, spoken code) |
| Tell you where a peer is | Kademlia DHT, with lookup keys derived from a pairwise secret | Needs bootstrap seeds to join the network the first time |
| Forward packets past NAT | Circuit Relay v2 — any reachable peer relays opaque bytes | Someone reachable must be online; relays see timing and volume |
| Hold mail while you're offline | Encrypted, TTL-bounded blobs parked with peers near your rotating tag | Weaker delivery guarantee, and a spam surface to defend |

---

## 01 · What survives the prototype

The `z_tests` layering was right — discovery, transport, crypto and
interpretation were already separate concerns. The structure carries over. Three
things must not.

| Prototype | Problem | Replacement |
|---|---|---|
| `z_server` generates keypairs and serves `GET /get_priv_key` | Custodial. That server can read every message ever sent. | Keys are generated on-device and never leave it |
| `crypt.rs` uses the raw X25519 DH output directly as the AEAD key | No KDF, static keys — one key per contact pair, forever. One stolen device decrypts all history. | Noise handshake per connection, Double Ratchet per message |
| `#BROADCAST\|INSTANCE:BOLD_TOKYO_a1b2` on the LAN | A stable identifier announced to every device on the network, forever | Rotating per-epoch tag only your contacts can recognise |
| Blocking HTTP fetch inside `update_peers` | Fires on every broadcast packet received — a remote stall and an amplification target | No synchronous network I/O on the packet path |
| No replay defence | A captured ciphertext can be re-delivered indefinitely | Ratchet counters plus a persisted dedupe window |

---

## 02 · Identity without a registry

An account is an Ed25519 keypair generated on the device. The **account ID is
the hash of its public key**. There is no registration step, no namespace to
contend over, and no lookup that can be denied or poisoned — if you can name
someone, you already hold what you need to verify them.

Each device gets its own keypair. Devices are authorised by entries in a
hash-linked, append-only log — a sigchain — where every entry is signed by a key
already trusted in the chain:

```
# entry N
{
  prev:    blake3(entry N-1),
  seq:     N,
  ts:      1755273600,
  body:    AddDevice { device_pk, prekey_bundle, label: "pixel-8" },
  sig:     Ed25519(account_root_pk | device_pk_already_in_chain)
}
```

Anyone holding your account ID can validate that chain **entirely offline** and
derive the answer to "which devices legitimately belong to this account?" — the
exact question a server used to answer. Revocation is another entry. Prekey
publication is another entry.

### Linking a second device

SPAKE2 over a six-word code or a QR scan. The same protocol runs whether the two
devices are on one table or on different continents — a short low-entropy code
is safe because PAKE gives one guess per attempt, not an offline dictionary
attack.

### Adding a contact

Out of band: QR, deep link, NFC. This is the trust root and it is deliberately
manual. Safety-number comparison afterwards, the same as Signal, to catch a
substituted key.

> **Design decision.** The libp2p `PeerId` is **not** the account identity. It is
> a per-epoch throwaway keypair. Anyone watching the LAN or the DHT sees a
> meaningless rotating network address; who you actually are is established
> afterwards, inside the encrypted session. Binding those two identities together
> would leak your identity to every network you ever join.

---

## 03 · Two layers of encryption, and why both

libp2p already encrypts every connection with Noise. That is *hop* encryption —
it protects the link between two directly connected peers. It is not sufficient
here, because Tiers 3 and 4 route your data through peers you have no reason to
trust.

```
        ┌──────────────── Olm session · end-to-end ────────────────┐
        │                                                          │
   ┌────┴─────┐   Noise hop    ┌────────────┐   Noise hop   ┌──────┴───┐
   │ Device A │ ─────────────► │ Relay peer │ ────────────► │ Device B │
   │  sender  │                │ a stranger │               │recipient │
   └──────────┘                └─────┬──────┘               └──────────┘
                                     │
        relay learns: 2 routing tags, byte count, timing
        relay never learns: content, sender, recipient
```

Hop encryption is unwrapped and rewrapped at the relay; the end-to-end session is
not. This is why a stranger can safely carry your traffic — and exactly what the
stranger still observes.

### The message layer

Use [vodozemac](https://github.com/matrix-org/vodozemac) rather than writing a
ratchet. It is a pure-Rust implementation of Olm (Double Ratchet, 1:1) and Megolm
(group ratchet), Apache-2.0, audited by Least Authority, and running in
production in every Element client. It maps onto this design almost exactly,
because Matrix solved the same shape of problem: per-device sessions,
multi-device fan-out, group ratcheting.

The alternative — libsignal — is AGPL-3.0, which is a licensing decision, not
just a technical one. Hand-rolling a Double Ratchet is the option worth avoiding
outright.

| Concern | Mechanism |
|---|---|
| Transport confidentiality | libp2p Noise (XX), per connection, per hop |
| End-to-end confidentiality | vodozemac Olm session, per recipient *device* |
| Forward secrecy | Double Ratchet — a stolen device does not decrypt history |
| Post-compromise security | Ratchet self-heals after key compromise once a fresh DH lands |
| Groups | Pairwise fan-out — one sealed message per member device, over the same Olm ratchet |
| Replay | Ratchet message index + persisted seen-set per session |
| Sender anonymity to relays | Sealed sender — the outer envelope carries only a routing tag |

---

## 04 · The transport ladder

This is the core of the system. For each contact, Vega maintains a set of scored
candidate paths and races the viable ones concurrently — the first to complete a
handshake wins, and the rest are either torn down or kept warm as fallback.
Message delivery is decoupled from connection state through a persistent outbox
with per-message acknowledgement, which is what makes this survivable on mobile,
where paths die constantly.

```
DEVICE A                                                      DEVICE B
   │                                                             │
   ├── T0 · same LAN — mDNS + QUIC ─────────────────────────────►│
   │                                                             │
   ├── T1 · direct — IPv6, port map, hole punch ────────────────►│
   │            ◇ NAT ◇                                          │
   │                                                             │
   ├╌╌ T2 · rendezvous — Kademlia lookup ╌╌[ DHT swarm ]╌╌╌╌╌╌╌╌►│
   │            returns an address — carries no message          │
   │                                                             │
   ├── T3 · relayed — Circuit Relay v2 ────[ relay peer ]───────►│
   │                                                             │
   ├── T4 · B is offline — parked mail ────[mailbox peer]╌╌╌╌╌╌╌►│
   │                                            when B returns   │
   │                                                             │
   ├┈┈ T5 · no internet — BLE / Wi-Fi Direct ┈┈┈┈┈┈┈┈┈┈┈┈┈┈┈┈┈┈►│
   │                                                             │
   ▼ cost and exposure increase downward                         ▼

   ──  solid  · carries encrypted message
   ╌╌  dashed · control, lookup or deferred
```

Tiers are attempted concurrently, not in sequence — but a lower tier always wins
when available, because each step down the ladder adds a party who learns
something about the conversation.

### What each tier is, and what builds it

| Tier | Mechanism | libp2p component | Notes |
|---|---|---|---|
| **T0** | Same subnet — mDNS/DNS-SD service discovery, then QUIC | `libp2p-mdns` + `libp2p-quic` | Announce a rotating tag, not a stable name. Replaces the prototype's UDP broadcast. |
| **T1** | Direct across the internet: IPv6 first, then UPnP/NAT-PMP/PCP port mapping, then UDP hole punching | `libp2p-autonat`, `libp2p-upnp`, `libp2p-dcutr` | **IPv6 is the most underrated win here** — many mobile carriers hand out a globally routable address, meaning no NAT at all. |
| **T2** | Kademlia lookup to learn where a contact currently is | `libp2p-kad` on a private protocol id | Control plane only. See §05 for why the lookup key must be a secret. |
| **T3** | A reachable peer forwards opaque ciphertext both ways | `libp2p-relay` (Circuit Relay v2) | This is the tier that makes 4G work at all. Carrier NAT usually defeats hole punching. |
| **T4** | Recipient offline — encrypted blob parked with peers, collected later | Custom `request_response` protocol | Nothing off the shelf. See §06. |
| **T5** | No internet at all — Bluetooth LE, Wi-Fi Direct, Wi-Fi Aware | Custom transport + native plugins | Phase 3. Needs Kotlin/Swift work outside anything Tauri exposes. |

> **On the "own DHT" choice.** Running libp2p Kademlia under a private protocol
> id — `/vega/kad/1.0.0` — means Vega nodes form their own network rather than
> joining the public IPFS DHT. You get Kademlia's routing without inheriting a
> stranger's network health, its record pressure, or its observability. The cost
> is that you must ship a bootstrap seed list, and the network is only as
> reachable as those seeds plus the peers each client has cached. Anyone can run
> a seed; none of them can read anything.

---

## 05 · Rendezvous that doesn't leak the social graph

A naive DHT design publishes "account X is at address Y". That turns the DHT into
a scrapable map of who exists and, over time, who talks to whom. Traffic content
stays encrypted while the entire social graph walks out the front door.

Instead, the lookup key is derived from a secret only the two of you share:

```
key   = HKDF(pairwise_secret, "vega-rv" || floor(now / 1h))
value = Seal(pairwise_secret, {
          addrs:     [/ip6/…/quic-v1, /ip4/…/p2p-circuit],
          peer_id:   <ephemeral, this epoch only>,
          expires:   <=1h
        })
```

Only a contact can compute the key, so the record is unfindable by anyone else —
it looks like random bytes at a random location. Rotating hourly bounds how long
a compromised key stays useful and stops long-term correlation of one identity
across epochs.

You publish under one key per contact. Costs scale with contact count, which is
fine for a messenger and would not be for a broadcast network.

---

## 06 · Mailboxes: reaching someone who is offline

Without this, "serverless" degrades into "you must both be online at the same
instant", which is not a messenger anyone will use. This is also the part with no
off-the-shelf answer, so it deserves the most design attention.

**Shape.** A sender picks the *k* peers whose IDs sit closest in Kademlia
keyspace to the recipient's current rotating tag, and offers each an encrypted
blob over `/vega/mailbox/1.0.0`. When the recipient comes online, they query the
same neighbourhood and collect anything addressed to their tag. Blobs are opaque;
a mailbox peer cannot tell who sent one, who it is for, or what it says.

**The hard parts.**

- **Spam.** An open "store this for me" endpoint is a free disk-filling service.
  Defences: hard size cap, short TTL, per-peer quota, and admission proof — a
  small proof-of-work, or restricting storage to peers you already have a
  relationship with.
- **Churn.** The *k* closest peers change as nodes come and go. Over-replicate
  and accept that some blobs are lost — the outbox retries anyway.
- **Deniability.** A mailbox peer must be able to say truthfully that it has no
  idea what it is holding. Uniform blob sizes with padding, so size doesn't
  fingerprint content.
- **Acknowledgement.** The recipient signals collection so the blob can be
  dropped early rather than waiting out its TTL.

A useful fallback exists for the trust-restricted case: your own other devices,
and mutual contacts, can hold mail for you. Briar takes this approach and it is a
much smaller problem — but it only works between people who already know each
other.

---

## 07 · Unifying a person across devices

This is the requirement that quietly shapes everything above. A server-based
messenger unifies devices by making the server the authority. With no server,
three mechanisms carry it:

1. **The sigchain** is the device roster. It is signed, offline-verifiable, and
   every contact independently derives the same answer about which devices are
   yours.
2. **Fan-out on send.** A message to a contact is encrypted separately for each
   of that contact's devices, *and* for each of your own — that copy to yourself
   is how your desktop learns what your phone just sent.
3. **Convergent history.** Each device keeps a local log; messages carry a
   sender-assigned sequence plus a wall-clock hint. Ordering is per-conversation
   and last-writer-wins on conflict. Trying for stronger consistency without a
   server is not worth the complexity for a chat log.

A device added today cannot decrypt history that predates it — the ratchet
guarantees that. Backfill is therefore an explicit transfer from an existing
device, encrypted device-to-device, and it should be a deliberate user action
rather than something that happens silently.

---

## 08 · Platform reality

Desktop being easier than mobile understates it. This is the single constraint
that should shape the roadmap.

| Platform | Background delivery | What it takes |
|---|---|---|
| **Desktop** (Linux · macOS · Windows) | Full — sockets stay open, can act as relay and mailbox | Nothing special. Desktop peers are the backbone the mobile peers lean on. |
| **Android** | Workable — foreground service with a persistent notification | Custom Tauri plugin for the service. Doze throttles wakeups. mDNS needs a `MulticastLock`, also via plugin. |
| **iOS** | **Effectively none** — the app suspends within seconds | Messages arrive when the app is open. The industry answer is APNs, which reintroduces a server. |

> **The iOS problem, stated plainly.** iOS does not permit a background process
> to hold a listening socket. This is why Briar is Android-only. The only ways
> out are a push relay — a server that sees "something arrived for tag X" but no
> content — or accepting foreground-only delivery. There is no third option, and
> it is worth deciding which compromise you prefer before writing iOS code rather
> than after. Separately, mDNS on iOS requires a multicast entitlement that must
> be requested from Apple with written justification.

Given the Desktop + Android choice, the architecture should assume **desktop
peers do the heavy lifting**: they are the reachable ones, so they relay and hold
mail, while phones are mostly leaf nodes that dip in and out. That asymmetry
should be explicit in the design rather than emergent — a phone should never be
asked to relay someone else's traffic on a metered connection.

### Tauri specifics

- Tauri v2 supports iOS and Android; Rust cross-compiles to
  `aarch64-linux-android`. The libp2p stack itself runs unmodified.
- The frontend can stay close to what the prototype already has — React 19,
  Tailwind, shadcn. That part is not the risk.
- Everything platform-native — foreground service, multicast lock, BLE, Wi-Fi
  Direct, battery exemptions — needs custom Tauri plugins in Kotlin. Budget for
  this; it is not incidental work.
- Keys go in the platform keystore, not a file: Android Keystore, and Secret
  Service / Keychain / DPAPI on desktop.

---

## 09 · What this does not protect against

Stating the limits precisely is what separates a security design from a marketing
claim.

- **Global passive observation.** Someone watching both ends of the network
  correlates timing and volume. Vega is not Tor and should not imply it is. Onion
  routing over the relay tier is a possible later addition, at a real latency
  cost.
- **Your relay knows you are online.** Relay and mailbox peers learn tags, sizes
  and timings. Content and identity stay hidden; participation does not.
- **Endpoint compromise.** A malicious OS or a rooted device reads the plaintext
  before it is ever encrypted. No protocol fixes this.
- **Contact exchange.** If someone substitutes a key during the out-of-band
  exchange, they are in the conversation. This is why safety-number verification
  is not optional decoration.
- **Bootstrap seeds are a censorship point.** They cannot read anything, but
  blocking all of them stops new nodes joining. Mitigations: many seeds, cached
  peers, LAN discovery, and invites that carry an address directly.
- **Availability.** With no server, if nobody reachable is online, the message
  waits. That is a genuine downgrade from centralised messengers, honestly
  acknowledged.

---

## 10 · Build order

Each phase ends with something demonstrable. Nothing is built on an unproven
layer.

| Phase | What | Proves |
|---|---|---|
| **P1** | **Identity and crypto, no network.** Sigchain, device keys, vodozemac sessions, encrypt/decrypt/ratchet against a local test harness. | Two simulated devices exchange forward-secret messages, and a third device is added and verified offline. |
| **P2** | **T0 — LAN, desktop only.** libp2p swarm with mDNS + QUIC + Noise. Rotating discovery tags. Outbox with retry and acknowledgement. | Two laptops on one wifi network chat with no configuration and no server. This is the prototype's capability, rebuilt correctly. |
| **P3** | **T2 + T1 — DHT and direct connection.** Private Kademlia network, secret-derived rendezvous records, AutoNAT, UPnP, DCUtR. Run the first seed nodes. | Two machines on different networks, different cities, connect directly. |
| **P4** | **T3 — relay.** Circuit Relay v2, reservation management, relay selection and scoring, opt-in relaying with a bandwidth budget. | A phone on 4G behind carrier NAT reaches a peer it cannot hole-punch to. |
| **P5** | **Android.** Tauri mobile build, foreground service plugin, multicast lock, battery behaviour, mobile-shaped UI. | A message sent while the phone is in a pocket arrives. |
| **P6** | **T4 — mailboxes.** Store-and-forward protocol, admission control, replication, TTL, collection and acknowledgement. | A message sent to a device that is switched off arrives when it comes back. |
| **P7** | **Groups, then T5.** Groups are done, by pairwise fan-out — see the note on Megolm below. T5 offline mesh is not started and needs a custom transport plus a native plugin per platform. | The design holds beyond one-to-one. |

P1 through P3 are where the risk actually lives. If P3 works reliably, the rest is
engineering rather than uncertainty.

---

## 11 · Open questions

1. **Who runs the seeds, and how many ship in the binary?** This is the only piece
   of the design that is even slightly centralised, and it deserves a deliberate
   answer rather than a default one.
2. **iOS: push relay, or foreground-only?** Deferred by the platform choice, but
   it will come back. Deciding early changes whether the envelope format needs a
   push-notification tag.
3. **Mailbox admission control.** Proof-of-work is unfriendly to phone batteries;
   contact-scoped storage is friendlier but far more limited. This needs a
   decision before P6.
4. **Does an account survive losing every device?** If yes, that implies a
   recovery phrase, and a recovery phrase is an attack surface. Signal chose no.
   Worth choosing consciously.
5. ~~**Groups: Megolm or MLS?**~~ **Settled: neither, for now.** Both assume a
   delivery service that accepts one ciphertext and fans it out. Vega has none:
   every envelope is sealed to one device and routed on a *pairwise* tag, so a
   group message is N envelopes whatever is inside them. Megolm would have
   changed the payload without changing the count, at the cost of session-key
   distribution, rotation on membership change, and weaker forward secrecy than
   the Olm ratchet already gives. So groups fan out pairwise. Megolm becomes
   worth its keep only alongside a **group-addressed envelope** — one shared
   routing tag and seal key for the whole group — which is a genuine N× saving
   and a genuine metadata cost, since a relay could then link every member's
   traffic by that shared tag. That is the decision to revisit, not the cipher.
6. **The `include_str!` per-company idea from the prototype notes.** Compiling a
   config into each build is a real distribution property, but it is not a
   security boundary — anyone can extract a constant from a binary. Worth
   revisiting explicitly as a build-and-distribution decision rather than a
   security one.

---

*Draft · for discussion before implementation*
