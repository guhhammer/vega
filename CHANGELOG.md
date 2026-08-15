# Changelog

Notable changes, newest first. Per-commit reasoning lives in [`done/`](done/);
this file is the summary you would read before upgrading.

Format loosely follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versions follow [semver](https://semver.org), with the pre-1.0 caveat that the
wire format is not yet stable and may break between releases.

## [Unreleased]

### Added

- **Identity without a registry.** An account is an Ed25519 keypair; the account
  id is the hash of its public key. Devices are authorised by a hash-linked
  signed log that anyone holding the account id can verify offline.
- **Message cryptography.** Olm double ratchet per device pair (vodozemac),
  wrapped in a sealed-sender layer so relays learn a rotating routing tag and
  nothing else.
- **The transport ladder.** Same-LAN discovery over mDNS and QUIC, direct
  connections with IPv6 first, DHT rendezvous on a private Kademlia network,
  Circuit Relay v2, and mailbox park/collect for offline recipients.
- **Rendezvous that does not leak the social graph.** Lookup keys are derived
  from a pairwise secret, so a DHT node stores a random-looking key holding
  random-looking bytes and cannot be scraped to map who talks to whom.
- **Desktop application.** Tauri v2 with a React frontend: invites, contacts,
  conversations, a persistent outbox, and safety numbers.
- **Headless seed node** (`./make node`) that acts as bootstrap, relay, and
  mailbox, and holds no key that can read anything.
- **One-time key replenishment.** Keys are topped up when they run low, and the
  updated chain reaches contacts inside messages and rendezvous records — both
  encrypted, both serverless.

### Security

Found and fixed during review, before any release:

- **Sender impersonation.** Anyone holding an invite could open a legitimate Olm
  session and label the message as coming from a third party. Decryption proves
  possession of a key, not ownership of an account. The claimed sender is now
  checked against that account's signed device roster, using the identity key
  recorded when the session was created rather than anything the envelope
  asserts, and a session is not filed until the check passes.
- **Forged self-copies.** A contact could send a message that appeared among
  your own sent messages. Only your own account may now do that.
- **Mail theft.** Collecting parked mail required only the routing tag, which
  every relay sees — so any of them could drain a mailbox and destroy the mail.
  Collection now requires a token derived from the pairwise secret.
- **Routing tag tampering.** The tag is bound into the sealed layer's associated
  data, so a relay that rewrites it produces a box that no longer opens.
- **Unbounded chains.** An invite could carry an arbitrarily long chain, or claim
  unlimited devices and multiply the ciphertexts sent per message. Both capped.
- **Unbounded replay set.** Message ids were kept forever; they now age out on a
  window, which the ratchet's own replay defence makes safe.

### Fixed

- **Incoming messages were filed under the wrong conversation.** The conversation
  was taken from a sender-chosen field, which is the recipient from the sender's
  point of view — so received messages never appeared in the thread.
- **Park and collect never worked over the wire.** The routing tag was serialised
  as a CBOR byte string and read back as an array.
- **A forked sigchain shorter than ours was silently ignored** instead of being
  reported, hiding what may be evidence of a compromised root key.

### Known limitations

Stated plainly rather than omitted; the README carries the current list.

- Nothing has been audited. vodozemac has; the code around it has not.
- The database key is a 0600 file, not the platform keystore.
- Device linking is not implemented — an account is one device today.
- Android cross-compiles, but the foreground service and multicast lock plugins
  are not written, so background delivery does not work yet.
- Delivery receipts are handled on arrival but nothing sends one.
