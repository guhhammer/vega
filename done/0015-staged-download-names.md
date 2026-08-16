# 0015 — One set of names for a local build and a released one

`build: stage installers under their download names`

## What landed

`./make dist` now builds the host platform's bundles explicitly, copies them
into `release/` under fixed download names, and writes `SHA256SUMS.txt` beside
them. `release/` is gitignored.

## Why the names are fixed

Tauri names its output after the version: `Vega_1.0.0_amd64.deb`. That is the
right thing for a release page, where you want to know what you are looking at,
and the wrong thing for a download link, which has to be written once and keep
working. So both exist — the version-stamped file is what the release shows, and
a copy under `Vega-linux-amd64.deb` is what a link points at.

The important part is that `./make dist` and `release.yml` stage the *same*
names from the *same* commands. A locally built installer and a released one are
then interchangeable, and testing the packaging locally actually tests the thing
CI will do. The moment those two diverge, the local build stops being evidence.

## Exactly one file, or stop

`stage_one` fails if a glob matches anything other than one file. The
alternative — take the first match, or the newest — silently publishes a stale
artifact from a previous build when the bundle layout moves underneath us. A
build that stops is recoverable; a release with the wrong bytes under the right
name is not.

## Two things that are environment, not choice

`APPIMAGE_EXTRACT_AND_RUN=1` is set unconditionally rather than probed for.
linuxdeploy is itself a type-2 AppImage and mounts itself with FUSE 2; Debian 13
and most current distributions ship only FUSE 3, where the mount fails and the
only message is `failed to run linuxdeploy`. Extracting produces an identical
AppImage and does not care which FUSE is present, so there is nothing to gain
from detecting the difference.

The checksum file is written outside `release/` and moved in. Writing it in
place risks a checksum file that lists itself, and `mktemp` creates it `0600`,
which is wrong for something published beside the downloads it describes — hence
the explicit `chmod`.

## What this deliberately does not do

Cross-compile. Tauri links against the system webview, so a machine builds its
own platform and nothing else. `dist` says so at the end, naming what is missing
and where it comes from, rather than leaving someone to wonder whether the run
was incomplete.
