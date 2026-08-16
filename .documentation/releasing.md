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

The version check in step 1 is enforced, not trusted: the workflow's first job
compares the tag against all three files and stops in seconds if any disagrees,
rather than after three platform builds have produced installers whose names
contradict the release they hang under.

## What the workflow produces

One runner per platform, because Tauri links against the system webview and
cannot cross-compile. Each runs the same bundle commands as `./make dist`.

Every release carries four installers under names that never change between
versions, so a download link written once keeps working:

| Platform | File |
|---|---|
| Linux (any distribution) | `Vega-linux-x86_64.AppImage` |
| Debian, Ubuntu | `Vega-linux-amd64.deb` |
| macOS (Intel and Apple silicon) | `Vega-macos-universal.dmg` |
| Windows | `Vega-windows-x86_64-setup.exe` |

Tauri's own version-stamped names (`Vega_1.0.0_amd64.deb`) are attached too —
same bytes, and they are what the release page shows. `SHA256SUMS.txt` covers
everything and is computed last, once all three platforms have uploaded.

The release body is this version's CHANGELOG section, so the page says what
changed rather than pointing at a file. A missing section is a warning, not a
failure — failing three platform builds over prose would not be sensible.

Nothing is signed or notarized. macOS and Windows both warn on first open, and
[installing.md](installing.md) tells users how to get past it. Adding
certificates is a matter of secrets and two extra env vars, not a rewrite.

### When a platform fails

`fail-fast` is off and the release is a draft, so one bad runner does not throw
away the installers that did build. Re-run the single failed job; the alias
upload uses `--clobber` and the create-release job updates rather than
duplicates, so a re-run against an existing tag is safe.

To rebuild a tag that already exists — after fixing a runner issue, say — use
the workflow's **Run workflow** button and give it the tag. Pushing the same tag
again is not needed and not enough on its own.

### Building the local half

`./make dist` produces this machine's installers into `release/`, under the same
names, with a `SHA256SUMS.txt` beside them. That is the check that the CI
bundle configuration still works before spending three runners on it.

## Version numbers

Semver, with a caveat that matters more than usual here: **the wire format is
not stable**, and the version number tracks the application rather than the
protocol. Two different builds may not talk to each other, and the changelog
says so at the top rather than in a footnote.

Anything that changes the canonical encoding, the sealed-box construction, a
key derivation, or the sigchain entry format breaks compatibility with stored
data as well as with peers. `0011` did exactly that. Those need a major bump and
a loud changelog entry, and — once there are users with data worth keeping — a
migration rather than a note.

## What a release is not

It is not a claim that the code is audited, and the release body says so. It is
also not a promise of support for the previous tag — only `main` gets fixes, and
[SECURITY.md](../SECURITY.md) states that plainly.

## What 1.0 does not mean

1.0 here means the application is packaged and installable — there is something
to download, on three platforms, with checksums and instructions. It does not
mean the cryptography has been reviewed or the wire format frozen. Neither is
true.

That is worth stating in the one place a reader might take the opposite from,
because a version number is exactly the kind of thing people read as a maturity
claim. [SECURITY.md](../SECURITY.md), the [threat model](threat-model.md), the
README and the changelog all say plainly that nothing here has been audited; the
version number is the only signal pointing the other way, and it is the weaker
one.

What would earn the words: an external review of `vega-core`, and a wire format
settled enough that a build from a year earlier still talks to a current one.
[ROADMAP.md](../ROADMAP.md) tracks both.
