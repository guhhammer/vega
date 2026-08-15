# 0009 — Filling .documentation

`docs: fill .documentation with as-built references`

## What landed

`README.md` (index), `architecture.md`, `wire-format.md`, `threat-model.md`,
`running-a-seed.md`, `testing.md`. Alongside the existing `design.md` and
`android.md`.

## The split between design and architecture

`design.md` is the draft, kept verbatim: why the system is shaped this way.
`architecture.md` is how the code actually turned out. They will drift, and
that is fine — a design document that gets edited to match the code stops being
a record of anything.

`architecture.md` carries the thing a newcomer most needs and cannot infer:
which boundaries are load-bearing. The `vega-core` / `vega-net` split is a
security property, not tidiness. If they merge, the argument for letting a
stranger relay your traffic quietly stops holding, and nothing fails to compile
to say so. A file that says this out loud is cheaper than rediscovering it.

## wire-format.md exists so the protocol can be reimplemented

Every byte that crosses a network, the exact HKDF inputs, the domain separation
strings, the canonical encoding, and every limit in one table.

It also states, at the point where the fields appear, that `from_account` and
`from_device` are **claims, not facts**. That is the exact misreading that
produced the impersonation break in 0005, and a reference document that lets
someone make it again has failed.

## threat-model.md is organised by adversary

Not by mechanism. "Someone on your local network", "a relay peer", "someone
holding your invite" — because that is how a reader actually asks the question.

Each entry says what the adversary can do, what they cannot, what stops them,
and — the part usually missing — what is **left over**. A relay still knows you
are online. A DHT node can count records and infer an upper bound on your
contact list. Vega is not Tor and traffic analysis works.

There is also a list of cryptographic dependencies with what breaks if each one
does, and five explicit assumptions. A broken assumption invalidates the whole
analysis, so they are written down rather than held in someone's head.

## running-a-seed.md is honest about the cost

A seed is the most useful contribution anyone can make, and the document says
so. It also says your IP becomes public, you forward traffic you cannot inspect,
and abuse complaints are a real possibility — with a note to consider whether
that is comfortable where you live.

Recruiting operators without telling them the downside would be a bad trade for
everyone, including the network.

The systemd unit locks the service down hard, because a seed stores nothing and
needs no filesystem access beyond its own binary.

## testing.md says what the suite does not prove

The list is as important as the pass count: two machines on two real networks,
the GUI, Android, long-running behaviour, and concurrency under load. A green
run means less than it looks like, and a reader deserves to know which parts.

It also argues for writing tests as the attacker.
`a_sender_cannot_claim_someone_elses_account` is worth more than
`test_decrypt_works`, because the name is the specification and a failure tells
you what broke without reading the body.
