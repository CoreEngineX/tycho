# Disaster recovery

Your machine is gone. What do you actually have, and what do you type?

Every command in this document has been run end to end against a real store and a
real folder remote, and the recovered working tree came back byte-identical to the
original - including a gitignored file, which is the case git alone cannot restore.

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
      tycho/...       each captured repository's branches and tags
    HEAD  config  ...
  RECOVERY.md         these instructions, written by every run
```

There is no separate index, manifest or catalogue held anywhere else. One folder is
a complete, self-sufficient recovery source, which is the property that makes the
answer to "what if the laptop explodes" short.

**Tycho itself is not needed to recover.** The store is an ordinary git repository
and every step below is plain git. That is deliberate: a backup you can only read
with a tool you also have to rebuild first is a backup with a dependency you cannot
verify in advance.

## 2. What you actually depend on

Stated honestly, because these are the things that fail in a real disaster:

| Dependency | Risk |
|---|---|
| Access to the cloud account | If the password or its 2FA lived only on the destroyed machine, nothing below runs. Keep recovery credentials somewhere the machine's loss does not take with it |
| `git` installed | Trivial on any new machine, and the only software required |
| The folder having synced completely | A cloud client that was mid-upload when the machine died may hold a partial push. This is why there is more than one remote |
| Knowing this procedure exists | Which is why `RECOVERY.md` is written into the remote folder itself, next to the repository it describes |

The T7 external drive is the answer to the first and third rows: it depends on no
account and no network.

## 3. Recovery

### Step 1 - get the repository onto the new machine

Sign into the cloud account and let the folder sync, or download it from the web
interface. Then either copy it or mirror-clone it:

```text
cp -R "~/Library/CloudStorage/GoogleDrive-…/CoreEngineX-Backups/coreenginex.git" ~/store.git
```

```text
git clone --mirror "…/CoreEngineX-Backups/coreenginex.git" ~/store.git
```

**Use `--mirror`, never a plain `git clone`.** A plain clone fetches `refs/heads/*`
and `refs/tags/*` only. Every captured repository lives under `refs/tycho/*`, so a
plain clone silently leaves all of it behind and hands you a repository that looks
fine:

```text
plain clone:    refs/heads/main, refs/remotes/origin/main       <- history missing
mirror clone:   refs/heads/main, refs/tycho/…/heads/main, …     <- everything
```

Check what you got before trusting it:

```text
git -C ~/store.git fsck
git -C ~/store.git log --oneline
```

### Step 2 - see what is in there

```text
git -C ~/store.git log --oneline                       every backup, newest first
git -C ~/store.git ls-tree -r --name-only HEAD         every file in the latest one
git -C ~/store.git for-each-ref --format='%(refname)' refs/tycho
```

Note the ref pattern is `refs/tycho`, not `refs/tycho/*` - git matches a plain
prefix up to a slash, and the trailing `*` form returns nothing.

To list captured repositories rather than raw refs:

```text
git -C ~/store.git for-each-ref --format='%(refname)' refs/tycho \
  | sed -e 's|^refs/tycho/||' -e 's|/heads/.*$||' -e 's|/tags/.*$||' | sort -u
```

### Step 3 - recover the plain files and overlays

```text
mkdir ~/recovered
git -C ~/store.git archive HEAD | tar -x -C ~/recovered
```

For an older backup, use its commit instead of `HEAD`:

```text
git -C ~/store.git archive 8f2a10c | tar -x -C ~/recovered
```

You now have every watched plain file, plus each captured repository's `REPO.txt`
and `overlay/` directory holding its uncommitted, untracked and gitignored files.

### Step 4 - rebuild a captured repository with its history

Using the key from step 2, for example `CoreEngineX/org/handbook`:

```text
mkdir -p ~/recovered-repos/handbook
git init ~/recovered-repos/handbook
git -C ~/recovered-repos/handbook symbolic-ref HEAD refs/heads/__tycho_restore
git -C ~/recovered-repos/handbook fetch ~/store.git \
  "+refs/tycho/CoreEngineX/org/handbook/heads/*:refs/heads/*" \
  "+refs/tycho/CoreEngineX/org/handbook/tags/*:refs/tags/*"
git -C ~/recovered-repos/handbook checkout main
```

The `symbolic-ref` line is not optional and is the step people trip on. `git init`
leaves HEAD pointing at an unborn `refs/heads/main`, and git refuses to fetch into
a branch that is checked out:

```text
fatal: refusing to fetch into branch 'refs/heads/main' checked out at …
```

Parking HEAD on a name that will never exist lets the fetch write every branch as a
real local branch, which is what you want from a backup - not remote-tracking refs.

### Step 5 - put the overlay back on top

The checkout gives you committed history. It does not give you what was never
committed:

```text
cp -R ~/recovered/CoreEngineX/org/handbook/overlay/. ~/recovered-repos/handbook/
```

Now the working tree matches what was on the destroyed machine: tracked files at
their committed state, plus the uncommitted edits, untracked files and gitignored
files that git alone could never have brought back. `REPO.txt` in the same directory
records which branch or commit was checked out, including the detached and
no-commits-yet cases.

### Step 6 - restore the rest

Repeat steps 4 and 5 per repository, or use `tycho restore` once you have the
binary, which does all of it in one command:

```text
tycho restore coreenginex --at "2026-07-22 18:00" ~/recovered
```

## 4. Recovering onto a machine that is not yours

The store contains no absolute paths - files are stored under their root alias, not
under `/Users/<you>/…`. So the recovered tree drops anywhere, and the person
recovering it does not need the same username, the same home directory, or the same
operating system.

## 5. Testing this before you need it

An untested restore path is a hope. The quarterly drill, calendared:

1. Mirror-clone a remote into a temporary directory.
2. `git fsck` it.
3. Recover one repository through steps 3 to 5 and open the files.
4. Confirm a deliberately gitignored file came back.
5. Delete the temporary directory.

Fifteen minutes, four times a year, and it is the only thing that turns "we have
backups" into a statement about reality.
