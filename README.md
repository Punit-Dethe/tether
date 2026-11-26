# tether

Keep a long-lived fork in step with the project it came from — even one that
was downloaded rather than forked, and whose history you don't want mixed
into yours.

tether is a small command-line tool built for AI coding agents first and
people second. Every command can answer in JSON with the same shape, and
always says what to do next.

> **Status: early — milestones M1 to M5 done.** Working now: `doctor`,
> `init`, `status`, `inbox`, `packet`, `decide`, `sync`, `verify`, `map`,
> `closeout` and `check`. They give the same results as the Python tooling they replace on a
> real product fork's history (Grain on Handy: 299 tracked commits, a
> 72-commit sync with 13 conflicts, 42 budgets, the port audit and the review
> gate). An AI agent can run a whole sync by following `tether status` alone
> (see [AGENT.md](AGENT.md)). See [docs/PLAN.md](docs/PLAN.md).

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
- **Plan the sync.** `tether plan` finds the furthest point you can sync to
  with no conflict, previews each upstream release in range, and sorts the
  waiting commits from easiest to hardest by what they touch in your fork.
- **Folders.** `tether folder <name> <commits>…` groups upstream commits your
  own way, for reading; it never changes what a sync does.
- **A local dashboard.** `tether ui` opens the same reports in your browser:
  what is waiting, the sync plan, releases, and where your fork differs, with
  drag-and-drop folders. It listens on this machine only and changes nothing
  else.
- **An MCP server.** `tether mcp` offers the same commands as tools to agents
  that speak the Model Context Protocol (see below).

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

## For agents without a shell: MCP

Agents that can run commands just run `tether … --json`. For the rest, add
tether as an MCP server; the agent's app starts `tether mcp` in the fork when
a session opens and stops it when the session ends. With Claude Code:

```sh
claude mcp add tether -- tether mcp            # read-only
claude mcp add tether -- tether mcp --allow-writes
```

Other apps take the same command in their MCP settings (`"command": "tether",
"args": ["mcp"]`, run in the fork, or add `-C <fork>`).

Each tool answers with the command's JSON report above. Read-only tools are
always on: `status`, `inbox`, `plan`, `packet`, `sync_preview`, `map`,
`check`, `doctor`, and `folder` (folders are only a reading aid). `decide`,
`sync`, `verify` and `closeout` appear only with `--allow-writes`, which only a
person can pass. Whoever calls the tools counts as an agent, so the fork's
`[agent] needs_human` policy applies to every call.

## Licence

MIT — see [LICENSE](LICENSE).
