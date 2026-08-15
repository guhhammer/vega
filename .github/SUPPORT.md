# Getting help

## Something is broken

Open a [bug report](https://github.com/guhhammer/vega/issues/new?template=bug_report.yml).
Include the output of `./make check` if it fails, and whether the problem shows
up on one machine or between two.

**Do not report security problems as issues.** See [SECURITY.md](../SECURITY.md).

## It will not build

Almost always a missing system dependency. On Debian or Ubuntu:

```bash
sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
```

## Two machines cannot see each other

Work down the ladder:

1. **Same wifi?** They should find each other over mDNS with no configuration.
   Some networks — most guest and corporate wifi — block multicast between
   clients. That is the network, not Vega.
2. **Different networks?** You need a seed node. Run one with `./make node`,
   then put its printed address in `seeds.json` in the app's data directory.
   Without a seed there is nothing to bootstrap the DHT from.
3. **Behind carrier NAT (4G)?** Expect the relay tier. That needs a seed that is
   itself publicly reachable.

`RUST_LOG=vega_net=debug ./make dev` shows which tier is being attempted.

## Understanding the design

Start with the [README](../README.md), then
[the design document](../.documentation/design.md). It says what the system does
*not* protect against as clearly as what it does; read that part before trusting
it with anything.

## Asking for a feature

Open a [feature request](https://github.com/guhhammer/vega/issues/new?template=feature_request.yml).
The useful ones say what you were trying to do, not just what to build.
