# Workflows

Three, and no more. Each one answers a different question, and a workflow that
does not answer a question nobody else does should not exist.

| Workflow | Runs on | Does |
|---|---|---|
| [`ci.yml`](ci.yml) | push to `main`, every PR | fmt, clippy, tests, frontend build |
| [`tag.yml`](tag.yml) | push to `main`, manual | Tags a commit that bumps the version, then calls `release.yml` |
| [`release.yml`](release.yml) | `v*` tags, `tag.yml`, manual | Installers for Linux, macOS and Windows on a draft release, plus checksums |

## Why they are split this way

**`ci.yml` is the gate and has to stay fast.** It is what `./make check` runs
locally, so a green local run means a green CI run — which is the only way a
pre-commit check actually gets used. Nothing slow belongs here. The one thing it
adds is a seconds-long `version` job, so that the three files carrying the
version disagreeing is a red run on the commit that caused it rather than a
surprise at the moment someone is trying to cut a release.

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

- **Warnings are denied**, by `cargo clippy … -- -D warnings` rather than by
  `RUSTFLAGS`. The flag variable applies to every crate cargo builds, several
  hundred dependencies included, and a warning in somebody else's release is
  not a defect in this one. Clippy runs the rustc lints as well, so the
  coverage is the same where it matters.
- **The toolchain is pinned** by `rust-toolchain.toml`, so the dependency cache
  is not invalidated every six weeks by a new stable release.
- **`Swatinem/rust-cache` in `ci.yml`, and nowhere else.** libp2p is several
  hundred crates and an uncached job is roughly ten times an incremental one —
  which is worth it on the workflow that runs constantly, and not worth it on
  the one that decides what ships.
- **Least privilege.** Jobs declare `permissions: contents: read` unless they
  genuinely need to write.

## Adding one

Ask whether it belongs in `ci.yml` first. If it must pass before a merge, that
is where it goes. A separate file is for something with a genuinely different
trigger, not for something with a different name — and a fourth workflow needs
to earn its place against the three above, which between them cover *is this
correct*, *is this a release*, and *build the release*.
