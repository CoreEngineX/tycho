## v0.2.1

**Fixes `..` path resolution to use lexical parsing instead of filesystem canonicalization.**

### Fixed

- **Path resolution no longer canonicalizes `..`**: On Windows, `canonicalize` folded `..` away before checking that intermediate directories existed, and the resolved path carried a `\\?\` verbatim prefix that no config entry matched — so `rules explain ..\x` reported that no watched root contained a path that plainly was inside one. Resolution is now purely lexical, matching how tycho treats paths everywhere else.
  - The walk now stops at a symlink instead of following it, since the store holds the literal tree rather than a resolved one.
  - A test pins the no-canonicalize behavior on both platforms.
