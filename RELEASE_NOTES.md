## v0.1.1

**Closes eight pre-release audit findings spanning checkout, config generation, and file permissions, stops backup runs from stamping wall-clock time into `REPO.txt`, and raises the pipe-buffer test's CI timeout budget.**

### Fixed

- **Unclassifiable overlay files vanished from a backup with no trace**: An overlay file that couldn't be classified at hash time was filtered out of the batch before it reached the tree, the unreadable list, or the commit message. Both existing safety nets count what survives that filter, so neither one could see the file go missing. The overlay is the gitignored content the store exists to protect, so this closed the gap rather than adding a third counter.
- **Files with unreadable modes restored world-readable**: A path whose mode couldn't be read at capture time was dropped from the manifest the same way. With no manifest record, the file restored at git's default `0644` — a `0600` file coming back world-readable on a run that reported success.
- **Branch names starting with `-` broke checkout**: `git update-ref` can create branch names (e.g. `-x`) that `git branch` itself refuses and `check-ref-format` accepts. Checkout used `--` before the branch name, which git parsed as a pathspec separator rather than an end-of-options marker, so checking out `-x` failed and the repo reported itself as having no commits despite its history sitting in `.git`. Switched to `--end-of-options` so any valid ref name checks out.
- **Backup interval could wrap into a schedule firing every few seconds**: An interval count was multiplied without an upper bound; in release builds the multiplication could wrap and produce a near-zero interval instead of the intended one.
- **Control bytes in remote paths could truncate the generated git config**: A control byte in a remote's path terminated a line early in the generated global git config, corrupting entries after it.
- **Profile names could resolve the store outside its own directory**: A profile name that was itself an absolute path resolved the store to that path instead of a subdirectory of the intended root — including on the command that offers to delete the store, making it possible to delete content outside the store's directory.
- **Predictable temp file names were open to a symlink attack**: A temp file was created under a predictable name and then opened, leaving a window for another process to plant a symlink at that path first.
- **`REPO.txt` timestamp caused every backup to report a false diff**: `REPO.txt` recorded `seen <now>`, so a backup that found nothing changed still rewrote the file and committed a diff for every captured repository on any run that crossed a minute boundary. The field was never read back on restore — `parse_repo_txt` matches known field names and ignores the rest — so this matches the no-mtime-churn policy `metadata.rs` already enforces elsewhere; when a capture happened is now read from the commit's own timestamp.

### Internal

- Raised the pipe-buffer hash-object test's CI timeout budget so a Windows runner with a virus scanner in the path of its ~10,000 file operations can finish within the ceiling; the test's guarantee (a genuine wedge still fails the gate) is unchanged.
