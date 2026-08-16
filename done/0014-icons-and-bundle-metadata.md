# 0014 — The icon set, and what a bundle tells the system about itself

`build: complete the icon set and bundle metadata`

## What landed

The full platform icon set under `app/src-tauri/icons/`, and the `bundle`
section of `tauri.conf.json` filled in.

## The Windows and macOS releases could never have been built

`icon: ["icons/icon.png"]` with a single 512×512 PNG beside it is enough for a
Linux build and enough for `tauri dev`, which is why it survived to a tagged
release without anyone noticing. It is not enough for the other two bundlers:
NSIS embeds a `.ico` and the macOS app bundle embeds an `.icns`, and neither
will synthesise one from a PNG. Both jobs would have failed at bundle time,
after their platform build had already run.

This is the failure mode worth naming: a configuration that is correct on the
machine you develop on and wrong on the two you do not. It cost nothing to fix
and would have cost a release to discover.

`tauri icon` generates the set, including the Android and iOS trees. Those are
checked in rather than generated at build time so the bundle is reproducible
from the repository alone.

## Metadata is not decoration

The `.deb` produced before this had no maintainer, no homepage, and no
dependency declaration. That last one matters: with `Depends:` populated,
`apt install ./Vega-linux-amd64.deb` resolves `libwebkit2gtk-4.1-0` and
`libgtk-3-0` for the user. Without it the package installs and the application
fails to start, which is a worse outcome than refusing to install.

The descriptions are the ones a package manager shows in search results, so they
are written for someone who has not heard of this and is deciding whether to
read further.

## Windows installs per-user

`installMode: currentUser` puts it in `%LOCALAPPDATA%` and asks for no
administrator prompt. A per-machine install would need elevation, and elevation
on an unsigned installer is a worse thing to ask someone to click through than
SmartScreen already is. The cost is that it installs per profile rather than
once per machine, which for a personal messenger is the right trade.
