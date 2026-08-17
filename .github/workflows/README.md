# Workflows

| Workflow | Runs on | Does |
|---|---|---|
| [`ci.yml`](ci.yml) | push to `main`, every PR | fmt, clippy, tests, frontend build |
| [`audit.yml`](audit.yml) | dependency changes, weekly | RustSec and npm advisories |
| [`nightly.yml`](nightly.yml) | nightly, manual | Android cross-compile, docs, release-profile build |
| [`tag.yml`](tag.yml) | push to `main`, manual | Tags a commit that bumps the version, then calls `release.yml` |
| [`release.yml`](release.yml) | `v*` tags, `tag.yml`, manual | Installers for Linux, macOS and Windows on a draft release, plus checksums |

## Why they are split this way

**`ci.yml` is the gate and has to stay fast.** It is what `./make check` runs
locally, so a green local run means a green CI run — which is the only way a
pre-commit check actually gets used. Nothing slow belongs here. The one thing it
adds is a seconds-long `version` job, so that the three files carrying the
version disagreeing is a red run on the commit that caused it rather than a
surprise at the moment someone is trying to cut a release.

**`audit.yml` runs on a clock as well as on change.** An advisory published on a
Tuesday should not wait for the next unrelated commit to be noticed. That is the
whole reason it is not folded into `ci.yml`.

**`nightly.yml` holds everything worth knowing but not worth blocking a PR
for.** An Android cross-compile takes minutes and breaks for reasons unrelated
to the change in front of you; finding out the next morning is soon enough.

**`tag.yml` exists so that releasing is a version bump rather than a
command.** It runs on every push to `main` and does nothing at all unless the
version in the tree has no tag yet, which is what makes it safe to leave switched
on: the deliberate act is the bump, not the push. Ten commits at the same version
release nothing; one version change releases exactly once. It has to *call*
`release.yml` rather than let the pushed tag trigger it, because GitHub will not
start a workflow from an event another workflow caused — a loop guard that would
otherwise leave the tag sitting there with nothing built.

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

Ask which of the existing buckets it belongs in before adding another file. If
it must pass before a merge it goes in `ci.yml`; if it is informational it goes
in `nightly.yml`. A separate file is for something with a genuinely different
trigger, not for something with a different name.
