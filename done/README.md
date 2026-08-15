# done/

One markdown file per commit, recording *why* the change was made.

This is not a changelog — [CHANGELOG.md](../CHANGELOG.md) is that, and it
summarises what shipped. These files hold the reasoning that would otherwise be
lost: the alternative that was rejected, the trade-off that was accepted, the
subtlety that cost an afternoon.

A commit that only moves code does not need a file here. One that makes a
decision does.

## Convention

`NNNN-short-slug.md`, numbered in order, opening with the commit subject.

Worth writing down:

- what the change actually does, in a sentence
- what was considered and not chosen, and why
- what it costs — every design note in this repo names its trade-off
- anything that was surprising, so the next person does not rediscover it

Not worth writing down: a restatement of the diff.
