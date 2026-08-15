# 0001 — Workspace, tooling and repository scaffolding

`build: workspace, toolchain and repository scaffolding`

## What landed

A three-crate Cargo workspace, the `make` entry point, `.cargo/config.toml`,
CI, and the documents GitHub expects a project to have.

```
crates/vega-core   identity, sigchain, message crypto — no networking
crates/vega-net    the transport ladder — moves bytes it cannot read
app/               the Tauri desktop application
```

## Why three crates and not one

The boundary between `vega-core` and `vega-net` is a security property, not a
tidiness preference. `vega-net` handles envelopes it has no way to open, and
that is exactly what makes it safe to ask a stranger's node to relay one. If the
two ever merge, the argument for peer relaying quietly stops holding, and
nothing would fail to compile to tell us.

Keeping the crates apart also means the crypto has no async runtime, no sockets,
and no reason to grow either.

## The `make` script

One word per task, cheapest checks first, because a break should surface in
seconds rather than after a full release build. `./make check` is the same gate
CI runs — if it passes locally it passes there, which is the only way a
pre-commit check gets used.

The comment at the top warns that a bare `cargo build` produces binaries that
prove the code compiles but are not applications: without the Tauri CLI there is
no frontend embedded, so the window opens on "could not connect to localhost".
That was worth writing down because it looks like a bug the first time.

## Build speed

`.cargo/config.toml` changes how the code is built, never what is produced.

Two settings do nearly all the work at this dependency count — libp2p alone is
several hundred crates:

- **`debug = "line-tables-only"`.** Full DWARF for ~400 dependencies is the
  single largest cost in a debug build and almost none of it is ever read. Line
  tables still give usable backtraces and panic locations, which is all a test
  run needs.
- **`split-debuginfo = "unpacked"`.** Leaves debug info in the object files
  rather than copying it into the binary at link time.

A `quick` profile was added for running the app at a sensible speed without
paying for fat LTO and a single codegen unit.

Two further wins are written down but left off:

- **A faster linker** (mold) is the next big one, but it must be installed
  first, and a missing linker turns every build into a hard error rather than a
  slow one. Commented, with the apt line.
- **sccache** is installed on this machine but interacts badly with `cargo fix`
  and anything that rewrites source mid-build, which this project does often.

Both are one uncomment away for anyone who wants them.

## CI

Three workflows:

- **`ci.yml`** — fmt, clippy at `-D warnings`, tests, and the frontend build.
  Warnings are denied because this ships a messenger; a warning in crypto or
  transport code is a defect until somebody has looked at it.
- **`audit.yml`** — RustSec and npm advisories, on push *and* weekly. An
  advisory published on a Tuesday should not wait for the next unrelated commit
  to be noticed. The npm side is gated at `high` rather than `moderate`: the
  frontend has no network access of its own and talks only to the Rust side.
- **`release.yml`** — tag-triggered Tauri bundles for Linux, macOS and Windows,
  as a draft release. It re-runs the tests even though the tagged commit already
  passed them, because shipping a messenger that fails its own authentication
  tests would be worse than shipping late.

Dependabot groups the libp2p crates together — it releases ~30 sub-crates in
lockstep and separate PRs would never merge cleanly — and groups the crypto
dependencies so they can be reviewed as a set.

## The community documents

`SECURITY.md` says what is in scope and, at more length, what is not: a global
passive observer, relays learning that you are online, endpoint compromise.
Stating the limits precisely is what separates a security policy from marketing.

`CODE_OF_CONDUCT.md` carries one clause that is not boilerplate: this repository
may not be used to coordinate attacks on people. A tool built so people can talk
privately should not host the planning of harm to anyone.

`CONTRIBUTING.md` asks for the thing this codebase actually runs on — name the
trade-off, explain *why* in a comment rather than in a PR description that will
be lost, and bring a test that would have failed.

## Cargo.lock is committed

It is gitignored in most library projects and should not be here. This repo
builds applications, and a reproducible dependency set is part of what a
messenger's users are trusting.
