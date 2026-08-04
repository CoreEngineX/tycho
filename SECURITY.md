# Security

## Reporting

Report privately through GitHub's **Report a vulnerability** button on the Security
tab, which opens a draft advisory only the maintainers can see. Please do not open a
public issue for anything exploitable.

Include what you ran, what you observed, and the platform and filesystem involved -
several of the sharpest issues found in this project so far were filesystem-specific
and invisible on the other platform.

## What is in scope, and what it protects

**The store holds gitignored content.** That is deliberate: the overlay exists so a
restore returns the `.env` file and the untracked key that history alone cannot. It
follows that the store is secret-bearing, and the guarantees around it are the ones
worth attacking:

- `Repo::open` refuses a store that anybody but its owner can read - `0o077` on Unix,
  the DACL on Windows - and refuses one on a volume that records no ownership at all,
  because there the mode is synthesised per reader and means nothing.
- Every git invocation against the store pins its config on the command line, so
  nothing is inherited from the ambient environment, and strips `GIT_DIR` and its
  family so an inherited variable cannot redirect a write.
- `trust_ownership` scopes git's `safe.directory` exception to Tycho's own
  invocations, rather than writing a permanent machine-wide entry.

**Failure must be loud.** A silent success on a backup that did not happen is treated
as a security-relevant bug here, not a papercut - the project exists because a
backup system failed quietly for a year. A path that reports success while writing
nothing, or an exit code of 0 on an incomplete restore, is worth reporting.

## Out of scope

- The backup's contents at rest on a remote. Tycho writes a bare git repository to a
  folder you choose; encrypting that volume is yours to decide, and `doctor` warns
  when a remote sits on a filesystem that cannot keep it private.
- Anything requiring an attacker who already has your user account, since they can
  read the watched files directly.
- `git` itself. Tycho shells out to it for every storage operation and inherits its
  behaviour; report those upstream.
