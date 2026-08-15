# 0007 — Repository metadata

`chore: repository metadata GitHub recognises`

## What landed

`GOVERNANCE.md`, `ROADMAP.md`, `CITATION.cff`, `.editorconfig`, `.gitattributes`.

With the files from 0001, this completes GitHub's community profile: README,
LICENSE, CODE_OF_CONDUCT, CONTRIBUTING, SECURITY, SUPPORT, issue templates, PR
template, CODEOWNERS, FUNDING, dependabot, governance, citation.

## GOVERNANCE says one person decides

Because one person does. Writing a committee into a document does not create
one, and a contributor who reads about a "steering group" and then waits for it
has been misled.

It also states the bus factor is one. That is a real risk to anyone considering
depending on this, and hiding it would be the kind of omission this project's
documents otherwise avoid.

The part worth having is the list of things refused **on principle**, so that
nobody spends a weekend on them first: anything needing a server, anything that
lets an intermediary learn who talks to whom, telemetry of any kind, new
cryptographic primitives, and convenience that quietly weakens a guarantee. Each
of those is defensible in a centralised messenger. None are available here.

## ROADMAP is marked against reality

The phases come from the design's build order, but each is marked with what
actually exists rather than what is planned. P3 is split — the code is written
and the rendezvous round-trip is tested, but two machines on two genuinely
different networks has never happened, and that cannot be faked on loopback.

Saying "✅ / ⏳" and explaining the gap is more useful than either tick or cross
would be.

The closing section says what would help most, in order. First is two machines
on two real networks, because everything above P3 assumes it works.

## .gitattributes does two useful things

- **Collapses the lockfiles.** `Cargo.lock` and `package-lock.json` are marked
  `linguist-generated -diff`. A lockfile change is worth *noticing* in a pull
  request, not worth scrolling through.
- **Fixes the language stats.** Without marking the markdown as documentation,
  GitHub reports this project as mostly JSON and Markdown, which tells a visitor
  nothing about what it is.

## CITATION.cff

Marginal for a messenger, and GitHub renders a "Cite this repository" button
from it, so it costs one file. The abstract is written to be readable by someone
who has never heard of the project, which is more than can be said for most
repository descriptions.
