# Documentation

| | |
|---|---|
| [installing.md](installing.md) | Getting the application onto a machine, verifying the download, and where it keeps its data. Start here if you are not building from source. |
| [design.md](design.md) | The protocol design, as drafted before implementation. Why the system is shaped this way, and what it deliberately does not protect against. |
| [architecture.md](architecture.md) | How the code is actually arranged, and which boundaries are load-bearing. Read after the design. |
| [wire-format.md](wire-format.md) | Byte-level reference for everything that crosses a network. What each party can see. |
| [threat-model.md](threat-model.md) | Who the adversaries are, what each one can do, and what stops them. |
| [running-a-seed.md](running-a-seed.md) | Operating a bootstrap, relay and mailbox node. |
| [testing.md](testing.md) | What the test suite proves, what it cannot, and how to write a test here. |
| [releasing.md](releasing.md) | Cutting a release, and what a version number does and does not promise. |
| [android.md](android.md) | Building for Android and the native work still outstanding. |

## Where else things are written down

- **[`done/`](../done/)** — one file per commit, recording *why*. The
  alternative that was rejected, the trade-off accepted, the subtlety that cost
  an afternoon. This is the most useful thing in the repository after the code.
- **[`../SECURITY.md`](../SECURITY.md)** — reporting policy, scope, and the
  known weaknesses.
- **[`../ROADMAP.md`](../ROADMAP.md)** — what is built, what is not, what would
  help most.
- **The code.** Comments here answer *why*; what the code does should be
  readable from the code itself.

## Reading order

Coming to this cold and wanting to understand it:

1. [`../README.md`](../README.md) — what it is, in a page.
2. [design.md](design.md) §00 — why "serverless" needs a careful definition,
   because everything else follows from that.
3. [design.md](design.md) §04 — the transport ladder, which is the core idea.
4. [architecture.md](architecture.md) — how that maps onto three crates.
5. [`done/0005-security-review.md`](../done/0005-security-review.md) — the
   sharpest thing in the repository, because it is where the design was wrong.

Coming to it wanting to change something: [architecture.md](architecture.md),
then [testing.md](testing.md), then
[`../CONTRIBUTING.md`](../CONTRIBUTING.md).
