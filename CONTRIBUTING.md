# Contributing

## Before anything else

```bash
./make check
```

That runs formatting, clippy at `-D warnings`, the whole test suite, and the
TypeScript typecheck. It is the same gate CI runs, so if it passes locally it
passes there. Run it constantly; it is fast after the first build.

## Getting set up

Debian or Ubuntu:

```bash
sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
```

Then `./make dev` to run the app.

The first Rust build takes a while — libp2p is several hundred crates. Later
builds are much faster; `.cargo/config.toml` explains what is tuned and what is
deliberately left alone.

## Where things live

```
crates/vega-core   identity, sigchain, message crypto — no networking at all
crates/vega-net    the transport ladder — moves opaque bytes, cannot read them
app/src-tauri      the shell: commands, runtime, key storage
app/src            the interface
```

That split is load-bearing rather than cosmetic. `vega-net` handles envelopes it
cannot open, which is what makes it safe for a stranger's node to relay them. A
change that gives `vega-net` the ability to read a message is a change to the
threat model, not a refactor.

## What makes a change easy to accept

**Say what you are trading away.** Every design note in this repo names its
cost. "Faster" is not a rationale; "faster, at the cost of one more party
learning that you are online" is.

**Explain in the code, not the pull request.** A comment explaining *why* a
check exists survives; a PR description does not. Comments here answer why —
what the code does should be readable from the code.

**Bring a test that would have failed.** For a bug, write it first and watch it
fail. For anything touching crypto or authentication, write the test as an
attacker: `a_sender_cannot_claim_someone_elses_account` is worth more than
`test_decrypt_works`.

**Keep the diff about one thing.** Formatting churn mixed with a logic change
makes the logic change unreviewable.

## Cryptography and networking

Changes under `crates/vega-core` or `crates/vega-net` get read more slowly, and
some things will be pushed back on by default:

- **No new primitives.** If a construction is not already in the stack, it needs
  a strong reason and a reference to something well-analysed.
- **Nothing parses untrusted bytes with `unwrap`.** A panic reachable from the
  network is a remote crash. There are currently none.
- **Anything a peer sends is untrusted**, including from a contact. Sender
  fields are claims until the sigchain confirms them.
- **New network-visible fields need a privacy argument.** Everything a relay can
  see is metadata someone can collect.

If you find a vulnerability, do not open a pull request that quietly fixes it.
See [SECURITY.md](SECURITY.md).

## Commits

The repo keeps a written record: every commit gets a short markdown note in
[`done/`](done/) explaining what changed and why. It is not a changelog — it is
the reasoning that would otherwise be lost. Follow the existing files.

Conventional-ish subjects, imperative mood:

```
feat(core): authenticate the claimed sender against the sigchain
fix(net): reject collect requests without a valid token
docs: record the prekey exhaustion trade-off
```

## Running two nodes

The fastest way to see it work:

```bash
./make dev          # on one machine
./make dev          # on another, same wifi
```

They find each other over mDNS. Copy the invite from one, paste it into the
other. For testing across networks, run a seed with `./make node` and put its
printed address in `seeds.json` in the app's data directory.
