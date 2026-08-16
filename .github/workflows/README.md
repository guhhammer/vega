# Workflows

| Workflow | Runs on | Does |
|---|---|---|
| [`ci.yml`](ci.yml) | push to `main`, every PR | fmt, clippy, tests, frontend build |
| [`audit.yml`](audit.yml) | dependency changes, weekly | RustSec and npm advisories |
| [`nightly.yml`](nightly.yml) | nightly, manual | Android cross-compile, docs, release-profile build |
| [`release.yml`](release.yml) | `v*` tags, manual | Installers for Linux, macOS and Windows on a draft release, plus checksums |

## Why they are split this way

**`ci.yml` is the gate and has to stay fast.** It is exactly what `./make check`
runs locally, so a green local run means a green CI run — which is the only way
a pre-commit check actually gets used. Nothing slow belongs here.

**`audit.yml` runs on a clock as well as on change.** An advisory published on a
Tuesday should not wait for the next unrelated commit to be noticed. That is the
whole reason it is not folded into `ci.yml`.

**`nightly.yml` holds everything worth knowing but not worth blocking a PR
for.** An Android cross-compile takes minutes and breaks for reasons unrelated
to the change in front of you; finding out the next morning is soon enough.

**`release.yml` re-runs the tests** even though the tagged commit already passed
them. Shipping a messenger that fails its own authentication tests would be
worse than shipping late.

It is also the one workflow with four jobs rather than one, and the shape is
deliberate. The tag is checked against the version in the tree first, so a
mismatch costs seconds instead of three platform builds. The release is then
created **once**, by a job of its own — letting the three build jobs each
create-if-missing is a race, since they start together, all see no release, and
two of them fail or duplicate it. Checksums come last, because they can only
cover files that have finished uploading. See
[`../../.documentation/releasing.md`](../../.documentation/releasing.md).

## Conventions

- **Warnings are denied.** `RUSTFLAGS: -D warnings` in `ci.yml`. A warning in
  crypto or transport code is a defect until someone has looked at it.
- **The toolchain is pinned** by `rust-toolchain.toml`, so the dependency cache
  is not invalidated every six weeks by a new stable release.
- **`Swatinem/rust-cache`** everywhere. libp2p is several hundred crates and an
  uncached job is roughly ten times an incremental one.
- **Least privilege.** Jobs declare `permissions: contents: read` unless they
  genuinely need to write.

## Adding one

Ask which of the four buckets it belongs in before adding a fifth workflow. If
it must pass before a merge it goes in `ci.yml`; if it is informational it goes
in `nightly.yml`. A separate file is for something with a genuinely different
trigger, not for something with a different name.
