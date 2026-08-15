# Architecture

How the code is arranged, and which boundaries matter. The *why* behind the
system is in [design.md](design.md); this is the shape it actually took.

## Three crates, one application

```
crates/vega-core     identity, sigchain, message crypto      no network, no async
crates/vega-net      the transport ladder                    cannot read a message
app/src-tauri        the shell: commands, runtime, keys      knows both
app/src              the interface
```

### The boundary that is load-bearing

`vega-net` handles envelopes it has no way to open. That is not tidiness — it is
the entire argument for asking a stranger's node to relay your traffic. If the
two crates ever merge, the argument quietly stops holding and nothing fails to
compile to say so.

A practical consequence: `vega-core` has no `tokio`, no sockets, and no reason
to grow either. Its tests run in milliseconds because there is nothing to wait
for.

### The boundary that is convenience

`app/src-tauri` versus `app/src` is an ordinary frontend/backend split. Every
command is a thin translation: parse the argument, take the lock, call into
`Runtime`, convert the error to a string. Nothing decides anything there.

## Data flow, sending

```
  UI          send_message(to, text)
   │
   ▼
  Runtime     load contact + their verified chain
   │          derive pairwise secret
   │          build Content { text, seq, sender_chain? }
   │          store the message locally  ← before any network call
   ▼
  core        fan_out: one ciphertext per recipient device,
   │                   plus one per my other devices
   │          Olm encrypt → Inner → seal → Envelope
   ▼
  Runtime     queue each envelope in the outbox
   │
   ▼          (lock released here)
  app         flush: offer each envelope to every reachable peer
   │
   ▼
  net         deliver over whichever tier is up
```

Two ordering decisions in there are deliberate:

- **The message is stored before anything touches the network.** A message is
  never lost because delivery happened to fail.
- **The lock is released before delivery.** `Runtime` performs no I/O at all; it
  returns plans and the caller executes them. A peer that takes the full request
  timeout to answer delays one delivery, not the UI and not incoming messages.

## Data flow, receiving

```
  net         an envelope arrives from some peer
   │
   ▼
  Runtime     unseal with this device's key
   │          └─ fails → not ours. The common case: on a LAN an envelope is
   │             offered to every connected peer and only one can open it.
   │
   │          Olm decrypt
   │          merge sender_chain, if present      ← before authentication
   │          authenticate: does that account's signed chain list this device,
   │                        with the key that actually decrypted?
   │          └─ fails → dropped, silently
   │
   │          file under the *sender*, not the label the sender chose
   │          dedupe on message id
   │          replenish one-time keys if the supply ran low
   ▼
  UI          event → refresh the thread
```

The chain merge happening *before* authentication is the subtle one. A device
the sender added since we last heard from them is not on the roster we hold —
and the update that fixes that is riding in the very message the check would
otherwise reject. Merging first is safe because `merge` only ever *extends* a
chain we already have; adopting one for an unknown account would let a stranger
install themselves as a contact, and only an invite may do that.

## `vega-core` modules

| Module | Responsibility |
|---|---|
| `identity` | Accounts, devices, key material at rest. Account id = hash of the root public key. |
| `sigchain` | The device roster as a hash-linked signed log. Replays and validates offline. |
| `keys` | Newtypes over raw public-key bytes — `Copy`, ordered, canonical, base64 in JSON. |
| `codec` | Canonical byte encoding for anything signed. Length-prefixed, so no two field sequences collide. |
| `seal` | Sealed sender: one-shot ECIES hiding who sent a message. |
| `session` | Olm sessions per device pair, fan-out, and sender authentication. |
| `envelope` | The three nested wire layers. |
| `tag` | Pairwise secrets and everything derived from them. |
| `store` | redb persistence: identity, chains, contacts, sessions, messages, outbox. |

### The `Directory` trait

`session::Directory` is how `vega-core` asks "which devices speak for this
account?" without knowing that the answer lives in a database. The app
implements it over its contact store; tests implement it over a `Vec`.

It has two methods, and the second is easy to get wrong: `offer_chain` must
*extend only*. An implementation that adopts a chain for an unknown account
turns every message from a stranger into a contact request that answers itself.

## `vega-net` modules

| Module | Responsibility |
|---|---|
| `behaviour` | The composed libp2p `NetworkBehaviour` — one field per tier. |
| `node` | The swarm task, and the `NodeHandle` everything else drives it through. |
| `protocol` | Protocol ids, request/response types, and every size limit. |
| `rendezvous` | Encrypted address records for the DHT. |
| `mailbox` | Bounded storage for envelopes awaiting collection. |
| `config` | Desktop and mobile node profiles. |

### One task owns the swarm

`Swarm` is neither `Sync` nor cheap to move, so it lives on a single tokio task.
Callers send commands over an mpsc channel and receive `NetEvent`s over another.
That keeps it off the UI thread and out of the app's type signatures.

Events are **dropped** rather than blocking when the channel is full. A stalled
consumer must not stall the network.

### The libp2p identity is not the account identity

A fresh keypair every start. A stable `PeerId` would let every LAN and every DHT
node this device ever touches link its sessions together. Who you are is
established afterwards, inside the encrypted session.

## `app/src-tauri`

| Module | Responsibility |
|---|---|
| `lib` | Tauri commands, the event pump, and the background loops. |
| `runtime` | The only place that knows both crypto and networking. |
| `invite` | Contact exchange: encode, verify, safety numbers. |
| `keystore` | Where the database key lives. Two functions wide, on purpose. |

### Four background loops

| Loop | Interval | Does |
|---|---|---|
| `pump` | event-driven | Drains `NetEvent`, opens what is ours, tells the UI |
| `retry_loop` | 10s | Flushes the outbox; climbs to DHT rendezvous if nothing is reachable |
| `announce_loop` | 20min | Republishes address records, one per contact |
| `upkeep_loop` | 6h | Prunes the replay set, tops up one-time keys |

The announce interval is half the record TTL, leaving room for two failures
before a contact loses track of us.

## State that persists

redb, in the app data directory, encrypted with a key from `keystore`.

| Table | Holds |
|---|---|
| `meta` | The pickled identity, and sequence counters |
| `chains` | One sigchain per account we know, including our own |
| `contacts` | Display name, contact key, verification state, chain-sent watermark |
| `sessions` | Olm session pickles, each with the remote identity key it is bound to |
| `messages` | Decrypted history |
| `seen` | Message ids, for replay defence. Pruned on a two-week window |
| `outbox` | Envelopes awaiting delivery |

Storing the remote identity key beside each session pickle matters: vodozemac's
`SessionKeys::identity_key` is the *initiator's* key, which is our own on any
session we opened. Losing that field would mean falling back to trusting what
the envelope claims — which is exactly the break the security review found.

## Where to add things

| Adding | Goes in | Watch for |
|---|---|---|
| A message type | `envelope::Body` | Handle it in `Runtime::receive`; decide who is allowed to send it |
| A transport | `behaviour::Vega` + a tier in `config` | Whether a phone should participate |
| A network request | `protocol::Request` | A size limit, and what it tells the peer serving it |
| A persisted field | `store` | `#[serde(default)]`, so existing databases still load |
| A key derivation | `tag::Pairwise` | A distinct `info` string; test that it does not collide |
