# tycho

[![CI](https://github.com/CoreEngineX/tycho/actions/workflows/ci.yml/badge.svg)](https://github.com/CoreEngineX/tycho/actions/workflows/ci.yml)
[![Licence: Apache-2.0](https://img.shields.io/badge/licence-Apache--2.0-blue.svg)](LICENSE)

<img src="docs/logo.svg" alt="Tycho" width="132">

**A backup daemon that treats git as the storage format, not one of the things being
backed up.** It captures exactly what git itself refuses to track - gitignored
secrets, untracked drafts, uncommitted edits - into a bare git repository, and pushes
that repository to external drives and synced cloud folders, so a restore anywhere
is `git log` and a checkout rather than a plea to whatever cloud client did or did
not finish uploading.

## See it work

A repository with a secret git would leave behind, and a draft it never saw:

```text
$ git status --short
?? notes.md

$ ls -la .env
-rw-------  1 you  staff  29 .env       # 0600, gitignored, never tracked
```

A run captures both, plus the repository's own history:

```text
$ tycho run personal
personal  e88afc1
  captured     5 added                                                 0 B
  written      in 0s                                               73.7 KB
```

The repository is gone from disk. Recovering it from what was already pushed:

```text
$ tycho restore personal --into ~/recovered
reading   1 backup, 2026-08-04 to 2026-08-04
using     e88afc1
restored  5 files                                                  2.96 KB
metadata  7 modes restored, 1 attribute
restored  1 repository with full history
          Documents/project                 main        overlay: 2

note      timestamps are not restored, and a rebuilt repository's own tree
          comes from git, which keeps only the execute bit

done      ~/recovered
```

`.env` comes back at content `DATABASE_URL=postgres://prod`, mode `0600` and its
extended attribute intact - git alone would have given back neither the file (it was
gitignored) nor, had it been tracked, anything stricter than `0644`. `notes.md`,
never committed anywhere, comes back too. The repository is a real repository again,
with both of its commits, not a directory of files that happen to match a snapshot.

## The problems this actually solves

- **Gitignored files are backed up too.** Tycho never consults `.gitignore` when
  deciding what to capture - that file exists to keep noise out of commits, and a
  backup tool that reused it for a different purpose would be the reason a `.env` or
  a private key silently had no copy anywhere. Untracked and uncommitted changes are
  captured the same way, in an overlay alongside each repository's own history.
- **The full permission mode and extended attributes survive a restore**, from a
  manifest written at capture time and replayed at restore - the `metadata` line
  above. A git tree records one bit per file, so without this every restored secret
  comes back world-readable regardless of what it left the source machine as.
  Ownership is attempted too, but only takes effect when the restore runs as root.
- **Restore refuses to write through a symlink.** `cp -R` follows a symlink in the
  destination and silently overwrites whatever it points at, fabricating a file that
  never existed on the source machine. Tycho reports the conflict and leaves both
  versions for you to resolve by hand instead.
- **Volumes that record no ownership - exFAT, FAT32, most external drives - are
  refused explicitly**, with the reason, rather than failing with a bare git error at
  push time or silently trusting a filesystem nothing vouches for. Trusting one is an
  opt-in per remote, never inferred.

## Install

```text
cargo install --path .
tycho __bootstrap            # installs, then writes shell completions
```

`__bootstrap` finds the checkout (from `TYCHO_SRC`, a bounded scan of the usual
roots, or the current directory), runs `cargo install`, writes completions for zsh,
bash, fish or PowerShell, patches the rc file once, and leaves a starter config so
the first command finds one. `--only-completions` refreshes completions without
rebuilding; `-d`/`--debug` builds from cargo's debug profile instead of release, for
faster iteration while working on Tycho itself.

The build sets `target-cpu=native`, so the binary is built for the machine that
builds it and is **not portable** to an older CPU. That is fine because installation
is from source; it would not be if a prebuilt binary were ever published.

## Getting started

The starter config `__bootstrap` writes has every profile key commented out - there
is nothing to watch and nowhere to send it until one exists - so the first
`config check` on a fresh install says exactly that, and exactly what to run next:

```text
$ tycho config check

  error   this config defines no profiles, so there is nothing to back up
          one command sets up a whole profile - what to watch, where to send
          it, and when to run:

            tycho profile add me \
              --watch ~/Documents \
              --remote drive=/Volumes/<your drive>/Backups \
              --schedule daily:12:00

          add --trust-ownership drive if that drive is exFAT or FAT32, and
          --local-only instead of --remote to keep the backup on this machine

1 error, 0 warnings
```

That error is the whole of getting started. Run the command it prints, then look
before writing anything and back up for real:

```text
tycho profile add me --watch ~/Documents \
  --remote drive=/Volumes/<your drive>/Backups --schedule daily:12:00
tycho config check                      # confirms it - every problem at once
tycho run --dry-run                     # the plan, before anything is written
tycho run                               # capture, commit, push
```

`--watch` and `--remote` both repeat, for more than one root or destination.
`--optional NAME` marks a `--remote` whose absence is a warning rather than a
failure - the removable drive you don't always have plugged in. `--schedule` takes
`daily:HH:MM`, `weekly:<weekday>:HH:MM`, `every:<N>h` or `every:<N>m`, and can be
added later with `tycho schedule set` instead. Exactly one of `--remote` or
`--local-only` is required: a profile with neither fails `config check` for the
same reason the empty starter file does, and `--dry-run` prints the block it would
write rather than writing it.

`--trust-ownership NAME` is not a formality on most external drives. exFAT and
FAT32 - what a drive usually ships formatted as - record no file ownership, and git
refuses to open a repository there unless told to trust it explicitly. Tycho checks
the destination's volume **at `profile add` time**, against the nearest ancestor
that already exists - so a backup folder that hasn't been created yet is still
checked against its drive - and refuses to write the remote without the flag,
rather than leaving that failure for the first scheduled run to discover silently.

`profile`, `remote` and `schedule` each keep working after the one-shot command:
`list`/`add`/`rm` for the first two, `show`/`set`/`off` for the third, so a mistake
here is not a config file you hand-edit to fix. A few of them refuse on purpose -
`remote rm` won't remove a profile's only remote without `--local-only` saying
that's deliberate, and `profile rm` won't remove a config's only profile, nor
touch one that still has a local store without being told `--keep-store` or
`--delete-store`:

```text
$ tycho remote rm drive -p me
error: 'drive' is the only remote on 'me'
  --> profile me
  |
  = note: removing it would leave this profile's backups on this machine alone
  |
  recovery:
    $ tycho remote add <name> <path>
        # point it at the replacement first
    $ tycho remote rm drive --local-only
        # or accept local-only backups
```

## Scheduling it

A `schedule` in the config is a declaration, not a trigger - nothing runs by
itself until the scheduler is told to. `tycho service install` is the deliberate
second step:

```text
tycho schedule set weekly:sunday:12:00 -p me
tycho service install                   # launchd or Task Scheduler
tycho status                            # what ran, where it went, what is behind
tycho doctor                            # everything that could be wrong, in one table
```

`service install` creates **two** agents: one per profile, running `tycho run
<profile>` on its own schedule, and one shared `catchup` agent running `tycho push
--all` every hour - see "If a drive goes away" below for why that exists.

The plist holds only the binary path, the arguments and the schedule; watched
roots, ignore/reinclude rules, remotes and `trust_ownership` are read fresh from
the config on every run, so most edits need nothing further done to the service:

| Change | Needs `tycho service restart`? |
| --- | --- |
| Watched roots, ignore/reinclude rules, remotes, `trust_ownership` | No - read fresh from the config on every run |
| A profile's `schedule` | Yes - it is baked into the installed agent |
| Adding or removing a profile | Yes - its agent has to be created or removed |

`tycho doctor` compares each installed agent's schedule against the config and
names the mismatch, so a forgotten restart does not have to be remembered.

## Your own excludes

```text
tycho ignore add '**/.audit' -p me            # a path or a glob
tycho reinclude add ~/Documents/.audit/keep -p me   # an exception back out of one
```

Precedence is deepest-match-wins, ties broken by tier: an explicit path beats a
glob beats the built-in list of junk (`node_modules`, `.DS_Store`, `target`, and
seventeen more - `use_default_ignores = false` turns it off). `tycho run
--dry-run` reports every rule that matched nothing, which is how a typo surfaces
before it costs gigabytes rather than after:

```text
$ tycho run --dry-run
excluded                                          reason
--------------------------------------------------------------------------
  **/.audit                                       glob rule
  ~/Documents/.audit/keep                         matched nothing
```

Resist the urge to add `.env` to that list. Capturing exactly what git itself will
never track - gitignored secrets included - is the reason this exists over `git
push`.

## If a drive goes away

A run commits to the local store *before* it attempts any push, so a drive that is
unplugged - or offline entirely - never costs the backup that was just taken:

```text
$ tycho run me
me  98420bb
  captured     1 added                                                 6 B
  written      in 0s                                               28.7 KB
error: behind 1 runs
  --> remote drive
```

The commit landed - `tycho history` shows it - and only the push failed, which is
why the run itself still exits non-zero: `doctor`'s overdue check skips any run
whose outcome was `Failed`, so a backup that never left the machine does not count
as one. That is the thesis this project exists to enforce, not a rough edge - it is
exactly the silent-failure mode of the shell script it replaces, described under
"Why" below, where every scheduled run failed for a year and nobody noticed.

The local store accumulates while the drive is away, and the hourly `catchup`
agent delivers all of it the moment the drive reappears - or catch up by hand with
`tycho push --all`. The push itself is `git push`, not a copy, so only the objects
the remote is missing actually transfer: the backup's own history (`refs/heads/*`)
pushes **fast-forward only**, while the history of any repository captured inside
it (`refs/tycho/*`) is forced, since that upstream can legitimately be rewritten.

One consequence is worth knowing before it happens to you: **deleting the local
store breaks continuity with the remote.** A fresh store shares no history with
what is already out there, so the next push is refused rather than silently
overwriting it:

```text
error: push rejected: error: atomic push failed for ref refs/heads/main. status: 5
error: failed to push some refs to '/Volumes/<your drive>/Backups/me.git'
hint: Updates were rejected because the remote contains work that you do not
hint: have locally. ...
```

Recover by re-cloning the store from the remote with `git clone --mirror`, into
the path `docs/architecture/store.md`'s path table names for your platform, then
`chmod 700` it - a mirror clone comes out group- and world-readable, and the store
refuses to hold gitignored content at anything looser than that - or by wiping the
remote folder for a clean first contact.

## Getting data back

Getting data back needs neither a schedule nor a config:

```text
tycho restore --store /path/to/backup.git --into ./recovered
```

That is the disaster path, and it reads no config file at all - on a replacement
machine there is nothing to read.

Full command reference: [`docs/architecture/cli.md`](docs/architecture/cli.md).
The whole lifecycle as terminal sessions, including this one and a full destroyed-machine
recovery: [`docs/walkthrough.md`](docs/walkthrough.md).

## Why

It replaces a 44-line shell script whose every logged scheduled run failed
silently: `git bundle verify` was called without `-C "$repo"`, launchd's working
directory is not a repository, and `set -e` aborted before anything was copied.
Nobody noticed, because a backup system that fails silently manufactures confidence
rather than removing it.

Three requirements come directly from that and from what followed:

- **Failure is loud.** Non-zero exits, red status, desktop notifications, and a
  `status --check` that a monitor can read without parsing output. A run in which
  no remote received the new commit is a failure, so "every run pushes" means
  something rather than being an aspiration.
- **Gitignored files are the precious ones.** A sync incident once deleted a
  repository's files; git restored everything except the one gitignored file. Tycho
  never consults `.gitignore` when deciding what to capture.
- **Local-only history is already dead.** An `rm -rf` destroyed a local-only branch
  permanently - Apple SSDs zero freed blocks within seconds and no recovery tool
  helps. Every run pushes.

## The name

Tycho Brahe (1546-1601) spent roughly twenty-one years, 1576 to 1597, at his
observatory Uraniborg on the island of Hven, taking systematic naked-eye
measurements of stellar and planetary positions before the telescope existed,
at a precision nobody could match. His own model of the solar system was
wrong. His data was not: when he died in 1601, his assistant Johannes Kepler
inherited the observations and derived the laws of planetary motion from
them, particularly the Mars data. The records outlived their maker and were
faithful enough that someone else could reconstruct deeper truth from them -
which is exactly what a backup is for. The Latinised spelling "Tycho" happens
to resemble the Greek τύχη, "fortune" - a coincidence picked up in
translation from the Danish Tyge, not an inherited meaning.

## Scope

Developer machines and company documents. Not a competitor to restic on storage
engineering for large binary datasets - point it at a 4K video library and you will
be disappointed. The store keeps full history forever, which is the right trade for
documents and source, and the wrong one for a media collection.

**Metadata is best-effort, not universal.** Mode and extended attributes round-trip
through a manifest on Unix, replayed on a full restore; ownership round-trips too,
but only takes hold when the restore runs as root. Timestamps never come back,
deliberately - see `docs/architecture/store.md` section 7. Restoring a single path
with `restore -- PATH` skips the manifest and gives back plain content, the same as
git itself would. On Windows there is no mode or uid to capture at all.

## Status

The design was audited before any code was written: four region auditors read the
doc set exhaustively and four adversarial verifiers re-ran the decisive experiments,
confirming 57 of 58 Critical and High findings. Every confirmed finding has been
fixed in the documents, and `docs/decisions.md` D13 to D16 record the four design
reversals that resulted. The audit workspace is at `.audit/`, which is gitignored.

## Docs

| File | Contents |
|---|---|
| [`docs/architecture/overview.md`](docs/architecture/overview.md) | Topology, module map, dependency rule, invariants |
| [`docs/architecture/config.md`](docs/architecture/config.md) | TOML schema and the watch/ignore rule tree |
| [`docs/architecture/store.md`](docs/architecture/store.md) | Store layout, ref namespaces, the commit pipeline, restore |
| [`docs/architecture/capture.md`](docs/architecture/capture.md) | What a run does, stage by stage |
| [`docs/architecture/remotes.md`](docs/architecture/remotes.md) | Push, verification, offline handling |
| [`docs/architecture/cli.md`](docs/architecture/cli.md) | Commands, flags, exit codes, output format |
| [`docs/architecture/scheduling.md`](docs/architecture/scheduling.md) | The service lifecycle: launchd on macOS, Task Scheduler on Windows |
| [`docs/decisions.md`](docs/decisions.md) | Why each choice, and what was rejected |
| [`docs/background.md`](docs/background.md) | The incidents behind the requirements, and the system being replaced |
| [`docs/git-primer.md`](docs/git-primer.md) | Objects, refs, refspecs and reachability - the git parts this design rests on |
| [`docs/disaster-recovery.md`](docs/disaster-recovery.md) | Recovering onto a new machine from a cloud folder, using only git |
| [`docs/build-plan.md`](docs/build-plan.md) | Layered build order from primitives upward, and the decisions that are settled |
| [`docs/walkthrough.md`](docs/walkthrough.md) | The whole lifecycle as terminal sessions: install, backup, restore one file, recover from nothing |

Start with the git primer if refs and refspecs are unfamiliar territory - everything
in `store.md` assumes it. Read disaster recovery before trusting any of it, since a
backup design is only worth what its restore path is.

## Development

    bash scripts/ci-check.sh

fmt, the gated pedantic clippy set, tests, doc and audit. Green before every commit.

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md). The short version: `bash
scripts/ci-check.sh` has to be green, and a change to what a backup does or how it
is recovered needs the matching document changed in the same commit.

## Licence

Apache-2.0. See [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE).

Chosen over MIT for its explicit limitation of liability, which matters more than
usual here: this is a backup tool, and its failure mode is somebody losing data.
Section 6 also makes clear that the licence grants no rights to the CoreEngineX name
or marks - the code is open, the brand is not.
