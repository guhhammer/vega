# Wire format

Everything that crosses a network, and what each party can see. Version 1.

Pre-1.0, so this is not stable between releases. `Envelope.v` is checked and an
unknown version is refused rather than guessed at.

## Layers

```
Envelope   { v, to_tag, epoch, sealed }        ← a relay or mailbox sees this
  sealed = ECIES box
    Inner  { from_account, from_device, … }    ← only the recipient device
      olm_ct = Olm ciphertext
        Content { body, sent_at, … }           ← only after the ratchet opens it
```

## Envelope

JSON. The outermost layer, and the only part an intermediary can read.

```json
{
  "v": 1,
  "to_tag": "b3f1…",              // 16 bytes, lowercase hex
  "epoch": 487500,                 // unix seconds / 3600
  "sealed": "Zm9vYmFy…"            // base64, no padding
}
```

| Field | Meaning |
|---|---|
| `to_tag` | Rotating routing tag. Meaningless to anyone who is not the recipient — see below. |
| `epoch` | Which hour's tag this is, so a recipient can check the neighbouring epochs across a rollover. |
| `sealed` | The ECIES box. |

**What a relay learns from this:** two routing tags across a conversation, the
size of each envelope, and when they were sent. Not who is talking, not to whom,
not what about.

## The sealed box

```
sealed = ephemeral_x25519_public (32 bytes) ‖ ChaCha20-Poly1305 ciphertext
```

Key derivation:

```
shared  = X25519(ephemeral_secret, recipient_seal_public)
okm     = HKDF-SHA256(
            salt = ephemeral_public ‖ recipient_public,
            ikm  = shared,
            info = "vega:seal:v1",
            len  = 44)
key     = okm[0..32]
nonce   = okm[32..44]

aad     = ephemeral_public ‖ recipient_public ‖ len(context) as u32 BE ‖ context
context = to_tag (16) ‖ epoch (8, big endian)
```

Three properties fall out of that construction:

- **Fresh ephemeral key per box**, so two identical plaintexts are unlinkable on
  the wire.
- **Both public keys bound into salt and AAD**, so a box replayed at a different
  recipient derives a different key and fails.
- **The routing header bound into the AAD.** The tag has to stay readable — a
  relay cannot forward what it cannot see — but rewriting it now produces a box
  that no longer opens. Tampering surfaces as a failure instead of a message
  quietly going to the wrong place.

Low-order recipient keys are refused rather than encrypted to a known constant.

## Inner

JSON, inside the seal. This is where the sender identifies themselves, which is
precisely why it is sealed.

```json
{
  "from_account": "a1b2…",     // 32 bytes, hex
  "from_device":  "c3d4…",     // 32 bytes, hex
  "from_olm":     "MCowBQ…",   // Curve25519 public, base64
  "to_device":    "e5f6…",     // 32 bytes, hex
  "olm_type":     0,           // 0 = pre-key message, 1 = normal
  "olm_ct":       "…"          // base64
}
```

> **These fields are claims, not facts.** Anyone can seal a box to a device
> whose public key they have — and every invite contains one. Decryption proves
> the sender holds *an* Olm identity key; it says nothing about whose account it
> belongs to. `from_account` and `from_device` are checked against that
> account's signed sigchain before a message is accepted, using the identity key
> recorded when the session was created rather than `from_olm`.

## Content

JSON, inside the Olm ciphertext.

```json
{
  "id":           "7a8b…",     // 32 random bytes, hex — dedupe key
  "conversation": "a1b2…",     // recipient's account, from the *sender's* view
  "sent_at":      1755273600,
  "seq":          42,
  "body":         { "type": "text", "text": "…" },
  "sender_chain": { "entries": [ … ] }   // optional
}
```

`conversation` is the recipient as the sender sees it, so a receiver must **not**
file a message under it — it would land under the receiver's own account. Filing
happens under the authenticated sender.

`sent_at` is the sender's clock. A hint for ordering, never trusted for anything
security-relevant.

`sender_chain` carries the sender's sigchain when it has advanced since this
contact last saw it. This is how fresh one-time keys reach a contact with no
server: it rides inside the ratchet, so it is private and authenticated.

### Body variants

```json
{ "type": "text",      "text": "…" }
{ "type": "receipt",   "message_id": "7a8b…" }
{ "type": "self_copy", "to": "a1b2…", "message_id": "7a8b…", "text": "…" }
{ "type": "file",       "file": { … } }
{ "type": "file_chunk", "file": { … }, "index": 0, "data": "base64" }
```

`self_copy` is how multi-device sync works without a server — a copy of an
outgoing message addressed to the sender's own devices. **Only accepted when the
authenticated sender is your own account**; otherwise a contact could plant
messages among the things you said.

### Files

A file is one `file` message, which is what a thread shows, followed by `chunks`
separate `file_chunk` messages carrying the bytes. Each is an ordinary message:
ratcheted, sealed and routed like any other, so a relay cannot tell a file from a
sentence.

```json
"file": {
  "transfer": "3c4d…",       // 32 random bytes, hex — names the transfer
  "name":     "holiday.jpg", // the sender's word, never used as a path unsanitised
  "size":     2097152,
  "hash":     "9e1f…",       // blake3 of the whole file
  "chunks":   21
}
```

The manifest is repeated **on every chunk**, at a cost of a few hundred bytes
against a 96 KiB payload. Nothing orders these messages — a retry, or a mailbox
handing back what it held, can deliver the last chunk before the announcement —
so any chunk must be able to open the transfer. Without that, an early chunk
would be dropped for belonging to a transfer nobody had announced yet, and the
file would never complete or explain why.

Every number in the manifest is the sender's claim, and each is checked once when
the transfer opens: `chunks` must be exactly what `size` implies, and `size` must
be within `MAX_FILE_BYTES`. Everything after that — that an index is in range,
that a chunk is chunk-sized, that the total cannot exceed the limit — follows.
The `hash` is verified after reassembly; a file that fails is discarded along
with the transfer rather than written anywhere.

`name` is the most obviously hostile field in the protocol: `../../.ssh/authorized_keys`
is a valid JSON string. It is reduced to a single harmless path component before
it reaches a filesystem, and each transfer is written into its own directory
named after `transfer`, so two files called `photo.jpg` cannot overwrite each
other.

Chunks are not messages. They are not shown, not receipted, and not copied to the
sender's own devices — a self-copy would double a ten-megabyte send. **A file
therefore does not appear on the sender's other devices**, which is the one place
files behave differently from text.

#### What a file costs an offline recipient

A mailbox holds `MAX_PARKED_PER_TAG` = 32 envelopes for one tag, so roughly 3 MB
of file will wait for someone who is offline. Past that the remaining chunks stay
in the sender's outbox and go out when the recipient reappears — the transfer
completes, it just needs both ends up at once. Those caps are deliberately not
raised for files: every parked envelope is a stranger's disk being used, and a
messenger that lets one person park ten megabytes with a volunteer is a messenger
nobody volunteers for.

## Derived values

Everything a third party might use to follow you around comes from one pairwise
secret, which two contacts derive with no round trip.

```
pairwise = HKDF-SHA256(
             salt = min(account_a, account_b) ‖ max(account_a, account_b),
             ikm  = X25519(my_contact_secret, their_contact_public),
             info = "vega:pairwise:v1",
             len  = 32)
```

Account ids are sorted so both sides reach the same value regardless of who
computes it.

| Value | `info` | Length | Used for |
|---|---|---|---|
| Routing tag | `"vega:tag:v1" ‖ recipient_account` | 16 | `Envelope.to_tag` |
| Rendezvous key | `"vega:rendezvous:v1" ‖ owner_account` | 32 | The DHT key an address is published under |
| Record key | `"vega:record:v1" ‖ owner_account` | 32 | Encrypts the record's contents |
| Collect token | `"vega:collect:v1" ‖ tag` | 32 | Claiming parked mail |

All four use the epoch as the HKDF salt, so all four rotate hourly.

Tags are **directional** — the tag I use to address you is not the one you use
to address me — so an observer who sees both cannot pair them up.

The collect token exists because the tag is visible to every relay that carried a
message. Without a second secret, any of them could walk up to a mailbox and
collect, and so destroy, someone else's mail.

## Identifiers

```
account_id = BLAKE3("vega:account-id:v1" ‖ ed25519_root_public)
device_id  = BLAKE3("vega:device-id:v1"  ‖ ed25519_device_public)
```

Domain-separated so the two can never collide even on the same input. Displayed
as lowercase base32 in groups of four, because they are meant to be read aloud
and compared.

## Sigchain entries

Signed over a hand-written canonical encoding, because `serde_json` does not
promise a stable byte string and a signature must cover exactly one.

```
signing_bytes = "vega:sigchain-entry:v1"
              ‖ seq (8, BE)
              ‖ prev (32)
              ‖ ts (8, BE)
              ‖ body_canonical
              ‖ signer_public (32)

entry_id = BLAKE3(signing_bytes ‖ signature)
```

Every variable-length field inside `body_canonical` is length-prefixed with a
u32 BE, so no two distinct field sequences can produce the same bytes.

Body discriminants: `1` Genesis, `2` AddDevice, `3` RevokeDevice,
`4` PublishPrekeys.

The next entry commits to `entry_id`, which covers the signature — so the link
pins the entry exactly as it was signed.

## Network protocols

| Protocol | Purpose |
|---|---|
| `/vega/kad/1.0.0` | Kademlia. A private id, so this is our own DHT rather than a corner of the public IPFS one. |
| `/vega/id/1.0.0` | libp2p identify. |
| `/vega/msg/1.0.0` | Delivery, parking and collection. CBOR. |

### Requests

```rust
Deliver { envelope: Vec<u8> }
Park    { tag: [u8; 16], token: [u8; 32], envelope: Vec<u8>, expires_at: u64 }
Collect { claims: Vec<([u8; 16], [u8; 32])> }
```

### Responses

```rust
Accepted
Collected { envelopes: Vec<Vec<u8>> }
Refused   { reason: String }
```

`Refused` carries a reason so a sender can try another peer rather than assume
success. A `Collect` with a wrong token returns an empty `Collected` — it takes
nothing and destroys nothing, so a guess cannot be used to delete someone's mail.

## Rendezvous records

Stored in the DHT under the rendezvous key. To anyone else this is a
random-looking key holding random-looking bytes.

```
value = nonce (12) ‖ ChaCha20-Poly1305(record_key, plaintext, aad)
aad   = "vega:rendezvous-record:v1"
```

Plaintext:

```json
{
  "peer_id":      "12D3KooW…",
  "addrs":        ["/ip6/…/udp/…/quic-v1", "/ip4/…/tcp/…"],
  "published_at": 1755273600,
  "expires_at":   1755277200,
  "chain":        { "entries": [ … ] }
}
```

The chain travels with the record, so a contact who looks us up to find an
address also collects whatever devices and one-time keys we published since.

Nobody signs a record. The chain inside is trusted only as far as `merge`
allows: it must extend a chain the reader already holds.

## Limits

Everything a stranger can cause us to allocate is bounded.

| Limit | Value | Why |
|---|---|---|
| `MAX_ENVELOPE_BYTES` | 256 KiB | The ceiling on one message |
| `MAX_FILE_BYTES` | 10 MiB | Largest file, ≈107 chunks. A courtesy limit: every chunk is an envelope somebody else's node may carry |
| `FILE_CHUNK_BYTES` | 96 KiB | Raw bytes per chunk. Base64 into the content JSON and the two base64 layers below it cost ≈2.4× on the wire, landing a full chunk near 235 KiB |
| `MAX_FILE_NAME_BYTES` | 255 | Longest file name accepted from a peer |
| `MAX_REQUEST_BYTES` | 272 KiB | Envelope plus framing, enforced by the codec before parsing |
| `MAX_RESPONSE_BYTES` | 8 MiB | A full mailbox for one tag |
| `MAX_PARK_SECS` | 7 days | Longest a mailbox will hold anything |
| `MAX_PARKED_PER_TAG` | 32 | Oldest evicted first once reached |
| `MAX_PARKED_TOTAL` | 4096 | Node-wide, across every tag |
| `MAX_ENTRIES` (chain) | 4096 | One signature check each, all attacker-chosen |
| `MAX_DEVICES` | 32 | Each live device multiplies the ciphertexts per message |
| `EPOCH_SECS` | 3600 | Tag and key rotation |
| `RECORD_TTL_SECS` | 3600 | Rendezvous record lifetime |
| Replay window | 14 days | How long a message id is remembered |
