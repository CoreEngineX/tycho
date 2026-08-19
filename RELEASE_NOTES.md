## v0.2.0

**Adds a `rules explain` command and per-directory local rule files, wires `--local` writes and a cross-profile watch confirmation into rule management, and fixes dot resolution in typed path arguments plus duplicate hit counting.**

### Added

- **`tycho rules explain <path>`**: Names the rule that decided a path's status and every rule it beat, with the file and line each was declared at, walking the full ancestor chain.
- **`--local` flag for `ignore`/`reinclude`**: Writes the rule into the `.tycho/rules.toml` of the deepest still-captured ancestor instead of the operator config, so a re-include under an ignored directory lands in the one place it will actually be read.
- **Local rule files (`.tycho/rules.toml`)**: Any captured directory can carry rules scoped to its own subtree, discovered during the normal walk with no second traversal. Rules now survive the directory being renamed, moved, or cloned, since they no longer depend on an absolute path staying valid.
  - A skipped directory's rule file is never read, even when the walk passes through it for a deeper carve-out.
  - On a direct conflict, the operator's config still outranks a local file.
- **Default ignore list gained `out`, `.ruff_cache`, `.kotlin`, and `xcuserdata`**: Covers Next.js/AOSP/Electron Forge build output and IDE/tool caches that were previously backed up despite being regenerable. `.swiftpm` was deliberately left off since it can carry registry/mirror config worth keeping.

### Changed

- **Cross-profile nested watch now warns instead of proceeding silently**: Watching a root nested inside another profile's root triggers a warning and a Y/N confirmation prompt. Watching a root nested inside the *same* profile's root remains the hard error it already was.

### Fixed

- **Typed path arguments rejected valid `..` components**: `AbsPath`'s parent-component refusal exists to protect config-sourced paths, but it also blocked ordinary shell-style relative paths like `../foo`. Typed arguments now resolve dots before validation — physically where the path chain exists on disk, so `..` crosses symlinks the way the filesystem does, and lexically where it doesn't, so a hypothetical path still resolves. The final path component is never resolved, so a symlink is still asked about as itself. This applies to `--local` commands as well.
- **Duplicate hit recording for plain files**: `record_hits` ran twice per plain file — once in the listing loop, once inside `take_file` — inflating hit counts. The duplicate call was removed.

### Internal

- Fixed flaky lock-acquisition tests (brief retry, since macOS occasionally reports a just-released lock as still held) and serialized the parallel tests that write the global colour flag, removing an intermittent CI failure.
- Removed the unreachable `RedundantWatch` diagnostic, superseded by `NestedWatchedRoot`, and merged the duplicate `RemoteEntry`/`NewRemote` types into one.
- Rule resolution now carries each directory's `Decision` down the walk stack instead of re-deriving it per file, and stores rule references as arena ids rather than cloned text.
- Split the junk list into `Junk::Name` (compile-time-checked path components) and `Junk::Glob` (patterns), and switched `RuleSet::junk` to a `&'static` slice so building the rule tree no longer copies the list per profile.
