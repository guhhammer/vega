# Vega

An encrypted messenger with no server. Peers find each other, carry each other's
ciphertext, and hold each other's mail.

Nothing here is a service you have to run or pay for, no node can read a message,
and no node's disappearance kills the network. The full design, including what
that does *not* protect against, is in
[`.documentation/design.md`](.documentation/design.md) — also published as an
[artifact](https://claude.ai/code/artifact/55208dd3-3049-42ef-a923-da62b1a9f61a).

## Layout

```
crates/vega-core   identity, sigchain, message crypto — knows nothing about networking
crates/vega-net    the transport ladder — moves opaque bytes, cannot read them
app/               Tauri desktop app (React frontend, Rust backend)
app/src-tauri      the shell: commands, runtime, key storage
```

The split is load-bearing. `vega-net` handles envelopes it cannot open, which is
what makes it safe for a stranger's node to relay them.

## Running it

```bash
./make check     # fmt, clippy, tests, typecheck — run this constantly
./make dev       # desktop app against the vite dev server
./make dist      # a real installer
./make node      # headless bootstrap/relay/mailbox node
```

`cargo build` alone produces binaries that prove the code compiles but are not
applications — without the Tauri CLI there is no frontend embedded. Use `dev` or
`dist`.

### Two machines on one network

Start the app on both. They find each other over mDNS with no configuration.
Copy the invite from one (`My invite`) and paste it into the other
(`Add contact`), then send a message.

### Across the internet

Run a seed somewhere reachable:

```bash
./make node -- --port 15000
```

It prints its address. Put that in `seeds.json` in the app's data directory — a
plain JSON array of multiaddrs — and restart. No rebuild needed. A seed holds no
key that can read anything, and cannot withhold a message it never sees.

## How a message travels

1. Encrypted with Olm — a separate ciphertext for every one of the recipient's
   devices, plus a copy to each of your own.
2. Wrapped in a sealed-sender box, so an intermediary sees a rotating routing
   tag and nothing else.
3. Handed to the cheapest transport that works: the LAN, then a direct internet
   connection, then a relay peer, then a mailbox peer if the recipient is gone.

## What is built

| | |
|---|---|
| Identity, sigchain, device roster | done, tested |
| Olm sessions, sealed sender, replay defence | done, tested |
| Local storage, outbox, invites | done, tested |
| T0 LAN discovery and delivery | done, tested over real sockets |
| T2 DHT rendezvous — publish, look up, dial | done, tested over real sockets |
| T4 mailbox park/collect | done, tested over real sockets |
| T1 hole punching, T3 relay | wired end to end, but only two machines on two real networks can prove it |
| Delivery receipts | done, tested — the outbox clears when the recipient confirms decryption, not when a peer accepts bytes |
| Key storage | platform keyring (Secret Service / Keychain / Credential Manager), with a 0600 file where none exists |
| Device linking (second device on one account) | the crypto is there — `Identity::adopt`, and the sigchain accepts device-signed additions — but there is no pairing flow, so an account is one device today |
| Android | cross-compiles; foreground service and multicast lock plugins not written (see `.documentation/android.md`) |
| Groups, T5 offline mesh | not started |

## Security notes

Read the [design document's](.documentation/design.md) non-goals section before
trusting this with anything.
**Nothing here has been audited.** vodozemac (the ratchet) has been; the code
around it has not.

### Known weaknesses, not yet fixed

- **Device linking is not implemented.** An account is one device. The crypto
  exists and the sigchain accepts device-signed additions; the pairing flow does
  not, and a rushed one would be worse than none.
- **Where there is no keyring, the key is a 0600 file.** Headless systems,
  containers, and Android take the fallback path. It protects against another
  user on the machine and a stolen backup, and nothing else. The app logs which
  backing it used at startup.
- **Android has no background delivery.** It cross-compiles; the foreground
  service and multicast lock plugins are not written.
- **A relay can still misroute.** The routing tag is now bound into the sealed
  layer, so rewriting it is detected rather than silently obeyed — but a relay
  that drops or misdirects an envelope still denies delivery. That is inherent
  to asking a stranger to carry your traffic; the outbox retries.

### Fixed during review

- **Prekey exhaustion.** One-time keys were published once and never again, so
  after ~50 conversations everyone fell back to the reusable fallback key. They
  are now topped up when they run low, and the updated chain reaches contacts
  two ways that need no server: attached inside the next encrypted message, and
  carried in the rendezvous record a contact fetches to find us. The merge
  happens *before* sender authentication, so a device added since we last spoke
  is not locked out by the very check that needs the update it is carrying.
- **Routing tag tampering.** The tag stays readable — a relay cannot forward
  what it cannot see — but it is now bound into the sealed layer's associated
  data, so rewriting it produces a box that no longer opens.
- **Unbounded replay set.** Message ids are pruned on a two-week window. Safe
  because the ratchet already refuses replays inside a live session; this layer
  only catches the same message arriving over two tiers, seconds apart.
- **Prekey exhaustion, receipts, and the conversation scan.** All three are
  fixed; see the 0.1.0 entry in [CHANGELOG.md](CHANGELOG.md).
- **The lock was held across network I/O.** `Runtime` now performs no I/O at
  all: it returns plans, and the caller executes them unlocked. A slow peer
  delays one delivery instead of every incoming message.
- **Sender impersonation.** Anyone holding your invite could open a legitimate
  Olm session and label the message as coming from a third party — decryption
  proves possession of *a* key, not whose account it is. The claimed sender is
  now checked against that account's signed device roster, using the identity
  key recorded when the session was created rather than anything the envelope
  asserts. Sessions are no longer filed before that check passes.
  (`crates/vega-core/src/session.rs`)
- **Forged self-copies.** A contact could send a `SelfCopy` and have it appear
  in your own outgoing messages. Now only your own account may send one.
- **Mail theft.** Collecting parked mail required only the routing tag, which
  every relay sees — so any of them could drain a mailbox and destroy the mail.
  Collection now needs a token derived from the pairwise secret.
- **Unbounded chains.** An invite could contain an arbitrarily long chain
  (CPU) or claim unlimited devices (fan-out amplification). Both are capped.
- **Misfiled incoming messages.** Conversation was taken from a sender-chosen
  field, so incoming messages landed under the wrong contact and a contact could
  drop messages into an unrelated thread.
