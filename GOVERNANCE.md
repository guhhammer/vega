# Governance

## Who decides

One maintainer, [@guhhammer](https://github.com/guhhammer), decides. This is a
small project and pretending otherwise would be theatre.

What that means in practice: you will get a direct answer rather than a process,
and occasionally that answer is "no, and here is why". It also means the bus
factor is one, which is a real risk and is written down rather than hidden.

## How a change gets in

1. Open an issue or a draft PR before doing large work. A design that has to be
   unwound is worse for you than a conversation that takes a day.
2. `./make check` passes.
3. The change names its trade-off. Every design note in this repo does.
4. A test exists that would have failed before.

Small fixes can skip step 1. Anything touching `crates/vega-core`,
`crates/vega-net`, or the wire format cannot.

## What gets refused

Some things are refused on principle rather than on merit, so that nobody spends
a weekend on them first:

- **Anything that requires a server to be running.** Not a relay, not a
  bootstrap seed — those are peers anyone can run and none of them can read a
  message. A component that must exist for the system to work, and that only one
  party can operate, defeats the entire premise.
- **Anything that lets an intermediary learn who is talking to whom.** Content
  encryption is the easy half. The metadata is what this project is actually
  about.
- **Telemetry, analytics, crash reporting that phones home.** No exceptions, not
  even opt-in. A messenger that reports on its users is a different product.
- **New cryptographic primitives.** If a construction is not already in the
  stack, it needs a strong reason and a reference to something well-analysed.
- **Convenience that quietly weakens a guarantee.** Cloud backup of message
  history, key escrow, "just log in with your email". Each is defensible in a
  centralised messenger and none of them are available here.

If your idea is in one of these categories and you think there is a version that
holds the line, say so — the categories describe defaults, not walls.

## Decisions that are already made

Recorded in [`done/`](done/) rather than relitigated. Each file says what was
chosen, what was rejected, and what it cost. Reopening one is fine if there is
new information; reopening one because the trade-off is inconvenient is not.

## If the maintainer disappears

The MIT licence means anyone can fork and continue. If that happens, the useful
things to carry forward are `done/` and `.documentation/design.md` — the code is
replaceable, the reasoning is what took the time.

Anyone who has landed substantial work and wants commit access should ask. The
answer is likely yes; the reason it has not been offered is that nobody has yet.

## Releases

Tagged from `main` when there is something worth tagging. No schedule, no
long-term support branch, and — pre-1.0 — no promise that the wire format is
stable between them. See [SECURITY.md](SECURITY.md) for which versions get
fixes.
