## v0.4.0

**Adds overdue-backup catch-up detection, matches virtualenvs by suffix during the sweep, and stops rewriting RECOVERY.md when its content hasn't changed.**

### Breaking

- **Optional remote past tolerance now fails the run**: A run with an optional remote that is past its tolerance window used to complete without failing. It now reports the run as failed, so automation that treated optional remotes as non-blocking will start seeing non-zero exits.
  - Before: optional remote past tolerance → run passes
  - After: optional remote past tolerance → run fails

### Added

- **Catch-up detection for stalled backups**: The catch-up agent now notices when a backup job has stopped running entirely, not just when it fails, closing a gap where a backup going silently stale was never flagged.

### Changed

- **RECOVERY.md only rewritten when its content changes**: Previously every run rewrote RECOVERY.md regardless of whether anything differed, producing spurious diffs and mtime churn. The write is now skipped when the generated content already matches what's on disk (RFC 002).
- **Documented that the sidecar sweep can't rescue a first exFAT push**: Clarified that if the very first push to a new exFAT volume fails, the sidecar sweep has no prior state to recover from and cannot bail you out — treat that first push as unprotected.

### Fixed

- **Virtualenv sweep missed venvs not literally named `.venv`**: The sweep only recognized a virtualenv when the directory was named exactly `.venv`, so venvs named by suffix (e.g. `foo.venv`) were left in place and swept into backups. The sweep now matches on suffix, verified post-rebuild against RFC 003.
