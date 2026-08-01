# tycho

A backup daemon that treats git as the storage format rather than as one of the
things being backed up.

Watched paths are captured into a bare git repository, one commit per run, with a
message a human can read. Directories that are themselves git repositories are
captured as repositories - their full history fetched into the store's object
database, plus an overlay holding the uncommitted, untracked and gitignored files
that history alone cannot restore. The store is then pushed to bare repositories in
synced cloud folders and on external drives, so every destination holds identical
history and going back is `git log` and a checkout.

**Design stage. No code yet.** The architecture is documented in full under `docs/`.

## Why

It replaces a 45-line shell script whose every scheduled run failed silently for
its entire life: `git bundle verify` was called without `-C "$repo"`, launchd's
working directory is not a repository, and `set -e` aborted before anything was
copied. Nobody noticed, because a backup system that fails silently manufactures
confidence rather than removing it.

Three requirements come directly from that and from what followed:

- **Failure is loud.** Non-zero exits, red status, desktop notifications, and a
  `status --check` that a monitor can read without parsing output.
- **Gitignored files are the precious ones.** A sync incident once deleted a
  repository's files; git restored everything except the one gitignored file. Tycho
  never consults `.gitignore` when deciding what to capture.
- **Local-only history is already dead.** An `rm -rf` destroyed a local-only branch
  permanently - Apple SSDs zero freed blocks within seconds and no recovery tool
  helps. Every run pushes.

## Scope

Developer machines and company documents. Not a competitor to restic on storage
engineering for large binary datasets - point it at a 4K video library and you will
be disappointed. The store keeps full history forever, which is the right trade for
documents and source, and the wrong one for a media collection.

## Docs

| File | Contents |
|---|---|
| [`docs/architecture/overview.md`](docs/architecture/overview.md) | Topology, module map, dependency rule, invariants |
| [`docs/architecture/config.md`](docs/architecture/config.md) | TOML schema and the watch/ignore rule tree |
| [`docs/architecture/store.md`](docs/architecture/store.md) | Store layout, ref namespaces, the commit pipeline, restore |
| [`docs/architecture/capture.md`](docs/architecture/capture.md) | What a run does, stage by stage |
| [`docs/architecture/remotes.md`](docs/architecture/remotes.md) | Push, verification, offline handling |
| [`docs/architecture/cli.md`](docs/architecture/cli.md) | Commands, flags, exit codes, output format |
| [`docs/architecture/scheduling.md`](docs/architecture/scheduling.md) | launchd integration and service lifecycle |
| [`docs/decisions.md`](docs/decisions.md) | Why each choice, and what was rejected |
| [`docs/background.md`](docs/background.md) | The incidents behind the requirements, and the system being replaced |

## Development

    bash scripts/ci-check.sh

fmt, the gated pedantic clippy set, tests, doc and audit. Green before every commit.
