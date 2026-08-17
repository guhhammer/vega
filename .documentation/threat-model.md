# Threat model

Who the adversaries are, what each can actually do, and what stops them.
[SECURITY.md](../SECURITY.md) is the reporting policy; this is the analysis.

Stating limits precisely is what separates a security design from a claim.

## What is being protected

| | |
|---|---|
| **Content** | Only the intended devices can read a message. |
| **Authorship** | A message attributed to someone was sent by them. |
| **Association** | Who is talking to whom stays hidden from intermediaries. |
| **Continuity** | A past compromise does not decrypt past messages; a present one does not last forever. |
| **Availability of your data** | Nobody else can delete or withhold your mail. |

Association is the one people underestimate. Content encryption is the easy
half; the metadata is what this project is actually about.

## Adversaries

### A. Someone on your local network

*Coffee shop wifi, a shared flat, a hostile office.*

**Can:** see that a Vega node is present, see every envelope offered on the LAN
(delivery is a broadcast), see sizes and timings, join as a peer.

**Cannot:** open an envelope, tell which peer an envelope is for, or learn a
stable identifier for anyone.

**What stops them:** the libp2p `PeerId` is a fresh keypair every start, so
there is no stable identity to log. Routing tags rotate hourly and are derived
from a pairwise secret, so a tag is recognisable only to the contact it names.
The sealed layer means an envelope offered to twenty peers opens for one.

**Residual:** they learn that *somebody* is using Vega here, and roughly how
much. The prototype this replaces broadcast a stable name to the whole LAN
forever; that is fixed, but presence itself is not hideable on a LAN.

### B. A relay or mailbox peer

*A stranger whose node is carrying your ciphertext. By design there will be many.*

**Can:** see two routing tags, the byte count, and the timing of everything it
forwards. Refuse to forward. Drop an envelope.

**Cannot:** read content, learn either party's account, link a tag to an identity,
link this hour's tags to last hour's, or collect mail it is holding for someone
else.

**What stops them:** end-to-end Olm inside the hop encryption, sealed sender
hiding the sender's key, hourly tag rotation, and a collect token derived from
the pairwise secret — the tag alone is not enough to claim mail.

**Residual:** it knows you are online and roughly how much you send. It can deny
delivery by dropping. The outbox retries and climbs a tier, but a relay that is
your only path can stop you. That is inherent to asking a stranger to carry your
traffic.

### C. A DHT node

*Stores rendezvous records.*

**Can:** see a 32-byte key holding an opaque blob, and how often it changes.

**Cannot:** determine whose record it holds, read the addresses inside, or find
a record without already being a contact of the publisher.

**What stops them:** the lookup key is `HKDF(pairwise_secret, epoch)`. A naive
design publishing "account X is at Y" would turn the DHT into a scrapable map of
the social graph; here the key is unguessable unless you are already a contact.

**Residual:** a node can see *that* records exist and count them. Publishing one
record per contact means the count leaks an upper bound on your contact list
size to a node positioned to observe all of them.

### D. Someone holding your invite

*Which is everyone you ever invited. Invites are public by design.*

**Can:** open a legitimate Olm session with you and send you messages. Consume
your one-time keys.

**Cannot:** impersonate anyone else to you, or read anything not addressed to
them.

**What stops them:** this is the attack the security review found and fixed. The
claimed sender is checked against that account's signed device roster, using the
identity key recorded when the session was created — not anything the envelope
asserts. A session is not filed until the check passes, so a forged message
cannot poison the session table for later ones.

**Residual:** they can burn one-time keys by opening sessions. Keys are topped up
below a low-water mark, and exhaustion degrades to the reusable fallback key —
costing forward secrecy on the *first* message of a new session only.

### E. A network observer

*An ISP, a national gateway, anyone watching a link.*

**Can:** see that you are speaking a peer-to-peer protocol, to which addresses,
and how much. Correlate timing across both ends if they can see both.

**Cannot:** read anything.

**What stops them:** not much, and this is deliberate. **Vega is not Tor.** It
does not pad, does not cover-traffic, and does not onion-route. Traffic analysis
by an adversary who sees both ends works.

**Residual:** everything above. Onion routing over the relay tier is a possible
later addition at a real latency cost. Until then, do not use this where being
*seen to communicate* is the danger.

### F. Someone who steals your device

**Can:** read everything on it, if they can also read the database key.

**Cannot:** decrypt history from before a compromised session was established —
the ratchet deletes as it advances.

**What stops them:** forward secrecy from the double ratchet, and encryption at
rest for everything with content in it.

Message bodies, contacts, sigchains, queued envelopes, partial transfers and
received files are stored sealed with XChaCha20-Poly1305 under a subkey derived
from the device key — see [`at_rest`](../crates/vega-core/src/at_rest.rs). The
account identity and the Olm sessions were already encrypted pickles. A received
file is a sealed blob in a directory named after its transfer id, so neither its
contents nor its name reaches the disk in the clear.

**What they still learn.** Encryption covers values, not keys, and the keys are
metadata: which account ids you hold, how many messages each conversation has,
when each message arrived, and how many files. Hiding that means encrypting the
index too, which needs a different design than lookups by account id. The local
device nickname is also stored in the clear.

**Residual, and it is the sharpest one:** where the key lives. With no platform
keyring — headless systems, containers, Android — it falls back to a 0600 file
beside the database, and the encryption above is then only as good as the file
permissions protecting that key. Against someone with your unlocked session it
does nothing: Vega is running, so the key is in memory and the history opens.
What it does defend is a stolen disk, a backup, or another user on the machine.
Written down in [`../README.md`](../README.md) rather than glossed.

### G. Someone who substitutes an invite in transit

**Can:** become the conversation, permanently and invisibly.

**Cannot:** do it after safety numbers have been compared.

**What stops them:** nothing automatic. This is the trust root and it is manual
on purpose. Safety-number comparison is the check, and the invite sheet in the
UI says so rather than burying it in documentation.

### H. A malicious contact

*Someone you legitimately added.*

**Can:** read what you send them — that is what a contact is. Send you anything.
Add devices to their own account, which you will then encrypt to.

**Cannot:** attribute a message to a third party, plant a message among your own
sent messages, drop a message into your conversation with someone else, or make
you fan out to unlimited devices.

**What stops them:** sender authentication against the sigchain; `self_copy`
accepted only from your own account; incoming messages filed under the
authenticated sender rather than a sender-chosen field; a 32-device cap; a
4096-entry chain cap.

**Residual:** they can still be a person who repeats what you told them. No
protocol fixes that.

## Cryptographic dependencies

If one of these is broken, so is Vega.

| | Used for | If broken |
|---|---|---|
| Ed25519 | Sigchain signatures | Device rosters forgeable — total identity break |
| X25519 | All key agreement | Everything |
| Olm / double ratchet | Message encryption | Content exposed |
| ChaCha20-Poly1305 | Sealed sender, records | Content and sender exposed |
| HKDF-SHA256 | Every derived secret | Tags predictable — association exposed |
| BLAKE3 | Ids, chain links | Id collisions; chain history rewritable |

vodozemac has been audited by Least Authority. The code around it has not been
audited by anyone.

## Explicit non-goals

- **Anonymity.** Vega hides who you are talking to from intermediaries. It does
  not hide that you are using it.
- **Plausible deniability of participation.** A relay knows you are online.
- **Resistance to a global passive adversary.** See E.
- **Protection from your own devices.** See F.
- **Availability under sustained attack.** A well-resourced adversary who can
  block your seeds and your relays can stop you communicating. Many seeds and
  LAN discovery are the mitigations, not a solution.

## Assumptions

Written down because a broken assumption invalidates the analysis above:

1. The device's RNG is sound. Every key here comes from `OsRng`.
2. The user compares safety numbers, or accepts the risk of not doing so.
3. The binary the user runs is the one that was built. Vega does not solve
   supply chain; reproducible builds would help and are not implemented.
4. At least one bootstrap seed is reachable, or the peer is on the same LAN.
5. Contacts are not compelled to hand over their devices.
