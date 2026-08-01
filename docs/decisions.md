# Decisions

Why Tycho is shaped the way it is, and what was rejected. The architecture docs
describe what the system does; this file records why, so that a later change is a
deliberate reversal rather than an accidental one.

`background.md` holds the incidents these decisions respond to.

## D1. Build this at all, rather than install kopia

restic, kopia, borg and Arq already do scheduled, deduplicated, encrypted backups.
If the only goal were "have backups", installing kopia is a half-hour job and it
would be dishonest not to say so.

Three things they do not have together:

- **Git-native capture.** Repositories are backed up as repositories - full history
  plus an overlay of what git cannot restore - and the store is itself a git
  repository, inspectable with tools already installed. The incumbents store opaque
  chunks.
- **Synced folders as a first-class remote**, with git push semantics on top.
- Ownership of the tool and its CLI.

Non-goal, stated in the README so nobody is disappointed: competing with restic on
storage engineering for large binary datasets. The target is developer machines and
company documents.

## D2. Repository history is fetched into the object database, not written as bundle files

**Rejected:** `git bundle create --all` per repository, committed into the store as
a file, as the original proposal specified.

A bundle is one opaque binary that changes wholesale when any ref moves, so git can
never delta it. handbook is 7.7 MB today, so a weekly run adds roughly 400 MB
per year for that one repository, forever, on local disk and in every cloud folder.
Twelve repositories make that untenable within a year.

**Chosen:** fetch each repository's refs into `refs/tycho/<key>/*` in the store.
Objects deduplicate and delta-compress, and a repository whose refs have not moved
costs zero bytes. A bundle is still the right artifact for handing history to
someone, so `tycho restore --bundle` generates one on demand, which is the only
moment one is genuinely needed.

**The cost, stated plainly:** `refs/tycho/*` is outside `refs/heads/*`, so pushes
need explicit refspecs and a mistake there silently omits the majority of what is
being protected. That risk is concentrated in one line of code with a test on it,
which is a better place for it than in unbounded storage growth.

## D3. The store is bare

**Rejected:** a store with a working tree that files are copied into and then
committed from.

**Chosen:** a bare repository, with commits built by plumbing - `hash-object`,
`update-index --index-info`, `write-tree`, `commit-tree`, `update-ref`.

This halves local disk, because there is no second full copy of the data alongside
the object database. It removes the copy step, since files are hashed straight from
the source. And it makes atomicity structural: the only non-append-only mutation in
a run is the final ref move, so an interruption cannot leave a partial state.

The cost is that the store cannot be browsed with `ls`. It can be browsed with
`git ls-tree` and `git show`, which is what a git repository offers anyway.

## D4. Every file is hashed on every run

**Rejected:** an mtime-and-size cache to skip unchanged files.

The cache introduces a failure where a file whose mtime did not move is silently
never backed up again, and that failure is invisible until a restore. Given this
project exists because of a silent failure, adding a new class of silent failure to
save seconds is the wrong trade. At a few gigabytes a full read is seconds of
sequential IO.

Revisit with measurements, behind a flag, if a profile ever grows large enough for
it to matter.

## D5. No resident daemon

**Rejected:** a long-lived process with an internal scheduler, catch-up logic, and
an IPC protocol for the CLI to query.

`man launchd.plist` states that unlike cron, launchd starts a missed
`StartCalendarInterval` job when the machine wakes. That is the catch-up behaviour
the daemon existed to provide. The proposal separately required the CLI to work
fully with the daemon down, which means every code path has to exist without it
regardless.

Dropping it removes tokio, croner, `interprocess`, a socket protocol, a second
lifecycle to install and debug, and a failure mode where the daemon is dead and
backups stop silently.

Windows Task Scheduler is the weaker half of this argument. Revisit on the real
machine rather than assuming.

## D6. No ports, no traits

**Rejected:** a `Remote` trait with a folder implementation, per the usual house
ports-and-adapters structure.

`~/.claude/guidance/architecture.md` is explicit that a port exists to be mocked,
injected or swapped, and that a port with a single forever-implementation which is
never mocked is dead abstraction. There is one remote kind. Tests run real git
against temporary directories, which is a better test double than a mock of git.

The boundaries still exist as module boundaries, with `config` and `plan` pure and
everything doing IO downstream of them. A trait goes in when a second remote kind
does.

## D7. Shell out to system git

**Rejected:** `gix`, or `libgit2` via `git2`.

Git is present on every machine Tycho targets and is the reference implementation
of the format the entire design depends on. Plumbing commands are a stable public
interface, and the batch forms - `hash-object --stdin-paths`, `update-index
--index-info` - mean process count is constant rather than per file.

Revisit only if a machine without git becomes a real target.

## D8. Full history forever, no retention policy

**Rejected:** keep-N-backups, as the old script did with `KEEP=8`.

Retention policies are a thing to get wrong, and the failure mode is that the
backup you need was pruned last month. Keeping everything means any backup is
recoverable.

**The honest cost:** a mistake committed once is permanent. Point Tycho at a
directory of large binaries and those objects stay in history after the config is
fixed. Mitigations are `--dry-run` before a first run, default junk ignores, and a
documented escape hatch of a fresh store with the old one archived. History
rewriting is deliberately not offered, because a backup tool that can quietly
rewrite its own history is not a backup tool.

## D9. `.gitignore` never affects capture

The July 2026 Google Drive incident deleted files under a repository. Git restored
everything except `handbook/CLAUDE.md`, which was gitignored - the only
unprotected file was the one git could not resurrect.

Exclusions come from the profile config and nowhere else. The overlay exists
specifically to capture uncommitted, untracked and gitignored files.

The consequence, and it is a real one: the overlay must be filtered through the
profile's rule tree, because `git status --ignored` will happily list every byte of
`target/` and `node_modules/`.

## D10. Alias collisions are a config error

**Rejected:** automatic disambiguation, appending a numeric suffix to the second
root sharing a basename.

A generated `docs-2` moves if the config list is reordered, and stored paths must be
stable across runs or history stops lining up. `tycho config check` fails and asks
for an explicit name.

**Also rejected:** mirroring full absolute paths into the store, which is
collision-free but embeds the username in every path in every cloud folder and
makes commit messages unreadable.

## D11. One machine per profile per remote

Two machines pushing the same profile name into one folder diverge, and the second
is rejected rather than merged. A merged history would describe neither machine.
Two machines mean two profile names.

## D12. An unchanged run still commits

A run where nothing changed anywhere commits with a body reading `no changes`.

A gap in the history would otherwise be ambiguous between "nothing changed" and
"the backup did not run" - and a year of the latter being invisible is exactly what
this project was built in response to.
