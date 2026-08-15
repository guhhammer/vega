# 0008 — Pinned toolchain, lints in the manifest

`build: pin the toolchain and enforce lints in the manifest`

## What landed

`rust-toolchain.toml`, `[workspace.lints]`, the `Debug` implementations those
lints demanded, and `./make clean`.

## Pinning the toolchain

`channel = "1.97.0"`, with the Android targets listed so a fresh checkout can
cross-compile without a separate `rustup target add`.

The reason is not reproducibility alone. A floating `stable` invalidates the CI
dependency cache every six weeks when a new rustc lands, and for a tree with
several hundred crates that is the difference between a two-minute job and a
twenty-minute one. Bumping it is a deliberate act, in its own commit.

## Lints in Cargo.toml, not RUSTFLAGS

`RUSTFLAGS` does not reach rustdoc or rust-analyzer, so an editor disagreeing
with CI about what is a warning is a good way to waste an afternoon.
`[workspace.lints]` applies everywhere the same way.

Four lints, each chosen for a reason rather than for tidiness:

- **`unsafe_code = "forbid"`.** There is no `unsafe` in this workspace and no
  plausible reason for any. `forbid` rather than `deny` means a future
  contributor cannot re-enable it with a local attribute. For a codebase that
  handles other people's key material, being able to say this and have it
  checked is worth more than it costs.
- **`missing_debug_implementations`.** Found real gaps — see below.
- **`todo` and `dbg_macro`, denied.** Both mean "unfinished". Neither belongs in
  something people run.
- **`cast_possible_truncation`, warned.** Silently truncating an integer in code
  that computes buffer sizes and key lengths is how memory bugs get written in
  safe Rust.

## What the Debug lint actually found

Thirteen public types with no `Debug`, which is an ordinary API annoyance — but
deciding what each should print turned out to be the interesting part, because
several of them hold secrets.

Three kinds of answer:

- **Plain data** — the view structs, `Writer`, `Recipient` — got a derive.
- **Types holding key material** got a manual implementation that redacts.
  `Peered` prints who it talks to and the session id, never the ratchet state.
  `IdentityPickle` prints the account id and label, and `finish_non_exhaustive`
  makes the omission visible rather than looking complete. `Store` prints
  `Store(<opaque>)`, because everything inside is either encrypted or somebody's
  message history.
- **Types that would be noise** got a summary. `Node` cannot derive it —
  `Swarm` has no `Debug` — and printing every in-flight query would not help
  anyone; counts of pending requests and queries are what a stuck node looks
  like. `Vega` prints which optional behaviours are enabled.

That is the value of the lint: not that thirteen types gained a trait, but that
somebody had to decide, for each one, what is safe to put in a log.

## ./make clean

`target/` reached 19 GB during this work — debug, test, release and the Tauri
bundle each keep their own copy of ~400 dependencies. That is worth knowing
before it fills a disk, so `clean` prints the size it is about to remove.
