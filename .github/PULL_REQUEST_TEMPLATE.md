## What this changes

<!-- One or two sentences. What is different afterwards? -->

## Why

<!-- The reasoning that would otherwise be lost. If this is a trade-off, name
     what you are giving up. -->

## Trade-offs

<!-- Delete if genuinely none. Most changes to the transport or crypto have one. -->

## Checks

- [ ] `./make check` passes
- [ ] Tests added for the behaviour that changed — for a bug, one that failed before
- [ ] A note added to `done/` if this is landing as its own commit
- [ ] Comments explain *why*, not what

## If this touches crypto, networking, or storage

- [ ] No new panic reachable from network input
- [ ] Data from peers is treated as claims until verified
- [ ] Any new field visible to a relay has a stated privacy cost
- [ ] Nothing new is stored unencrypted
