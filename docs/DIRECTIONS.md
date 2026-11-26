# Where tether could go next

Status: ideas, not commitments (November 2026). This page records the
directions worth taking, why, and in what order, so the next step is a
decision rather than a brainstorm.

**Built (27 November 2026):** direction 1, `tether deps`, as a read-only
command and MCP tool (not yet in the inbox or dashboard), and direction 2,
several upstreams (`tether -u <name> …`, with `upstream.prefix` for a
vendored folder). Deferred by decision: runs by itself, agents prove it, give
fixes back, other tools' output, many repositories.

## The idea in one line

tether already answers "what changed upstream of my fork, and did I deal with
it?" for one fork. The same question applies to **everything a product is
built on**: the forked app, the forked libraries pinned inside it, and the
ordinary dependencies. tether could become the **maintenance desk** for a
product: one place, one inbox, one record of decisions, that a person or an
agent works through.

It should not become a general project manager (tickets, sprints, people).
Linear, Jira and GitHub Projects do that. tether's lane is *upstream*: code you
did not write but depend on.

## What already exists, and the gap

| Tool | What it does well | What it leaves open |
| --- | --- | --- |
| Dependabot, Renovate | Open PRs to bump dependency versions; security alerts for packages | A dependency pinned to a **fork** (`git = …, rev = …`): is the fork behind its own parent? Did the parent ship a fix the fork lacks? |
| OSV-Scanner, `cargo audit`, `npm audit` | Known vulnerabilities in the versions you use | Fixes that never got an advisory; anything not from a package registry |
| endoflife.date | When runtimes and frameworks stop being supported | Nothing about your code |
| GitHub "Sync fork" | One click for a fork with a few patches | Forks that diverged, or any record of what was skipped |

The gap is the same one tether fills for Grain itself: **forks and vendored
code**, where no registry, version number or advisory tells you that you fell
behind.

Grain is the proof. Besides Handy, it builds on six pinned forks
(November 2026):

| Dependency | Pinned to | Kind |
| --- | --- | --- |
| transcribe.cpp | `Punit-Dethe/transcribe.cpp` @ rev | our fork, 7 commits on top, upstream 35 ahead |
| xn | `Punit-Dethe/xn` @ rev | our fork |
| hf-hub | `cjpais/hf-hub`, branch `cancellable-downloads` | someone else's fork |
| tao, tao-macros | `cjpais/tao` @ rev | someone else's fork |
| rdev | `rustdesk-org/rdev`, no pin | someone else's fork |
| tauri-nspanel | `ahkohd/tauri-nspanel`, branch `v2.1` | a branch |

Today nothing tells us when any of these fall behind, or when their parent
fixes a bug or a security hole.

## Directions

### 1. Forked dependencies (`tether deps`): the best next step

Read the manifests (`Cargo.toml`, `package.json`, `go.mod`, `.gitmodules`,
vendored folders with a recorded origin) and list every dependency that comes
from git rather than a registry. For each one:

- where it is pinned (rev, branch, or nothing: flag unpinned ones);
- whose fork it is, and its parent project (from GitHub, or a recorded
  `upstream` entry);
- how far the pinned commit is behind the fork's own branch, and the fork
  behind its parent;
- upstream commits that look like security fixes (the GHSA/CVE/keyword
  detection tether already has);
- for **our own** forks: a merge preview against the parent (the same
  `merge-tree` dry run as `tether plan`), so "can I just rebase?" has an answer.

All of it lands in the existing inbox and dashboard as one more source, with
the same decide/ignore/note flow. A small fork with shared history gets a
light mode: no `.tether/` config needed in the fork, just the patch stack and
a rebase preview. A fork that grows big can graduate to full tether.

Why first: it is the gap no other tool covers, Grain needs it today, and it
reuses almost everything tether already has (fetch, security detection,
merge preview, inbox, decisions, UI, MCP).

### 2. Several upstreams in one repository

Already promised in the README. One config per upstream (`[[upstream]]`), each
with its own sync point, inbox, plan and decisions. Direction 1 makes this
more natural: a vendored library is just another upstream.

### 3. Runs by itself

- A ready-made GitHub Action: run `tether status` and `plan` on a schedule,
  open a PR with the conflict-free step already synced, and post the inbox
  summary.
- A status check or badge that turns amber when security fixes are waiting.
- A weekly digest (issue or PR comment): what arrived upstream, what is
  waiting on a decision, what got older.

This closes the honest gap in the comparison table ("runs by itself").

### 4. Agents do the work, tether proves it

The agent resolves conflicts and ports fixes; tether checks the result: the
port audit says no upstream change was dropped, three-way verify says whose
test broke. Over MCP, an agent's app can run the weekly round on its own and
stop only where the fork's `needs_human` policy says so. This is the
differentiator against "an agent rebases blindly".

### 5. Give fixes back

Mark a fork change as "should go upstream"; tether drafts the upstream PR
(diff, description, link to why). Every change upstream accepts is one less
thing to carry, so the fork shrinks over time. Copybara does the mechanics for
code moves; tether would do it for the fork's own fixes, tracked in the same
decisions.

### 6. Ordinary dependencies, by reading other tools

Do **not** rebuild Dependabot or OSV-Scanner. Instead, read their output
(OSV-Scanner JSON, `cargo audit --json`, Renovate's dependency dashboard) and
show it in the same inbox, so a maintainer sees forks, vendored code and
registry packages in one place, with one decision record. Add end-of-life
dates for runtimes from endoflife.date the same way.

### 7. Many repositories (later, maybe)

A dashboard across several repositories for someone who maintains a family of
products. Only worth it once 1–3 are proven; this is where "project manager"
scope creep starts, so it needs a real user first.

## Suggested order

1. **`tether deps`**: forked dependencies, read-only, in the inbox and UI.
2. **Runs by itself**: the GitHub Action and badge (makes 1 useful without
   anyone remembering to look).
3. **Several upstreams** in one repository.
4. **Agents do the work, tether proves it**.
5. **Give fixes back**.
6. **Other tools' output** in the same inbox.
7. Many repositories, only if someone needs it.

## What to keep true

- Local and free; no account, no phone-home.
- Read-only by default; every write needs a person or an explicit agent policy.
- One inbox, one decision record, one JSON contract, whatever the source.
- Never compete with a tool that already does a job well; read its output.
