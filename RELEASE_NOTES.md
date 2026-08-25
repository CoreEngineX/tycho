## v0.3.0

**Adds size reporting for backup runs that grow by more than 100 MB, fills gaps in the default junk list, and fixes misreported backup failures and UTF-16LE decoding.**

### Breaking

- **Minimum Rust version raised to 1.98**: Required for `String::from_utf16le_lossy`, which replaced the hand-rolled Windows UTF-16LE decoders in `schtasks::decode` and the volume ACL parser. Building from source now requires rustc 1.98 or newer.
  - Before: rustc 1.97.1
  - After: rustc 1.98

### Added

- **Large-growth reporting for backup runs**: A run whose backup grows by more than 100 MB now names the largest paths responsible, in both the run summary and the notification.
  - Reporting is best-effort and never blocks or refuses a run — unlike the shrink gate, there is no predicate here that can turn a large addition into a failure.
  - Git attribution is attempted for the reported paths, but a git failure only drops the attribution, never the run.

### Fixed

- **Default junk list was missing common build directories**: `cmake-build-debug` (CLion's default), `.tox`, `.nox`, `.cxx`, `.turbo`, `.parcel-cache`, `.angular`, `.idea`, and several object-code extensions were absent, so backups of projects using these tools included their entire build trees. Entries are now grouped by ecosystem to make future gaps easier to spot.
  - `*.d` and `.swiftpm` remain intentionally excluded: `*.d` collides with D's source extension, and `.swiftpm` can carry registry config worth keeping.
- **UTF-16LE decoders silently dropped a trailing odd byte**: `schtasks::decode` and the volume ACL parser now use `String::from_utf16le_lossy`, which emits U+FFFD for malformed input instead of discarding it. This matters because `schtasks::decode`'s output feeds an equality check that decides whether an installed task matches the config, so a truncated read must no longer compare equal.
- **Backup failure messages misreported the cause**: A rejected push (history diverged, Drive mounted the whole time) was reported as "could not reach gdrive," sending the reader toward a network problem that didn't exist. Only `RemoteState::TooFarBehind` now reports out-of-reach; other failures report their actual `RemoteState::Failed` reason.
- **`rules explain` suggested a command that couldn't answer the question**: It pointed readers at `tycho profile list`, which prints a count, when they needed to know which root is watched. It now points at `tycho watch list -p <name>`.

### Internal

- Added `scripts/junk-audit.sh` to diff the bundled junk list against the github/gitignore templates and surface upstream entries missing from it, for a maintainer to accept or reject.
- Documented that the junk list may exclude build output but must never exclude secrets, and added tests covering `*.jks`/`*.keystore` and `.env`/`.pypirc`.
- Brought the README up to date: corrected the junk-list entry count, documented the growth report, `rules explain`, and `ignore --local`, and fixed a stale Rust 1.97.1 reference in the build plan.
- Expanded the README with guidance on how `.tycho` differs from `.gitignore` and a five-part test for what belongs on an ignore list.
