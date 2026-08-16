# 0017 — A dev build that says why it cannot start

`fix(app): explain a dev build launched without its dev server`

## What landed

A debug-only check in `run()` that waits for the vite dev server and, if it
never appears, prints what is wrong and exits instead of opening a window on
"Could not connect to localhost".

## The failure this replaces

A development build does not contain the frontend. `tauri.conf.json` points the
window at `http://localhost:1420`, so running the binary directly — `cargo run`,
or `target/debug/vega-app` — starts the Rust half *correctly*, prints its
listening addresses, and then shows a webview error naming a symptom.

Everything about that output says the application is working. The libp2p
listeners are real, the log lines are the ones you would expect, and the only
thing that failed is the half that does not announce itself. The `make` script
has warned about this in a comment since it was written, which turns out to be
the wrong place for it: nobody reads a build script when the build succeeded.

The fix is to put the explanation where the failure is.

## Why it waits rather than checks

The first version checked the port once and exited if nothing answered. That
broke `./make dev` — `tauri dev` starts vite and the binary concurrently, and
against a warm cargo cache the binary wins. A 0.7s incremental build beats
vite's ~250ms startup often enough that the dev loop would fail at random, which
is a far worse defect than the one being fixed: an intermittent failure in the
command everyone uses, introduced to improve a message.

So it polls to a deadline. Twenty seconds is long enough that no plausible vite
start misses it and short enough to be a pause rather than a hang, and the
"waiting" line after two seconds means a slow start does not look like one.

The general lesson: a guard that turns a confusing failure into a clear one is
only worth having if it cannot itself fail. Checking a concurrently-starting
service exactly once is a race, and races in developer tooling get blamed on the
tooling.

## Scope

`debug_assertions` and not `mobile`. Release builds embed the frontend and have
no dev server to look for, so they never reach it — the check cannot affect what
ships. The port is hardcoded because `vite.config.ts` sets `strictPort`, so
there is exactly one port a dev server can be on.
