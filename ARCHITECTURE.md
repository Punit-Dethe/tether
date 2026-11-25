# Architecture

A map of the code for anyone (person or agent) about to change it. The full
design and milestones are in [docs/PLAN.md](docs/PLAN.md).

## Shape

```
src/main.rs      thin binary: parse args, run, print, exit code
src/lib.rs       the library the binary and tests share
src/cli.rs       clap definitions only — no behaviour
src/commands/    one file per command; each returns a Report, none prints
src/report.rs    the JSON envelope (the agent contract) and text rendering
src/error.rs     every failure, each with a stable machine code
src/git.rs       the ONLY place git runs
src/config.rs    .tether/config.toml: upstream, moved folders, owned areas,
                 review areas, relocations, budget scope
src/syncpoint.rs Upstream-Synced: markers -> the sync point (refuses divergence)
src/decisions.rs .tether/decisions/<sha>.toml: status, note, port and review
                 records with evidence
src/inbox.rs     derived list of tracked upstream commits and their state
src/engine.rs    merge mode: graft + merge --squash, owned-area rules, commit
src/review.rs    review gate: commits in review areas need evidence
src/budget.rs    divergence budgets, strays, adoption, drift (.tether/budgets.toml)
src/ports.rs     port audit: synced fixes to relocated files need records
src/paths.rs     fnmatch-style patterns
src/time.rs      the one date format tether reads
tests/support/   throwaway two-repo fixtures with no shared history
tests/cli.rs     end-to-end tests against the built binary
```

## Rules

1. **Real git, one door.** tether shells out to the `git` binary (≥ 2.40) so
   rename detection, rerere, merge drivers and `replace --graft` behave
   exactly as in git. Only `src/git.rs` builds a `git` command line.
2. **Commands return, `main` prints.** A command never writes to stdout; it
   returns a `Report`. That keeps every command testable and every output in
   the same envelope.
3. **Codes are contract.** `Error::code()` strings and the envelope fields
   are what agents depend on. Add freely; never rename or remove.
4. **Exit codes:** 0 done, 1 failed, 2 worked but action needed.
5. **Tests never touch the machine's git config.** Fixtures set a repo-local
   identity and `GIT_CONFIG_NOSYSTEM`; `GIT_CEILING_DIRECTORIES` stops git
   from climbing out of a temp dir into some other checkout.
6. **No network in tests.** Fixtures use local paths as remotes.
7. **Low overhead.** Few dependencies; no async runtime, no daemon.

## Where things will go

Planned modules (see PLAN.md §6): carry mode, `relocate`, `map`, `budget`, `verify`, `packet`, `session`, and an optional
`semantic` merge driver behind a cargo feature.
