#!/bin/bash
#
# Vega's build entry point. Every task is one word, and the cheap checks run
# before the expensive ones so a break surfaces in seconds rather than minutes.
#
#   ./make check    types + lints + tests          (the one to run constantly)
#   ./make test     tests only
#   ./make fmt      rewrite Rust and TS in place
#   ./make dev      run the desktop app against the vite dev server
#   ./make dist     build a real installer
#   ./make android  build the Android APK
#   ./make node     run a headless relay/mailbox/bootstrap node
#   ./make all      fmt, check, dist
#
# `cargo build` on its own leaves binaries that are proof the code compiles, not
# applications: without the Tauri CLI there is no frontend embedded, so the
# window opens on "could not connect to localhost". Use `dev` or `dist`.
set -euo pipefail

cd "$(dirname "$0")"

have_npm_deps() { [ -d app/node_modules ]; }
ensure_npm() { have_npm_deps || (cd app && npm install); }

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

  dev)
    ensure_npm
    (cd app && npm run tauri dev)
    ;;

  dist)
    ensure_npm
    (cd app && npm run tauri build)
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

  all)
    ./make fmt
    ./make check
    ./make dist
    ;;

  *)
    echo "unknown task: $1" >&2
    sed -n '3,20p' "$0" >&2
    exit 1
    ;;
esac
