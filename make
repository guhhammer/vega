#!/bin/bash
#
# Vega's build entry point. Every task is one word, and the cheap checks run
# before the expensive ones so a break surfaces in seconds rather than minutes.
#
#   ./make check    types + lints + tests          (the one to run constantly)
#   ./make test     tests only
#   ./make build    compile everything, no lints or tests
#   ./make fmt      rewrite Rust and TS in place
#   ./make dev      run the desktop app against the vite dev server
#   ./make dist     build this machine's installers into release/
#   ./make android  build the Android APK
#   ./make node     run a headless relay/mailbox/bootstrap node
#   ./make clean    delete build artifacts
#   ./make all      fmt, check, dist
#
# `cargo build` — and so `./make build` — leaves binaries that are proof the code
# compiles, not applications: without the Tauri CLI there is no frontend
# embedded, so the window opens on "could not connect to localhost". Use `dev` or
# `dist` to get something that runs.
set -euo pipefail

cd "$(dirname "$0")"

have_npm_deps() { [ -d app/node_modules ]; }
ensure_npm() { have_npm_deps || (cd app && npm install); }

# Where `dist` leaves what it built. The names used here are the ones the
# download table in README.md links to, and the ones release.yml uploads
# alongside Tauri's version-stamped files — so a bundle built on this machine
# and one built by CI are interchangeable, and a link written once keeps
# working.
STAGE=release

host() {
  case "$(uname -s)" in
    Linux) echo linux ;;
    Darwin) echo mac ;;
    MINGW* | MSYS* | CYGWIN*) echo windows ;;
    *) echo unknown ;;
  esac
}

# The version every artifact is named after. One source of truth, checked
# against package.json and tauri.conf.json by ci.yml on every push, and again by
# tag.yml and release.yml before a tag builds.
version() { grep -m1 '^version = ' Cargo.toml | cut -d'"' -f2; }

# Copy one bundle into release/ under its download name. Exactly one file is
# expected to match: anything else means the bundle layout moved underneath us,
# and publishing a stale or silently wrong artifact is worse than stopping.
stage_one() {
  local pattern="$1" name="$2" matches
  # Word splitting is the point here — the pattern is a glob to expand.
  # shellcheck disable=SC2206
  matches=( $pattern )
  if [ ${#matches[@]} -ne 1 ] || [ ! -f "${matches[0]}" ]; then
    echo "Expected one file matching $pattern, found ${#matches[@]}." >&2
    echo "The bundle layout may have changed; $STAGE/ was not updated." >&2
    exit 1
  fi
  cp "${matches[0]}" "$STAGE/$name"
}

case "${1:-check}" in
  fmt)
    cargo fmt
    ensure_npm
    (cd app && npm run format)
    ;;

  check)
    cargo fmt --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace
    ensure_npm
    (cd app && npx tsc --noEmit)
    ;;

  test)
    cargo test --workspace
    ;;

  build)
    # Compilation only — no lints, no tests, no bundling. `check` is the gate and
    # `dist` is the product; this is for the times the question is just whether a
    # wide change still compiles, on both halves, without waiting for the rest.
    #
    # Not `--all-targets`: tests and benches compiling is `check`'s business, and
    # building them here would be most of the cost of `check` for none of the
    # answer. `npm run build` type-checks before it bundles, so the frontend half
    # is the same command ci.yml runs.
    cargo build --workspace
    ensure_npm
    (cd app && npm run build)
    ;;

  dev)
    ensure_npm
    (cd app && npm run tauri dev)
    ;;

  dist)
    ensure_npm
    target="$(host)"
    if [ "$target" = unknown ]; then
      echo "Unrecognised platform $(uname -s) — no bundles can be built here." >&2
      exit 1
    fi

    # Tauri links against the system webview and cannot cross-compile, so this
    # machine only ever produces its own platform's installers. That is the
    # same reason release.yml uses one runner per platform rather than one job.
    case "$target" in
      linux)
        # linuxdeploy, which assembles the AppImage, is itself a type-2
        # AppImage and mounts itself with FUSE 2 in order to run. Debian 13 and
        # most current distributions ship only FUSE 3, where that mount fails
        # and the bundler stops with nothing but `failed to run linuxdeploy`.
        # Extracting instead of mounting produces an identical AppImage and does
        # not care which FUSE is present, so it is set unconditionally rather
        # than probed for — release.yml sets the same variable.
        (cd app && APPIMAGE_EXTRACT_AND_RUN=1 npm run tauri build -- --bundles appimage,deb)
        ;;
      mac)
        # One binary carrying both Apple architectures, so the download table
        # has a single macOS row and nobody can pick the wrong one.
        rustup target add aarch64-apple-darwin x86_64-apple-darwin
        (cd app && npm run tauri build -- --bundles app,dmg --target universal-apple-darwin)
        ;;
      windows)
        # NSIS only: tauri.conf.json installs per-user, which needs no
        # administrator prompt. An MSI would require WiX and elevation.
        (cd app && npm run tauri build -- --bundles nsis)
        ;;
    esac

    rm -rf "$STAGE"
    mkdir -p "$STAGE"

    # Bundles land under the workspace target/, not app/src-tauri/target:
    # app/src-tauri is a member of the root workspace and shares its target dir.
    case "$target" in
      linux)
        stage_one 'target/release/bundle/appimage/*.AppImage' Vega-linux-x86_64.AppImage
        stage_one 'target/release/bundle/deb/*.deb'           Vega-linux-amd64.deb
        ;;
      mac)
        stage_one 'target/universal-apple-darwin/release/bundle/dmg/*.dmg' Vega-macos-universal.dmg
        ;;
      windows)
        stage_one 'target/release/bundle/nsis/*-setup.exe' Vega-windows-x86_64-setup.exe
        ;;
    esac

    # So a download can be checked against something other than its file name.
    # Written outside the directory first: a checksum file must not end up
    # listing itself. macOS has shasum where Linux has sha256sum; the output
    # format is the same, and `sed` strips the ./ that globbing introduces.
    sums="$(mktemp)"
    (cd "$STAGE" && { sha256sum ./* 2>/dev/null || shasum -a 256 ./*; }) \
      | sed 's| \./| |' > "$sums"
    mv "$sums" "$STAGE/SHA256SUMS.txt"
    # mktemp creates 0600, which is wrong for a file meant to be published
    # beside the downloads it describes.
    chmod 644 "$STAGE/SHA256SUMS.txt"

    echo
    echo "Staged in $STAGE/ for $(version):"
    ls -1sh "$STAGE" | tail -n +2 | sed 's/^/  /'
    echo
    case "$target" in
      linux)   echo "Still missing: macOS .dmg, Windows setup .exe." ;;
      mac)     echo "Still missing: Linux AppImage and .deb, Windows setup .exe." ;;
      windows) echo "Still missing: Linux AppImage and .deb, macOS .dmg." ;;
    esac
    echo "Those cannot be cross-compiled here. Land a version bump on main — or"
    echo "push a v* tag — and CI builds all three, attaching them to a draft"
    echo "release."
    ;;

  android)
    ensure_npm
    # Needs ANDROID_HOME and NDK_HOME; `tauri android init` is a one-time step.
    (cd app && npm run tauri android build)
    ;;

  node)
    shift
    cargo run --release -p vega-net --example seed -- "$@"
    ;;

  clean)
    # target/ passes 10GB quickly once a few profiles have been built — debug,
    # test, release and the Tauri bundle each keep their own copy of ~400
    # dependencies. Worth knowing before it fills a disk.
    echo "removing $(du -sh target 2>/dev/null | cut -f1 || echo 0) of build artifacts"
    cargo clean
    rm -rf app/dist app/src-tauri/gen "$STAGE"
    ;;

  all)
    ./make fmt
    ./make check
    ./make dist
    ;;

  *)
    echo "unknown task: $1" >&2
    # The header above, ending where the code starts. Anchored rather than a
    # line range, so adding a task cannot leave this printing half a paragraph.
    # The `;` before `}` is redundant under GNU sed and required by the BSD one.
    sed -n '3,/^set -euo/{ /^set -euo/!p; }' "$0" >&2
    exit 1
    ;;
esac
