## v0.4.1

**Adds RFC 005 on ownership versus encryption, and fixes `__bootstrap` adding a redundant `compinit` to every zsh startup.**

### Added

- **RFC 005: ownership versus encryption**: Splits the privacy question (is the data encrypted?) from git's ownership question (who owns the files?), so a backup is no longer called exposed on ownership grounds alone.
  - A backup on an encrypted volume now gets a green row instead of being omitted from the analysis.
  - Reporting asks whether the volume is encrypted before calling a backup exposed.

### Fixed

- **`__bootstrap` appended a second `compinit` to every zsh startup**: The zsh path wrote its own `fpath` line and `compinit` to `.zshrc` behind a `# tycho completions` marker. Behind oh-my-zsh or any other `compinit`, that caused a second full completion-cache rebuild on every shell start, and deleting the block brought it back on the next install. Bootstrap now asks a fresh `zsh -i` whether `~/.zsh/completions` already loads and edits only when it does not.
  - If the directory is missing, the `fpath` line goes into `~/.zshenv`. A `compinit` is added to `.zshrc` only when nothing else runs one.
  - The old marker block is removed as an exact match once a probe has succeeded. If zsh cannot be run, nothing is edited.

### Internal

- Ran the zsh probe through `sys::process` like every other child process, and recorded the design as D19.
