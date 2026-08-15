# Testing

```bash
./make check    # fmt, clippy at -D warnings, all tests, tsc
./make test     # tests only
```

94 tests. They run in about half a second, because `vega-core` has no network
and no async runtime to wait on.

## What is where

| | Tests | Proves |
|---|---|---|
| `crates/vega-core` | 59 | Identity, chain validation, sealed sender, sessions, storage |
| `crates/vega-net` | 21 | Mailbox limits, rendezvous records, address parsing |
| `crates/vega-net/tests/two_nodes.rs` | 5 | Two real nodes, real sockets, real ciphertext |
| `app/src-tauri` | 9 | Invite encoding and verification, key storage |

## The integration tests matter most

Everything in `two_nodes.rs` spawns actual libp2p nodes on loopback QUIC with
mDNS turned off, so results are deterministic rather than dependent on whatever
else is on the tester's network.

They prove things the unit tests structurally cannot:

- an envelope crosses the wire intact;
- two accounts exchange an encrypted message and **the account id never appears
  in the wire bytes**;
- a third party on the same network can neither decrypt nor recognise the tag;
- parked mail is collected exactly once, and not by someone holding only the
  routing tag;
- a rendezvous record round-trips through the DHT and is unreadable without the
  pairwise key.

This is not theoretical. The park/collect test caught a hand-written tag
serializer that wrote CBOR byte strings and read back arrays — every park and
collect failed over the wire, and no unit test could see it because none of them
crossed a codec.

## Write tests as the attacker

For anything touching crypto or authentication, the useful test is the one that
tries to break it. Compare:

```rust
#[test] fn test_decrypt_works()                      // proves almost nothing
#[test] fn a_sender_cannot_claim_someone_elses_account()  // proves the thing
```

The second name is also the specification. When it fails, you know what broke
without reading the body.

The existing ones worth copying the shape of:

| Test | Attack |
|---|---|
| `a_sender_cannot_claim_someone_elses_account` | Impersonation with a legitimately-obtained session |
| `a_refused_message_does_not_leave_a_session_behind` | Poisoning the session table with a rejected message |
| `a_stranger_cannot_add_a_device` | Forging a sigchain entry |
| `tampering_with_history_breaks_the_chain` | Rewriting a past entry |
| `rewriting_the_routing_tag_is_detected` | A relay redirecting a message |
| `the_wrong_token_collects_nothing_and_destroys_nothing` | Stealing mail with a tag seen on the wire |
| `truncated_input_is_rejected_without_panicking` | Every truncation length, checking for panics |
| `arbitrary_dht_bytes_do_not_panic` | Attacker-chosen bytes from the DHT |

## Rules that are actually enforced

**Nothing panics on untrusted input.** There are no `unwrap`, `expect`, `panic!`
or indexing panics reachable from network data anywhere in the workspace; the
remaining `expect`s are on compile-time constants. A panic reachable from a
socket is a remote crash. When you add a parser, add the truncation test.

**No `unsafe`.** `unsafe_code = "forbid"` in the workspace lints. There is none
and there is no plausible reason for any.

**Clippy at `-D warnings`.** A warning in crypto or transport code is a defect
until someone has looked at it.

## Writing a test that needs two peers

`vega-core` has a `Peer` helper in `session.rs` that bootstraps an account,
registers a device and publishes prekeys. `TestDirectory` stands in for the
app's contact store — it is what tells `decrypt` which devices speak for an
account.

Note that a `TestDirectory` which does **not** contain the sender's chain is a
valid test: it proves a message from someone you have never added is refused.

For network tests, `two_nodes.rs` has `isolated()` for a node that only talks to
what it is told to, `wait_for` for events with a timeout, and `connect` to wire
two nodes together.

## What the suite does not prove

Worth knowing before trusting a green run:

- **Two machines on two real networks.** Hole punching and relay cannot be
  exercised on loopback. This is the largest untested area and the most valuable
  thing anyone could contribute.
- **The GUI.** Nothing drives the frontend. `tsc --noEmit` proves it compiles.
- **Android.** It cross-compiles; nothing runs it.
- **Long-running behaviour.** No test covers key exhaustion over weeks, chain
  growth, or a database that has been in use for a year.
- **Concurrency under load.** The tests are single-threaded and cooperative. The
  event pump has not been tested against a peer that stalls deliberately.

## Debugging a failure

```bash
cargo test -p vega-core session::tests::two_strangers -- --nocapture
RUST_BACKTRACE=1 cargo test …
RUST_LOG=vega_net=debug cargo test --test two_nodes -- --nocapture
```

Integration tests have a 20-second patience; a genuine hang shows up as a
timeout naming what it was waiting for rather than as a stuck run.
