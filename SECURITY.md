# Security

Vega is a messenger. A bug here can expose what someone said, or who they said
it to, and that second one is often the more dangerous of the two.

## Reporting a vulnerability

**Do not open a public issue.** Use GitHub's
[private vulnerability reporting](https://github.com/guhhammer/vega/security/advisories/new)
on this repository, or email the maintainer.

Please include what you need to make the problem reproducible: the version or
commit, what you did, what happened, and — if you have one — a failing test. A
test that demonstrates the break is worth more than a paragraph describing it,
and it becomes the regression test for the fix.

You will get an acknowledgement within a few days. If a report turns out to be
real, the fix and the advisory go out together, and you are credited unless you
would rather not be.

## What is in scope

Anything that breaks one of these:

- **Confidentiality.** Nobody but the intended devices can read a message.
- **Authentication.** A message attributed to someone was really sent by them.
- **Metadata.** A relay, mailbox, or DHT node learns no more than the design
  says it does — routing tags, sizes, and timings, never identities or content.
- **Availability of your own data.** Nobody else can delete or deny your mail.

Also in scope: anything that makes a node crash, hang, or consume unbounded
memory or CPU in response to bytes from the network.

## What is not in scope

These are known and documented, not bugs to report:

- **A global passive observer can correlate traffic.** Vega is not Tor and does
  not claim to be. Onion routing over the relay tier is a possible future
  addition, at a real latency cost.
- **Relays and mailboxes learn that you are online**, and how much you send and
  when. Content and identity stay hidden; participation does not.
- **A compromised device reads its own messages.** No protocol fixes an attacker
  who is already inside the endpoint.
- **A substituted contact invite.** Invites are the trust root and travel out of
  band. Compare safety numbers; that is what they are for.
- **Denial of service from someone who can already reach you at the network
  layer.** Firewalls and rate limits live below this project.

## Known weaknesses

Tracked in the README rather than hidden. The current list is short and each
entry says what it costs and what would fix it. Two worth repeating here:

- **The database key is a 0600 file**, not the platform keystore
  (`app/src-tauri/src/keystore.rs`). It protects against another user on the
  machine and against a stolen backup, and nothing else.
- **Nothing here has been audited.** vodozemac, which implements the ratchet,
  has been audited by Least Authority. The code around it has not been audited
  by anyone.

## Supported versions

Pre-1.0. Only the tip of `main` receives fixes. There is no long-term support
branch and it would be dishonest to pretend otherwise.

| Version | Supported |
| ------- | --------- |
| `main`  | yes       |
| tagged releases | no |

## Cryptography

For reviewers, the short version of what is used and why:

| Layer | Primitive | Why |
| --- | --- | --- |
| Message | Olm double ratchet (vodozemac) | Audited, per-device, forward secret |
| Sealed sender | X25519 → HKDF-SHA256 → ChaCha20-Poly1305 | Hides the sender from relays |
| Transport | Noise XX (libp2p) | Per-hop, mutually authenticated |
| Identity | Ed25519 over a hash-linked log | Offline verifiable, no registry |
| Routing tags | HKDF-SHA256 of a pairwise secret | Unguessable without being a contact |

No novel cryptography. Where a choice looked clever, it was replaced with a
boring one — see the note on Olm session versions in
`crates/vega-core/src/session.rs`.
