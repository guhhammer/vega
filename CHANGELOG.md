# Changelog

Notable changes, newest first. Per-commit reasoning lives in [`done/`](done/);
this file is the summary you would read before upgrading.

Format loosely follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versions follow [semver](https://semver.org). **The wire format is still not
stable** and may break between releases — the version number tracks the
application, not a protocol guarantee, and two different builds may not talk to
each other.

## [Unreleased]

### Added

- **Files, up to 10 MB.** A file is sent as an announcement plus its bytes in
  96 KiB chunks, each an ordinary ratcheted, sealed message — so a relay cannot
  tell a file from a sentence, and files inherit the same encryption, routing and
  retries as text. The receiver reassembles, checks a blake3 hash against the
  announcement, and writes the result under the app data directory. A file that
  fails its hash is discarded rather than written.
- **An attach button in the composer.** The picker runs inside the page rather
  than as a native dialog, so the application's capability list stays empty: no
  filesystem plugin, and no permission to read a path it was not handed. A
  received file shows its name, size, arrival progress, and a button that copies
  its path — Vega does not offer to open what a contact sent it.
- **Images appear in the conversation**, and open full size inside the app when
  clicked. What counts as an image is decided from the file's first bytes, never
  from its name, and SVG is deliberately not on the list — it is a scriptable
  document rather than a picture. Anything over 4 MB waits to be asked for
  rather than loading itself. Nothing is handed to the operating system: the
  bytes come through Rust, so the web view still has no filesystem access.
- **Right-click menus.** On a contact: rename, copy account id, clear this chat.
  On a message: copy its text, its file path, or the time it was sent.
- **A reload button beside the name.** The interface is event-driven, but an
  event can be missed and nothing on screen distinguishes "quiet" from "stale".
- **Local names for contacts and for this device.** Both are stored here and
  never sent: an account id is what identifies someone on the wire, and the
  device label inside the sigchain is signed and cannot be edited after the
  fact. Rename from the right-click menu or the pen beside either name.
- **A stored-history screen**, listing every conversation with what it holds,
  and clearing one or all of them. Clearing takes the messages and the files
  that came with them.
- **A note under your account id** that both ends must add each other before
  either can send. It is the thing people get wrong first.
- **Automatic tagging.** A version bump landing on `main` is now tagged and built
  into a draft release without anyone running a command. See
  [tag.yml](.github/workflows/tag.yml) and
  [releasing.md](.documentation/releasing.md).
- **`./make build`** — compile both halves without lints, tests or bundling.

### Changed

- **The wire format gained two body types**, `file` and `file_chunk`. This breaks
  compatibility with 1.0.0: a 1.0.0 build receiving a file message cannot parse
  it. Text messages between the two versions are unaffected.
- **CI checks that the three version files agree** on every push, rather than
  only when a release is being cut.
- **The content security policy now allows `data:` images**, which is what lets
  a received picture be displayed. It permits no scripts, no styles and no
  network of any kind — `img-src 'self' data:` is the whole of the change.
- **A new install calls itself "some device"** rather than "this device". The
  label is only ever read next to others, and a list where every entry says
  *this* device names nothing.
- **The project is attributed to "The Vega contributors"** in the licence, the
  crate metadata, the bundle publisher and the citation file, rather than to a
  named individual.

### Known limitations

- **A file does not appear on the sender's other devices.** Text is copied to
  them; a file is not, because a self-copy would double a ten-megabyte send.
- **A file larger than about 3 MB needs both people online at once.** A mailbox
  holds 32 envelopes for one recipient, and the rest waits in the sender's outbox
  until the recipient reappears. Those caps were deliberately not raised for
  files: every parked envelope is a volunteer's disk.

## [1.0.0] — 2026-08-16

A packaging and distribution release. The protocol, crypto and transport code
are unchanged from 0.1.0; what changed is that there is now something to
download and instructions for using it.

**Nothing here has been audited.** That was true at 0.1.0 and it is still true —
read [SECURITY.md](SECURITY.md) and the
[threat model](.documentation/threat-model.md) before trusting this with
anything that matters. A 1.0 version number is not a review.

### Added

- **Installers for Linux, macOS and Windows**, attached to every tagged release
  under names that do not change between versions, so a download link written
  once keeps working: `Vega-linux-x86_64.AppImage`, `Vega-linux-amd64.deb`,
  `Vega-macos-universal.dmg`, `Vega-windows-x86_64-setup.exe`. The macOS build
  is universal, so there is one download for Intel and Apple silicon.
- **`SHA256SUMS.txt`** on each release, covering every artifact. Neither the
  macOS nor the Windows build is signed, so a checksum you check against a
  second source is worth more than the warning the operating system shows.
- **[Installation instructions](.documentation/installing.md)** — per-platform
  steps, how to get past the Gatekeeper and SmartScreen warnings, where the
  account data lives, and the fact that deleting that directory destroys the
  account with no recovery.
- **A complete icon set.** Only a single PNG was present, so the Windows and
  macOS bundles could not have been built at all — those bundlers require
  `.ico` and `.icns` respectively.
- **Bundle metadata**: category, publisher, homepage, licence and descriptions,
  which is what makes the `.deb` declare its WebKit and GTK dependencies so
  `apt` resolves them.

### Changed

- **`./make dist` stages what it builds** into `release/`, under the same names
  the release workflow uploads and with a `SHA256SUMS.txt` beside them, so a
  locally built installer and a released one are interchangeable.
- **The release workflow creates its draft release once**, in a job of its own,
  before any platform build starts. Previously all three build jobs raced to
  create it, which duplicates the release or fails two of the three. It also now
  verifies the tag against every file carrying the version before spending three
  runners on a build whose file names would contradict it.

### Fixed

- **A development build launched on its own now says so.** Running
  `cargo run`, or the binary in `target/debug/`, starts the Rust half correctly
  and then opens a window on "Could not connect to localhost", because a dev
  build loads its frontend from the vite dev server rather than containing it.
  The port is now checked at startup and the reason printed, with the two
  commands that do work. Release builds embed the frontend and are unaffected.
- **The nightly `rustdoc` and `unused-deps` jobs** never ran: both compile the
  desktop crate, which links against the system webview, and neither installed
  it. They failed in `pkg-config` before reaching the thing they were meant to
  check.

## [0.1.0] — 2026-08-15

First release. Nothing here has been audited; read
[SECURITY.md](SECURITY.md) before trusting it with anything that matters.

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
- **Delivery receipts.** A recipient confirms decryption, which clears the
  sender's outbox and lets a mailbox drop its copy. Acceptance by a peer only
  ever meant "it took the bytes" — on a LAN that peer may not even have been the
  recipient.
- **Platform keystore.** The database key goes to Secret Service, Keychain or
  Credential Manager, falling back to a 0600 file where no keyring exists
  (headless, container, Android). An existing file key is migrated into the
  keyring and removed only after the keyring is read back and confirmed.

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

### Performance

- **Reading a conversation no longer scans every stored message.** A multimap
  index makes the cost proportional to that conversation rather than to the
  whole database.

### Known limitations

Stated plainly rather than omitted; the README carries the current list.

- **Nothing has been audited.** vodozemac has; the code around it has not.
- **Device linking is not implemented** — an account is one device today. The
  crypto is there and the sigchain accepts device-signed additions, but there is
  no pairing flow, and shipping a rushed one would be worse than shipping none.
- **Android cross-compiles** but the foreground service and multicast lock
  plugins are not written, so background delivery does not work.
- **Tiers 1 and 3 are unproven in the field.** Hole punching and relaying cannot
  be exercised on loopback; they need two machines on two real networks.
- **No keyring means the file fallback**, which protects against another user
  and a stolen backup and nothing else. The app logs which one it used.
