# tether

Keep a long-lived fork in step with the project it came from — even one that
was downloaded rather than forked, and whose history you don't want mixed
into yours.

tether is a small command-line tool built for AI coding agents first and
people second. Every command can answer in JSON with the same shape, and
always says what to do next.

> **Status: early.** Milestone M0 (the skeleton) is done: `tether doctor` and
> `tether status` work; every other command answers `not_implemented` and
> names the milestone that brings it. See [docs/PLAN.md](docs/PLAN.md).

## What it will do

- **Clean history.** Each sync lands as one commit on your side, with an
  `Upstream-Synced: <sha>` line recording how far you have taken upstream.
  None of upstream's commits or authors appear in your history.
- **An inbox, not a wall of commits.** Upstream changes since your last sync,
  grouped by kind (security fixes first) and by what you decided about them.
- **Work packets for agents.** For one upstream change: the diff, why it was
  made, where it lands in your code (even after you moved files), and how to
  check the port worked.
- **Decisions with evidence.** Take, skip or adapt each change, with the
  reason kept next to the code.
- **Verify three ways.** Build and test your fork as it was, upstream as it
  shipped, and the result — so you know whose test broke.
- **A divergence map generated from the code**, not maintained by hand.

## Try it

```sh
cargo install --git https://github.com/Punit-Dethe/tether
cd your-fork
git remote add upstream <url-of-the-original-project>   # if not already there
tether doctor
tether status --json
```

## The JSON contract

```json
{
  "tether": "0.1.0",
  "command": "status",
  "ok": true,
  "summary": "tether is not set up in this repository yet.",
  "data": { "configured": false },
  "next": { "command": "tether init", "why": "..." },
  "error": null
}
```

Exit codes: `0` done, `1` failed (see `error.code`), `2` worked but something
needs action (see `next`). Agents should branch on `error.code`, never on
message text.

## Licence

MIT — see [LICENSE](LICENSE).
