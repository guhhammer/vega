# 0011 — The casts the new lint found

`fix(core): remove three truncating casts, one of them inside a signature`

## What happened

`cast_possible_truncation`, added in 0008, fired on three casts. The incremental
build had them cached; a clean rebuild surfaced them, and `./make check` runs
clippy at `-D warnings`, so the tree I had just committed did not pass its own
gate. Fixed immediately rather than left.

That is the lint doing exactly what it was added for, on its first outing.

## The one that mattered

`codec.rs` wrote its length prefix as `v.len() as u32`.

The prefix exists to make the canonical encoding injective — without it,
`("ab", "c")` and `("a", "bc")` produce identical bytes. A truncated prefix
brings the collision back: two different field sequences encode the same, and
they do so **inside a signature**, silently.

Unreachable in practice — nothing here is four gigabytes — but "unreachable in
practice" is a poor guarantee for something that would corrupt signed data. The
prefix is now `u64`, which removes the length at which it could happen rather
than arguing about whether that length can occur. Four extra bytes per field on
a structure that is only ever hashed.

The same reasoning applies to the context length in `seal.rs`, changed the same
way.

## The other one

`olm_type as u8`, where vodozemac's `to_parts()` returns a `usize` that is 0 or
1. Now a checked conversion returning a wire error. It cannot fail; a value that
did would mean vodozemac had changed its contract, and finding that out as an
error beats finding it out as a silently wrapped byte.

## Note

Both encodings changed, so signatures and sealed boxes from before this commit
do not verify against code after it. Nothing has been released and there is no
deployed data, so no migration is needed — but it is the kind of change that
would need a version bump later.
