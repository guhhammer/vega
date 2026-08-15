# 0010 — The nightly workflow, and documenting the set

`ci: add the nightly workflow and document the set`

## What landed

`.github/workflows/nightly.yml`, `.github/workflows/README.md`, and least-
privilege `permissions` on `ci.yml`.

## Four workflows, four different questions

The split is by *when you need the answer*, not by subject:

| | Question | So |
|---|---|---|
| `ci` | Can this merge? | Must be fast. It is exactly `./make check`. |
| `audit` | Has a dependency become dangerous? | On a clock as well as on change. |
| `nightly` | Is anything else broken? | Tomorrow morning is soon enough. |
| `release` | Is this shippable? | Thorough, and slow is fine. |

`ci` matching `./make check` exactly is the important one. A green local run
meaning a green CI run is the only way a pre-commit check actually gets used;
the moment they diverge, people stop trusting the local one.

## What nightly covers

- **Android cross-compile.** Proves the Rust half still builds for a phone. It
  takes minutes and breaks for reasons unrelated to whatever change is in front
  of you, which is precisely why it does not gate a pull request.
- **rustdoc with `-D warnings`.** Broken intra-doc links are how documentation
  rots without anyone noticing.
- **A release-profile build and test run.** LTO and `codegen-units = 1`
  occasionally surface what a debug build hides.
- **`cargo udeps`,** non-blocking. A dependency nobody uses is still a
  dependency somebody has to trust. It needs nightly rustc and produces false
  positives, so it reports rather than fails.

## Why the workflows have their own README

Four YAML files with overlapping triggers are hard to reason about, and the
question "where do I add this check?" has a right answer that is not obvious
from reading them. The file gives the rule: if it must pass before a merge it
goes in `ci`, if it is informational it goes in `nightly`, and a new file is for
a genuinely different trigger rather than a different name.

That is the kind of thing that is obvious to whoever wrote it and opaque to
everyone else six months later.
