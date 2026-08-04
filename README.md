# tycho

<img src="docs/logo.svg" alt="Tycho" width="132">

A backup daemon that treats git as the storage format rather than as one of the
things being backed up.

Watched paths are captured into a bare git repository, one commit per run, with a
message a human can read. Directories that are themselves git repositories are
captured as repositories - their full history fetched into the store's object
database, plus an overlay holding the uncommitted, untracked and gitignored files
that history alone cannot restore. The store is then pushed to bare repositories in
synced cloud folders and on external drives, so every destination holds identical
history and going back is `git log` and a checkout.

**Complete, on macOS and Windows.** Every command in `docs/architecture/cli.md`
works: capture, push to remotes, verify, restore whole trees or single files or a
point in time, rebuild captured repositories with their history, the scheduled agent
on launchd or Task Scheduler, and `doctor`. The architecture is documented in full
under `docs/`, and `docs/build-plan.md` carries the order it was built in.

## Install

```text
cargo install --path .
tycho __bootstrap            # installs, then writes shell completions
```

`__bootstrap` finds the checkout (from `TYCHO_SRC`, a bounded scan of the usual
roots, or the current directory), runs `cargo install`, writes completions for zsh,
bash, fish or PowerShell, patches the rc file once, and leaves a starter config so
the first command finds one. `--only-completions` refreshes completions without
rebuilding.

The build sets `target-cpu=native`, so the binary is built for the machine that
builds it and is **not portable** to an older CPU. That is fine because installation
is from source; it would not be if a prebuilt binary were ever published.

## Quickstart

```text
tycho config init                       # a starter ~/.config/tycho/tycho.toml
tycho watch add ~/Documents             # what to back up
tycho config check                      # every problem at once, not the first
tycho run --dry-run                     # the plan, before anything is written
tycho run                               # capture, commit, push
```

Then add a `[[profile.remotes]]` block and a `schedule` to the config and:

```text
tycho service install                   # launchd or Task Scheduler
tycho status                            # what ran, where it went, what is behind
tycho doctor                            # everything that could be wrong, in one table
```

Getting data back needs neither a schedule nor a config:

```text
tycho restore --store /path/to/backup.git --into ./recovered
```

That is the disaster path, and it reads no config file at all - on a replacement
machine there is nothing to read.

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

**What a restore gives back is file contents, not filesystem state.** Permissions
beyond the execute bit, ownership, timestamps, extended attributes and ACLs are not
preserved, because git does not store them. A restored private key comes back
world-readable. `docs/architecture/store.md` section 7 has the full list.

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
