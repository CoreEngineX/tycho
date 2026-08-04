# Disaster recovery

Your machine is gone. What do you actually have, and what do you type?

**Verification scope, stated precisely.** Every command below was executed against a
real store and a real folder remote on a local disk, in the literal form written
here, against **both** a repository that had a stash and one that did not, and the
recovered file contents came back byte-identical including a gitignored file. Three
things were **not** verified: the procedure against a real `~/Library/CloudStorage`
folder with dataless placeholders, recovery onto a machine other than this one, and
metadata - permissions, timestamps and extended attributes are **not** restored, and
"byte-identical" here means file contents only.

Two earlier versions of this document were wrong, in the same shape both times. The
first claimed it had been "run end to end" when the commands had only been run with
shell variables substituted for the literal paths; four of them did not work as
written. The second fixed those and was genuinely executed - against one repository,
which happened to have a stash, so the exact-ref stash refspec in step 5 passed. On
any repository without one it aborted the whole fetch and recovered nothing. Both
failures are the original script's un-`-C`'d `bundle verify`: a check that passed in
a context it was not actually checking.

## 1. What is in the cloud folder

Everything. A remote is a bare git repository, so the folder contains the complete
object database and every ref:

```text
CoreEngineX-Backups/
  coreenginex.git/
    objects/          every blob, tree and commit - plain files AND all captured
                      repository history, deduplicated
    refs/             or packed-refs
      heads/main      the backup history, one commit per run
      tycho/...       each captured repository's branches, tags, remotes and stash
    HEAD  config  ...
  personal.git/       a second profile, if one points here - fully independent
  RECOVERY.md         these instructions, covering every repo in the folder
```

A folder may hold more than one profile. Each `*.git` directory is a separate,
complete recovery source, so recover the one you need and ignore the rest.

There is no separate index, manifest or catalogue held anywhere else. One folder is
a complete, self-sufficient recovery source.

**Tycho itself is not needed to recover.** The store is an ordinary git repository
and every step below is plain git. That is deliberate: a backup you can only read
with a tool you must rebuild first is a backup with a dependency you cannot verify in
advance. If you do have the binary, section 6 is one command.

## 2. What you actually depend on

| Dependency | Risk |
| --- | --- |
| Access to the cloud account | If the password or its 2FA lived only on the destroyed machine, nothing below runs. Keep recovery credentials somewhere the machine's loss does not take with it |
| `git` installed | Trivial on any new machine, and the only software required |
| The folder having synced completely | A client that was mid-upload when the machine died may hold a partial push. See step 1 |
| Knowing this procedure exists | Which is why `RECOVERY.md` is written into the remote folder itself |

The external drive answers the first and third rows: it depends on no account and no
network.

## 3. Recovery

### Step 1 - get a complete copy onto the new machine

Sign into the cloud account and let the folder sync. **Force full materialisation
before copying anything.** On macOS, files under `~/Library/CloudStorage` are
dataless placeholders until read, so a copy taken mid-download produces a store with
a missing packfile and no obvious symptom:

```text
find ~/Library/CloudStorage/GoogleDrive-Acct/My\ Drive/CoreEngineX-Backups -type f -exec cat {} + > /dev/null
```

That reads every byte, which is what forces the provider to download them. It takes
as long as the backup is large.

Then copy or mirror-clone. Note the tilde is **outside** the quotes - inside them the
shell does not expand it and the command fails:

```text
cp -R ~/"Library/CloudStorage/GoogleDrive-Acct/My Drive/CoreEngineX-Backups/coreenginex.git" ~/store.git
```

```text
git clone --mirror ~/"Library/CloudStorage/GoogleDrive-Acct/My Drive/CoreEngineX-Backups/coreenginex.git" ~/store.git
```

**Use `--mirror`, never a plain `git clone`.** A plain clone fetches `refs/heads/*`
and `refs/tags/*` only. Every captured repository lives under `refs/tycho/*`, so a
plain clone silently leaves all of it behind and hands you a repository that looks
fine:

```text
plain clone:    refs/heads/main, refs/remotes/origin/main       <- history missing
mirror clone:   refs/heads/main, refs/tycho/…/heads/main, …     <- everything
```

Now verify before trusting it. If `fsck` reports anything, go back and re-copy - the
source folder was not fully materialised:

```text
git -C ~/store.git fsck
git -C ~/store.git log --oneline
```

`git log` succeeding is **not** sufficient. It exits 0 on a store whose objects are
missing; `fsck` is what catches that.

### Step 2 - restore attribute neutralisation

Do this before extracting anything:

```text
printf '* -text -diff -filter -ident -export-subst -export-ignore\n' > ~/store.git/info/attributes
```

`info/attributes` lives in the repository directory, not in the object database, so
it is neither pushed to remotes nor carried by a mirror clone. Without it, a
`.gitattributes` file that was itself backed up can make the extraction in step 4
silently drop files marked `export-ignore` and rewrite the line endings of text
files - at exit 0, while `git ls-tree` still lists the file that was never written.

### Step 3 - see what is in there

```text
git -C ~/store.git log --oneline                       every backup, newest first
git -C ~/store.git ls-tree -r --name-only HEAD         every file in the latest one
```

To list the captured repositories, derive the key from `REPO.txt`, which every
captured repository contributes to the tree:

```text
git -C ~/store.git ls-tree -r --name-only HEAD \
  | sed -n 's|^\.tycho/repos/\(.*\)/REPO\.txt$|\1|p' | sort
```

Do not derive keys by stripping ref names. The `refs/tycho/<key>/…` namespace also
contains `remotes/…` and `stash/…` entries, so a `sed` that only strips `/heads/` and
`/tags/` emits several wrong keys for every real one.

Read a repository's provenance - it records which branch or commit was checked out,
including the detached and no-commits-yet cases:

```text
git -C ~/store.git show HEAD:.tycho/repos/CoreEngineX/org/handbook/REPO.txt
```

### Step 4 - recover the plain files and overlays

```text
mkdir ~/recovered
git -C ~/store.git archive HEAD > ~/store.tar && tar -xf ~/store.tar -C ~/recovered
```

**Do not pipe `git archive` straight into `tar`.** In a pipeline the shell reports
only the last command's status, so a store with a missing object prints an error,
extracts **zero files**, and still exits 0. Writing the tar to a file first means the
`&&` actually gates on git succeeding. `set -o pipefail` is the alternative if you
prefer the pipe.

If git printed anything, the copy is incomplete - go back to step 1 and re-copy, or
use a different remote.

For an older backup, use its commit instead of `HEAD`:

```text
git -C ~/store.git archive 8f2a10c > ~/store.tar && tar -xf ~/store.tar -C ~/recovered
```

You now have every watched plain file, plus `.tycho/repos/<key>/REPO.txt` and
`.tycho/repos/<key>/overlay/` for each captured repository.

### Step 5 - rebuild a captured repository with its history

Using a key from step 3, for example `CoreEngineX/org/handbook`:

```text
mkdir -p ~/recovered-repos/handbook
git init ~/recovered-repos/handbook
git -C ~/recovered-repos/handbook symbolic-ref HEAD refs/heads/__tycho_restore
git -C ~/recovered-repos/handbook fetch ~/store.git \
  "+refs/tycho/CoreEngineX/org/handbook/heads/*:refs/heads/*" \
  "+refs/tycho/CoreEngineX/org/handbook/tags/*:refs/tags/*" \
  "+refs/tycho/CoreEngineX/org/handbook/remotes/*:refs/remotes/*" \
  "+refs/tycho/CoreEngineX/org/handbook/stashes/*:refs/tycho-stash/*"
git -C ~/recovered-repos/handbook checkout main
```

**On Windows the checkout rewrites line endings, and that is not damage.** Git for
Windows sets `core.autocrlf=true` in its system config, so `checkout main` above
lands CRLF in the working tree for a blob stored as LF. The recovered *objects* are
byte-exact - `git cat-file blob HEAD:file` proves it, and `tests/remote.rs` asserts
exactly that pair. If you need the working tree to match the stored bytes as well,
set the option on the repository after creating it:

```text
git -C ~/recovered-repos/handbook config core.autocrlf false
```

before the `checkout`. **Not `-c` on the `git init`**, which an earlier version of this
document said: `git init` rejects `-c` outright with `error: unknown switch 'c'`, so
following that instruction leaves you with no repository at all. Moving it in front of
the subcommand - `git -c core.autocrlf=false init` - is accepted and still does not
work, because `-c` applies to the invoking process and `init` touches no working tree;
the `checkout` two lines later is a different process and reads the system config
again. Measured under a simulated Git-for-Windows system config: only the `config`
form above leaves LF in the working tree.

The same applies to a `restore --bundle` handed to someone else: what their `git
clone` writes is their config's business, not the bundle's.

The `symbolic-ref` line is not optional and is the step people trip on. `git init`
leaves HEAD pointing at an unborn `refs/heads/main`, and git refuses to fetch into a
branch that is checked out:

```text
fatal: refusing to fetch into branch 'refs/heads/main' checked out at …
```

Parking HEAD on a name that will never exist lets the fetch write every branch as a
real local branch, which is what you want from a backup.

**Every refspec above is a glob, and that is not a stylistic choice.** A glob that
matches nothing is silently skipped, so a repository with no tags fetches its branches
regardless. A refspec naming one exact ref that is absent does the opposite: git
aborts the entire fetch with `couldn't find remote ref` and writes **nothing at all**,
not the branches, not the tags. Verified: an earlier version of this document put
`+refs/tycho/<key>/stash:refs/stash` in this list, which worked against the one
repository it was tested on - a repository that happened to have a stash - and failed
completely against every repository that did not.

So the stash's top entry, which is one exact ref rather than a pattern, is fetched on
its own, where failing means only that there was no stash:

```text
git -C ~/recovered-repos/handbook fetch ~/store.git \
  "+refs/tycho/CoreEngineX/org/handbook/stash:refs/stash"
```

Only the top entry can become `refs/stash`, because git's stash stack is a reflog and
cannot be rebuilt from refs. The rest arrive under `refs/tycho-stash/` from the glob
in the main fetch, and `REPO.txt` records how many there were.

**`git stash list` will be empty afterwards, and the stash is not lost.** That command
reads the *reflog* of `refs/stash`, and a fetch does not carry reflogs - so the stack
is invisible to it even though every commit is present. Verified: apply them by name.

```text
git -C ~/recovered-repos/handbook stash apply refs/stash
git -C ~/recovered-repos/handbook show refs/tycho-stash/1
```

### Step 6 - put the overlay back on top

The checkout gives you committed history. It does not give you what was never
committed:

```text
cp -R ~/recovered/.tycho/repos/CoreEngineX/org/handbook/overlay/. \
      ~/recovered-repos/handbook/
```

Now the working tree matches what was on the destroyed machine: tracked files at
their committed state, plus the uncommitted edits, untracked files and gitignored
files git alone could never have brought back.

**Check the copy afterwards.** A plain recursive copy has two failure modes here, and
both are silent or confusing:

- Where the checkout holds a **symlink** and the overlay a regular file of the same
  name, `cp` writes **through the link to its target**, creating a file that never
  existed on the source machine.
- Where one side is a **file** and the other a **directory**, `cp` fails partway with
  `Not a directory` or `Is a directory`, having already copied part of the tree.

`tycho restore` handles both by refusing type mismatches and not following symlinks.
Doing it by hand, inspect the overlay for symlink and type conflicts first, or copy
file by file.

## 4. Recovering onto a machine that is not yours

The store contains no absolute paths - files are stored under their root alias, not
under `/Users/<you>/…` - so the recovered tree drops anywhere and the person
recovering does not need the same username, home directory or operating system.

Two constraints do apply. **Timestamps are not restored**, and neither is ownership
unless you recover as root. Permissions and extended attributes are: a git tree
records one bit per file, so `tycho restore` replays them from
`.tycho/metadata.tsv`, which travels inside the backup. A recovery with plain git and
no Tycho gets the files at `0644` and `0755` - read that file and `chmod` from it, or
anything secret-bearing is world-readable until you do. And
**on Windows**, some names do not survive the restore. `config.md` section 10 has
the measured table and it is not the one this paragraph used to give: `<>:"|?*` and
control characters are refused outright, but reserved device names are created as
ordinary files, and trailing dots and spaces are **silently stripped** - a rename
rather than a refusal, which is worse, because a refusal is a report. `tycho restore`
names every path it could not write and exits 1; a manual `tar -x` does not, so read
the table before trusting one.

## 5. Testing this before you need it

An untested restore path is a hope. The quarterly drill, calendared:

1. Materialise and mirror-clone a remote into a temporary directory.
2. `git fsck` it.
3. Write `info/attributes` into the clone.
4. Recover one repository through steps 4 to 6 and open the files.
5. Confirm a deliberately gitignored file came back.
6. Delete the temporary directory.

Fifteen minutes, four times a year, and it is the only thing that turns "we have
backups" into a statement about reality.

## 6. With the binary, one command

```text
cargo install --git https://github.com/CoreEngineX/tycho

tycho restore --store ~/"Library/CloudStorage/GoogleDrive-Acct/My Drive/CoreEngineX-Backups/coreenginex.git" \
              --into ~/recovered
```

`--store` reads no config file at all and does not need a profile name - the store
names itself. It performs every step above: the `info/attributes` fix before anything
is extracted, the four-glob fetch per repository with the stash as a separate command,
the checkout of whatever branch `REPO.txt` recorded, and a type-safe overlay copy.

Two things it does that the manual procedure cannot. It **refuses a path that is not
a git repository** rather than reporting an empty backup, which matters because the
`--store` argument here is a long `~/Library/CloudStorage/…` path typed by hand on a
machine that has nothing else on it. And it **reads a store it does not own**: a
mirror clone is mode `0755`, and Tycho's own permission check - right for the live
store, which holds gitignored content - is deliberately not applied to a restore
source, because refusing to read one would protect a file the person recovering is
already holding in their hands.
