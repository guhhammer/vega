# 0005 — Security review, and what it found

`fix(security): authenticate senders, bind routing, bound growth`

A full adversarial pass over everything in 0002–0004 before any of it shipped.
This file records what was wrong and why, because the reasoning is worth more
than the diff.

## The serious one: sender impersonation

**Decryption proves the sender holds *an* Olm identity key. It says nothing
about whose account that key belongs to.** `Opened.from_account` was copied
straight out of the envelope, where the sender writes it.

The attack needs only a copy of your invite, which is public by design — it is
how conversations start:

1. Eve takes Bob's invite and opens a legitimate Olm session with him.
2. She sends a message with `from_account` set to Alice.
3. Bob decrypts it successfully, because the session really is Eve's, and
   attributes it to Alice.

Worse, the new inbound session was filed under the *claimed* device id. One
forged pre-key message poisoned the session table, and every follow-up message
inherited the trust.

### The fix

The claimed sender is checked against that account's signed device roster,
through a new `Directory` trait the app implements over its contact store. A
session is not filed until the check passes.

The subtlety that cost a round of debugging: vodozemac's
`SessionKeys::identity_key` is the **initiator's** key, which is your own key on
any session you opened. Useless for deciding who is talking to you. Sessions now
record the remote identity key explicitly at creation, from a source that is
already authenticated — the peer's signed chain for sessions we initiate, the
3DH for sessions they initiate.

Three tests cover it, including the session-poisoning variant and the case of a
message from an account we hold no chain for.

## Forged self-copies

`Body::SelfCopy` marks a message as one of *your* outgoing messages. Any contact
could send one and have it appear among the things you said. Now only your own
account may send one.

## Mail theft

Collecting parked mail required only the routing tag — which every relay that
carried a message has seen. Any of them could walk up to a mailbox and collect,
and therefore destroy, someone else's mail.

Collection now requires a token derived from the pairwise secret, which a relay
never sees. A wrong token takes nothing and destroys nothing: the real
recipient's mail is still there afterwards. Compared in constant time.

## Routing tag tampering

The tag has to stay readable — a relay cannot forward what it cannot see. But it
was not authenticated, so a relay could rewrite it and silently misroute.

It is now bound into the sealed layer's associated data. Rewriting it produces a
box that no longer opens, which turns a silent misroute into a detected failure.
A relay can still drop or misdirect an envelope; that is inherent to asking a
stranger to carry your traffic, and the outbox retries.

## Prekey exhaustion

`replenish_prekeys` ran once, at account creation. After ~50 new conversations
the reusable fallback key was used for every new session, costing forward
secrecy on the first message of each.

The reason it was hard: prekeys live in the sigchain, and chains only travelled
inside invites. Fixing it needed a way for an updated chain to reach contacts
with no server.

Two routes, both encrypted and both already existing:

- **Inside messages.** `Content.sender_chain` carries our chain when it has
  advanced since this contact last saw it. It rides inside the ratchet, so it is
  private and authenticated, and reaches exactly the people entitled to it.
- **Inside rendezvous records.** A contact who looks us up to find an address
  also picks up whatever we published since.

One ordering detail matters: the chain update is merged **before** sender
authentication, not after. A device the sender added since we last heard from
them is not yet on the roster we hold — and the update that fixes that is riding
in the very message the check would otherwise reject. Merging first breaks the
deadlock without weakening anything, because `merge` only ever *extends* a chain
we already hold. Adopting a chain for an unknown account would let a stranger
install themselves as a contact; only an invite may do that.

## Unbounded growth

- **Chains.** An invite could carry an arbitrarily long chain (one signature
  check per entry, all attacker-chosen) or claim unlimited devices, which
  multiplies the ciphertexts we produce for every message. Capped at 4096
  entries and 32 devices.
- **The replay set.** Every message id ever seen was kept forever. Now pruned on
  a two-week window, which is safe because the Olm ratchet already refuses
  replays inside a live session; this layer only catches the same message
  arriving over two tiers, seconds apart.

## The lock across network I/O

The event loop held the runtime lock while awaiting `deliver`, so one slow peer
could stall all message processing for up to the 30-second request timeout.

`Runtime` now performs no I/O at all. It returns plans; the caller executes them
unlocked and reports back. A slow peer delays one delivery.

## A plain bug found on the way

Incoming messages were filed under `content.conversation`, which the *sender*
sets to the recipient — so on arrival it was your own account id. Received
messages were filed under yourself and never appeared in the thread. It also let
a contact drop messages into your conversation with someone else.

The integration tests missed it because they stopped at the crypto layer and
never asked where the message ended up.

## What was checked and found clean

- No `unwrap`, `expect`, `panic!` or indexing panic reachable from network input
  anywhere. The remaining `expect`s are on compile-time constants.
- Sealed-sender construction binds both public keys, rejects low-order keys, and
  uses a fresh ephemeral key per box, so identical plaintexts are unlinkable.
- Chain validation checks the signer before applying the entry, so an entry
  cannot authorise its own signer.
- Codec size limits are set explicitly rather than inherited.

## Still open, recorded rather than hidden

The database key is a 0600 file rather than the platform keystore.
`conversation()` scans every stored message. Nothing here has been audited —
vodozemac has, the code around it has not.
