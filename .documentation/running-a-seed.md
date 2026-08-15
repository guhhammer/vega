# Running a seed

A seed is an ordinary Vega node with no user attached. It does three jobs:

- **Bootstrap** — the entry point that makes the DHT joinable. A client with no
  cached peers and nobody on its LAN has nowhere to start without one.
- **Relay** — forwards opaque ciphertext for peers that cannot reach each other
  directly. This is what makes carrier NAT survivable.
- **Mailbox** — holds encrypted blobs for recipients who are offline.

It holds no key that can read anything, and it cannot withhold a message it
never sees. Running one is the single most useful contribution to this project,
and it costs a port and some bandwidth.

## Starting one

```bash
./make node -- --port 15000
```

It prints its addresses on startup:

```
listening: /ip4/203.0.113.10/udp/15000/quic-v1/p2p/12D3KooWAbc…
listening: /ip6/2001:db8::1/udp/15000/quic-v1/p2p/12D3KooWAbc…
```

The `/p2p/<id>` suffix is part of the address — a client needs it to know it
reached the right node rather than whoever answers on that port.

To join an existing network rather than start one:

```bash
./make node -- --port 15000 --bootstrap /ip4/198.51.100.5/udp/15000/quic-v1/p2p/12D3KooW…
```

## Pointing clients at it

Create `seeds.json` in the app's data directory — a plain JSON array:

```json
[
  "/ip4/203.0.113.10/udp/15000/quic-v1/p2p/12D3KooWAbc…",
  "/ip6/2001:db8::1/udp/15000/quic-v1/p2p/12D3KooWAbc…"
]
```

| Platform | Location |
|---|---|
| Linux | `~/.local/share/dev.guhhammer.vega/seeds.json` |
| macOS | `~/Library/Application Support/dev.guhhammer.vega/seeds.json` |
| Windows | `%APPDATA%\dev.guhhammer.vega\seeds.json` |

Restart the app. No rebuild. A missing or malformed file means LAN-only
operation, which is a perfectly good way to run Vega — it is a debug log, not an
error.

**List both IPv4 and IPv6.** Many mobile carriers hand out a globally routable
v6 address, which sidesteps NAT entirely and is the cheapest connection on the
whole ladder.

## What it needs

- **A public address.** Behind NAT it can still join the DHT but cannot accept
  reservations, which is most of the point.
- **One open port**, UDP and TCP on the same number. QUIC is preferred; TCP is
  the fallback for networks that block or throttle UDP.
- **Very little else.** Idle memory is a few tens of megabytes. The mailbox is
  bounded at 4096 envelopes of at most 256 KiB, so roughly 1 GiB worst case,
  held in memory and never written to disk.

```bash
sudo ufw allow 15000/udp
sudo ufw allow 15000/tcp
```

## As a service

```ini
# /etc/systemd/system/vega-seed.service
[Unit]
Description=Vega seed node
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=vega
ExecStart=/opt/vega/seed --port 15000
Restart=on-failure
RestartSec=10
Environment=RUST_LOG=seed=info,vega_net=warn

# It stores nothing, holds no secrets worth stealing, and needs no filesystem
# access beyond its own binary. Lock it down accordingly.
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=
MemoryMax=2G

[Install]
WantedBy=multi-user.target
```

Build the binary with `cargo build --release -p vega-net --example seed`.

## What a seed can and cannot see

**Can:** that a peer with some ephemeral `PeerId` connected, from which address,
when. Rendezvous records as opaque blobs under opaque keys. Routing tags, sizes,
and timings of what it relays or holds.

**Cannot:** read a message, tell who is talking to whom, link a peer to an
account, find whose rendezvous records it stores, or collect the mail it holds —
that needs a token derived from a pairwise secret it has never seen.

The `PeerId` it sees is a fresh keypair generated at the client's startup and is
deliberately not derived from the account key, so it cannot be used to recognise
a returning user.

## Honestly, the risks of running one

- **Your IP is public.** It is a bootstrap address; that is the job.
- **You forward strangers' traffic.** Opaque, but it is your bandwidth and your
  address in someone's connection logs. Consider whether that is comfortable
  where you live.
- **Abuse complaints.** You cannot inspect what you relay, which also means you
  cannot demonstrate what it was. Circuit Relay v2 applies per-reservation
  duration and data limits, but the exposure is real.
- **The mailbox is in memory**, so a restart drops what it held. Deliberate: a
  relay that never writes ciphertext to disk cannot be compelled to produce it
  later, and cannot leak it through a stolen backup.

## Operating notes

Seeds are peers. There is no coordination protocol, no registry, and no way for
one to have authority over another. Run several in different places if you can —
blocking all of them is the only way to stop new nodes joining, and that gets
harder the more there are.

`RUST_LOG=vega_net=debug` shows connections and reservations. It logs peer ids
and addresses; treat those logs as sensitive to the people connecting to you,
and prefer not to keep them.
