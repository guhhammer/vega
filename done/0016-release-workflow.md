# 0016 — A release workflow that cannot race itself

`ci: rebuild the release workflow around a single draft release`

## What landed

`release.yml` rewritten as four jobs — `verify`, `create-release`, `build`,
`checksums` — and two nightly jobs fixed that had never run.

## The race that was there

Every job in the old build matrix passed `tagName` to `tauri-action`, which
creates the release if it is missing. Three runners start at the same moment,
all three see no release, and all three try to create it. What you get is
either a duplicate release or two failed jobs, depending on timing — and it
would have looked like a flaky runner rather than a design error, because it
only appears when the jobs are close enough together to collide.

Creating the release once, in a job of its own that the builds depend on,
removes the question rather than narrowing the window.

## Verify first, because it is free

Three platform builds take tens of minutes. Comparing the tag against the three
files that carry the version takes seconds, and a mismatch there means every
installer would be named in contradiction to the release holding it. It runs
first for that reason alone.

Three files, not one: `Cargo.toml` names the binary, `tauri.conf.json` names the
bundles, and `package.json` is what a reader of the frontend believes. Any of
them drifting is the kind of thing noticed only once it is already published.

## Checksums last, and only last

`SHA256SUMS.txt` covers files that exist. It cannot be computed by a build job,
because a build job only knows about its own platform, and it cannot be computed
before the uploads finish. So it is a job that depends on all of them, runs with
`always()` so a single failed platform still leaves the rest checksummed, and
deliberately checks nothing out — hence `GH_REPO`, without which `gh` looks for
a git remote that is not there.

## Draft, not published

A bad build should be discardable before anyone sees it. The cost is that
`releases/latest/download/…` links do not resolve until the draft is published,
which is one click and is stated in the file so nobody debugs a working link.

## No cache here, unlike everywhere else

`ci.yml` and `nightly.yml` both cache, and should — they run constantly and the
dependency tree is several hundred crates. A release is rare and is the one
build whose output people run, so it starts cold rather than assembling itself
partly from whatever a previous job left behind. The earlier draft of this file
kept a cache step with a key nothing ever writes, which bought nothing and
implied otherwise.

## Two nightly jobs that had never worked

`rustdoc` and `unused-deps` both compile the workspace, which includes the
desktop crate, which links against the system webview. Neither installed it, so
both failed in `pkg-config` long before reaching the thing they were meant to
check. They also shared a cache key and so evicted each other. Nightly failures
are quiet by design, which is exactly why a job that never ran can stay broken.
