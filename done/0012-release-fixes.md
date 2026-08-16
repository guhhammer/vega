# 0012 — The rest of the known weaknesses

`feat: platform keystore, delivery receipts, conversation index`

Three of the five items on the README's known-weaknesses list, closed. The other
two are honestly out of reach here, and saying which is part of the work.

## Platform keystore

The database key now goes to the OS credential store — Secret Service on Linux,
Keychain on macOS, Credential Manager on Windows — via the `keyring` crate,
declared per target so Android does not try to pull in D-Bus.

**With a fallback, because a keyring is not always there.** A headless seed, a
container, an SSH session with no D-Bus: all of those have to keep working. The
fallback is the previous 0600 file, and the app logs at startup which backing it
got. A weaker mode that announces itself is much better than one that does not.

The migration is the part worth care. An existing file key is written to the
keyring, **read back, and compared** before the file is deleted. A store that
accepts a write but cannot return it would otherwise cost the user every message
they have. If the read-back disagrees, the file stays and the log says so.

The keyring path is deliberately not unit-tested: a test that writes to the
developer's real credential store leaves litter behind and fails on any machine
without a session keyring. The file path and the public entry point are tested.

## Delivery receipts

`Body::Receipt` was handled on arrival and never sent, so the outbox cleared
when *a peer accepted the bytes*. On a LAN, delivery is a broadcast — that peer
may not have been the recipient at all. "Accepted" meant almost nothing.

Now the recipient sends a receipt after successfully decrypting, and the sender
clears every outbox entry carrying that message id. One message fans out to
several devices, so several entries carry it; `dequeue_message` clears them all,
and there is a test for exactly that.

The obvious hazard is a receipt for a receipt, forever. Two things stop it: only
non-outgoing messages trigger one, and a receipt carries no text for the handler
to act on. There is an integration test asserting the loop terminates after one
round, because "obviously it terminates" is how infinite loops get written.

Receipts are encrypted and fanned out like any other message, so they reveal no
more to the network than what they acknowledge. No self-copy — my other devices
do not need to know I read something.

## Conversation index

`conversation()` read every message ever stored and filtered. Correct, and
O(total) per call, on a path the UI hits on every incoming message.

A redb multimap keyed by conversation now holds the sequence numbers, so the
cost is proportional to the conversation. Sequence numbers are allocated
monotonically, so the multimap's ordering is already chronological and the tail
is the recent end — no sorting needed.

A missing message behind an index entry is skipped rather than fatal. That
should not happen; a corrupt index taking down a whole thread would be a bad way
to find out.

## What is not fixed, and why

**Device linking.** The crypto is all there — `Identity::adopt` exists, and the
sigchain already accepts a device-signed `AddDevice`. What is missing is the
pairing flow: SPAKE2 over a short code, and a UI for it.

Not attempted here on purpose. A pairing protocol is security-critical, it is
the one place where a low-entropy secret guards a high-value transfer, and the
failure mode is silent. Shipping a rushed one would be worse than shipping none,
and "an account is one device" is at least a limitation a user can understand.

**Android background delivery.** Needs Kotlin, a foreground service, a multicast
lock, and a physical device to test on. `.documentation/android.md` says exactly
what each piece must do.
