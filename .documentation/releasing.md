# Releasing

## Checklist

```bash
./make clean && ./make check    # from cold, so no cached lint result hides
```

Then:

1. **Version.** `version` in the workspace `Cargo.toml`, and `app/package.json`
   and `app/src-tauri/tauri.conf.json`, all agree.
2. **CHANGELOG.** `## [Unreleased]` becomes `## [x.y.z] — YYYY-MM-DD`. The
   *Known limitations* section is current — that is the part a user reads before
   deciding whether to trust this, and a stale one is worse than none.
3. **README.** The "what is built" table matches reality.
4. **`done/`** has a file for every commit since the last tag.
5. **Tag.** `git tag -a vX.Y.Z -m "..."` and push it. `release.yml` builds
   bundles for Linux, macOS and Windows as a **draft** release.
6. **Draft.** Read what the workflow produced before publishing. It re-runs the
   tests, but a green build is not the same as a release worth shipping.

## Version numbers

Semver, with a pre-1.0 caveat that matters more than usual here: **the wire
format is not stable**. Two different 0.x builds may not talk to each other, and
the changelog says so at the top rather than in a footnote.

Anything that changes the canonical encoding, the sealed-box construction, a
key derivation, or the sigchain entry format breaks compatibility with stored
data as well as with peers. `0011` did exactly that. Until 1.0 those go in a
minor bump with a loud changelog entry; after 1.0 they need a migration.

## What a release is not

It is not a claim that the code is audited, and the release body says so. It is
also not a promise of support for the previous tag — only `main` gets fixes, and
[SECURITY.md](../SECURITY.md) states that plainly.

## Before 1.0

1.0 should mean the wire format is stable and someone other than the author has
reviewed the cryptography. Neither is true, and neither is close. In the
meantime the honest position is a 0.x that says what it does not do — which is
what [ROADMAP.md](../ROADMAP.md) and the changelog's limitations section are for.
