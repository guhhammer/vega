# 0002 — Identity, sigchain and message cryptography

`feat(core): identity, sigchain and message cryptography`

`vega-core`. No sockets, no async runtime, no server. It turns a message plus a
recipient into sealed envelopes, and sealed envelopes back into messages.

59 tests.

## Identity is self-certifying

An account is an Ed25519 keypair and the account id is the BLAKE3 hash of its
public key. There is no registration step, no namespace to contend over, and no
lookup that can be denied or poisoned — if you can name someone you already hold
everything needed to verify them.

That single decision is what removes the server. Everything else follows from
having nowhere to ask "who is this person?".

The id is rendered as lowercase base32 in groups of four, because it is meant to
be read aloud and compared by a human. Parsing tolerates case and grouping.

## The sigchain replaces the key server

Devices are authorised by a hash-linked append-only log where every entry is
signed by a key the chain already trusts. Anyone holding the account id can
replay it offline and derive which devices legitimately speak for that account.

Validation checks the signer **before** applying the entry, so an entry can
never authorise its own signer. Revoked devices are removed from the roster, so
a later lookup is also a revocation check — one fewer thing to remember.

`merge` refuses a fork in both directions rather than taking whichever chain is
longer. Two valid chains for one account that disagree about history mean either
a bug or a compromised root key, and both are things the caller must be told
about rather than have papered over. A shorter chain that is a prefix of ours is
simply older and merges to a no-op; one that diverges is an error.

## The root key is optional

`Identity::root` is an `Option` on purpose. The root signs the genesis entry and
the first device; after that any live device can authorise the next one. So the
root need not be present on every device, and a device that never holds it
cannot be used to take over the account.

## Canonical encoding for anything signed

`codec.rs` exists because `serde_json` cannot promise a stable byte string —
map ordering and number formatting are not pinned by the format. Signed
structures are encoded by hand, every field length-prefixed, so no two distinct
field sequences can produce the same bytes. There is a test for exactly that
confusion: without the prefix, `("ab","c")` and `("a","bc")` would collide.

## Message cryptography: vodozemac, not our own

Olm double ratchet per device pair. Audited by Least Authority, Apache-2.0, and
running in production in every Element client. The alternative, libsignal, is
AGPL-3.0 — a licensing decision as much as a technical one. Hand-rolling a
double ratchet was never on the table.

One deliberate choice worth recording: **Olm session version 1, not 2.** v2 has
an untruncated MAC and is better on paper, but it sits behind vodozemac's
`experimental-session-config` feature, and enabling an experimental flag for a
security-critical component is the wrong trade. The truncation is also not
load-bearing here — every Olm ciphertext is wrapped in the sealed-sender layer,
whose Poly1305 tag authenticates the whole thing at full length.

## Sealed sender

Olm needs the sender's identity key to open a pre-key message. Putting that key
in the clear would tell every relay who is talking to whom, which is precisely
the metadata this design exists to withhold. So the sender's identity travels
inside a one-shot ECIES box addressed to the recipient device: ephemeral X25519
→ HKDF-SHA256 → ChaCha20-Poly1305.

Both public keys are bound into the salt and the AEAD's associated data, so a
box cannot be replayed at a different recipient. Low-order recipient keys are
refused rather than encrypted to a shared constant.

The routing header is bound too — see 0005.

## Pairwise secrets

Two accounts that know each other's contact key derive a shared secret with no
round trip. Everything a third party might use to follow you around comes from
it: the tag announced on a LAN, the key an address is published under in the
DHT, the token that claims parked mail.

Tags are **directional** — the tag I use to address you is not the one you use
to address me — so an observer who sees both cannot pair them up. They rotate
hourly, which bounds how long a leaked tag stays useful.

This is the property that stops the DHT becoming a scrapable map of the social
graph. The lookup key is unguessable unless you are already a contact.

## Nothing panics on untrusted input

The prototype this replaces called `.unwrap()` on bytes off a socket, which is a
remotely triggerable crash. There are no panics on network input anywhere in
this crate; the remaining `expect`s are on compile-time constants. There is a
test that feeds truncated input at every length and asserts none of it panics.

## Storage

redb — embedded, pure Rust, no C to cross-compile, which is what keeps the
Android build simple. Secrets are encrypted with a `pickle_key` the caller
supplies; this crate never decides where that key lives, because on a real
install it belongs in the platform keystore.

An invalid chain is never written: a corrupt chain on disk is indistinguishable
from one an attacker planted there.
