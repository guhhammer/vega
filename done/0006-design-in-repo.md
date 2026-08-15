# 0006 — The protocol design, in the repository

`docs: keep the protocol design in the repo`

*Written after the fact — commit `6447f41` landed before this record did.*

## What changed

`.documentation/design.md`, and the README and SUPPORT links pointed at it.

## Why

The design was published as an artifact and lived only there. The reasoning the
entire codebase rests on was one broken link away from being lost, and could not
be read offline, diffed, or reviewed alongside a change that contradicted it.

A protocol design that is not in the repository is not really the project's
design; it is somebody's note about the project.

## Saved verbatim, not updated

The obvious temptation was to rewrite it so it describes the code as it now
stands. That would have been worse.

A design record is worth more when it still shows what was believed at the time.
It says a sigchain will carry prekeys and does not anticipate that chains would
need to travel inside messages to make that work; it does not foresee the sender
impersonation break. Both of those are *useful* — they show where the reasoning
was thin, which is where the next thin spot probably is too.

So it keeps the original text, with a header pointing at the README and `done/`
for what changed since. `done/0005` covers the security review specifically.

## The figures

The two SVG diagrams relied on CSS custom properties for theming, which do not
exist in a markdown file. They became box drawings, which render in a terminal,
on GitHub, in an editor, and in a diff.

Losing the colour coding cost little: the ladder diagram carried its meaning in
the ordering and in the solid-versus-dashed distinction, and both survive as
text.
