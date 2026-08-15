# 0004 — The desktop application

`feat(app): tauri desktop application`

Tauri v2, React frontend, Rust backend. 9 tests.

## What the app is responsible for

`vega-core` knows crypto and nothing about networks. `vega-net` moves bytes it
cannot read. `runtime.rs` is the only place that knows both, and it owns the
decisions that need both: which peer to hand an envelope to, what to do when
that fails, and when a received envelope becomes a message on screen.

## Invites are the trust root

An invite carries an account id, a contact key, and the full signed device
chain, so the recipient verifies the roster offline rather than taking the
invite's word for it. Decoding checks that the chain validates, that its account
id matches the claim, that its contact key matches, and that it lists at least
one device — all before it can become a contact.

This is deliberately manual and deliberately out of band. Anyone who can
substitute an invite in transit becomes the conversation, which is what safety
numbers exist to catch. The UI says so in the invite sheet rather than burying
it in documentation.

## The runtime performs no network I/O

Every method is synchronous and returns a *plan* — deliveries to attempt,
records to publish, lookups to make. The caller runs those without holding the
lock, then reports back.

This started as a correctness fix (see 0005) and turned out to be the better
shape anyway: the runtime became testable without a network, and the lock is now
held only for as long as it takes to touch the database.

## Delivery on a LAN is a broadcast

We do not know which connected peer is the recipient — that is the point of
sealed sender. So an envelope goes to every connected peer and only the right
device can open it; the rest fail to unseal and drop it, learning nothing. Once
a rendezvous lookup has told us a peer id, delivery is targeted instead.

Failing to decrypt is therefore the *common* case, not an error, and the code
says so where it happens. A message that fails sender authentication lands in
the same silent path — a forged message deserves no more attention than noise.

## Seeds come from a file

`seeds.json` in the app data directory, a plain array of multiaddrs. Adding a
seed does not need a rebuild, which matters because the seed list is the one
piece of this design that is even slightly centralised and people should be able
to change theirs without a toolchain.

A missing or malformed file means LAN-only operation, which is a perfectly good
way to run this — so it is a debug log, not an error.

## Key storage is the weak point, and it is labelled

`keystore.rs` writes a 0600 file next to the database. That protects against
another user on the machine and against a stolen backup, and against nothing
else — anything running as this user can read it.

The key belongs in the platform keystore, which needs a plugin per platform. The
interface here is two functions wide precisely so swapping it touches nothing
else. A truncated key file is an error rather than a silent regeneration, since
silently making a new key would render every stored message undecryptable while
looking like a successful start.

## The frontend

Plain CSS with tokens, dark-first because a messenger is used at night and a
bright panel in a dark room is a hostile default. The light theme is a full
palette rather than an inversion.

No component library. The app is a sidebar, a thread, and a composer; pulling in
forty Radix packages to draw three things would be more code to audit for
something that displays decrypted plaintext.

Errors surface as plain strings — the UI has no use for the Rust error type, and
leaking internals into a web view is a bad habit.

## The opener plugin was removed

It was registered and never called. An unused plugin is unused attack surface,
and the capability grant went with it.
