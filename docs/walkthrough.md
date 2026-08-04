# Walkthrough: install to disaster and back

The whole lifecycle as terminal sessions - install, first backup, ordinary use,
recovering one damaged file, and recovering from a destroyed machine.

**This is the designed interface, not a recording of working software.** No code
exists yet. It is here so the UX can be argued with before it is built, and so the
build has something concrete to match. The git mechanics underneath section 8 have
been executed for real and are recorded in `disaster-recovery.md`, with that
document's verification scope stated precisely at its top.

Timeline for every example below: installed Saturday 2026-08-01, weekly schedule on
Sunday at 12:00, "now" in sections 5 onward is Monday 2026-11-02.

---

## 1. Install

```text
$ cargo install --git https://github.com/CoreEngineX/tycho

$ tycho --version
tycho 1.0.0
```

From a local checkout instead:

```text
$ cargo install --path ~/Developer/system-tools/daemons/tycho
```

The binary lands in `~/.cargo/bin` and keeps every piece of state outside its
checkout, so deleting the source directory later breaks nothing.

## 2. First config

```text
$ tycho config init
wrote  ~/.config/tycho/tycho.toml

cloud folders found under ~/Library/CloudStorage, listed as commented-out remotes:
  GoogleDrive-Acct/My Drive
  OneDrive-Personal
  OneDrive-Work

None are enabled. Uncomment the ones you intend to write to.

edit the file, then run: tycho config check
```

Every detected folder is listed and **none is enabled**. An earlier draft had
`config init` silently skip the university account as "looks like an institutional
account" - an unspecified heuristic making a data-governance decision. Listing
everything and enabling nothing puts that decision where it belongs.

Edit it into shape:

```toml
version = 1

[[profile]]
name = "coreenginex"

watch = [
  "~/Developer/CoreEngineX",
  "~/Books",
]

ignore = [
  "~/Developer/CoreEngineX/scratch",   # transient, regenerated constantly
]

remotes = [
  { name = "gdrive",   path = "~/Library/CloudStorage/GoogleDrive-Acct/My Drive/CoreEngineX-Backups" },
  { name = "onedrive", path = "~/Library/CloudStorage/OneDrive-Personal/CoreEngineX-Backups" },
  { name = "t7",       path = "/Volumes/T7/tycho", optional = true },
]

schedule = { weekly = { day = "sunday", at = "12:00" } }
```

The Google Drive path is written out in full rather than globbed. A glob matching
more than one account directory is a hard error, and this machine has two OneDrive
accounts, so the habit is worth forming.

```text
$ tycho config check
coreenginex    2 roots, 1 ignore, 0 reincludes, 3 remotes, weekly Sun 12:00

ok, no errors
```

That echo is worth reading rather than skipping. It is how a remote attached to the
wrong profile, or a schedule you thought you set, becomes visible.

## 3. Look before the first backup

```text
$ tycho run coreenginex --dry-run

roots                                                 files       size
----------------------------------------------------------------------
  CoreEngineX   ~/Developer/CoreEngineX               8,412    1.19 GB
  Books         ~/Books                                 126     340 MB

repositories                        head          state
----------------------------------------------------------------------
  CoreEngineX/org                   main aef686f  1 untracked
  CoreEngineX/org/handbook      main 1930b99  clean
  CoreEngineX/products/a sibling project   dev  41c8ee2  3 modified
                                                  and 9 more

excluded                                          reason
----------------------------------------------------------------------
  ~/Developer/CoreEngineX/scratch                 ignore rule
  **/node_modules                                 default junk
  **/target                                       default junk

----------------------------------------------------------------------
  to read      8,538 files in 12 repositories        1.53 GB
  to write     estimated new objects                  412 MB
```

**Do not skip this on a new profile.** The store keeps history, so a missing ignore
rule cannot be fixed retroactively - only going forward. This is the moment to
notice that a 38 GB build cache would have been included, or that an ignore path you
typed is in the `matched nothing` list.

Twelve repositories from two roots, because repository discovery is recursive:
`org` is itself a repository and its four submodules are found inside it.

## 4. First backup

```text
$ tycho run coreenginex
  capture   8,538 files in 12 repositories                     1.53 GB
  commit    8f2a10c  backup 2026-08-01 14:22 UTC
  push      gdrive      ok, all refs verified                  412 MB
  push      onedrive    ok, all refs verified                  412 MB
  push      t7          skipped, not mounted

done in 3m 41s, 1 remote behind
```

Exit code 0. The T7 is optional and unplugged, which is a lag, not a failure.

Put it on a schedule:

```text
$ tycho service install coreenginex
created ~/Library/Logs/tycho/
wrote   ~/Library/LaunchAgents/com.coreenginex.tycho.profile.coreenginex.plist
wrote   ~/Library/LaunchAgents/com.coreenginex.tycho.catchup.plist
loaded  both agents

next backup   Sunday 2026-08-02 12:00
catch-up      on every mount, and hourly
```

Two agents. The first captures on the schedule, one per profile. The second only
pushes what already exists, so an unplugged drive catches up when you plug it in
rather than at the next weekly backup.

The `profile.` infix in the label is not decoration - without it a profile named
`catchup` would produce exactly the second agent's label and one would silently
displace the other.

## 5. Ordinary use

Plug the T7 in. Nothing is typed - `StartOnMount` fires the catch-up agent:

```text
$ tycho status coreenginex
tycho 1.0.0

coreenginex                                    next run  Sun 12:00, in 6d 2h
  14 backups since 2026-08-02, newest yesterday 12:00, store 1.2 GB

  gdrive     ok             pushed yesterday 12:00                  verified
  onedrive   ok             pushed yesterday 12:00                  verified
  t7         ok             pushed today 09:31                      verified
```

```text
$ tycho history coreenginex -n 4

when                commit    summary                              written
--------------------------------------------------------------------------
  yesterday 12:00   8f2a10c   14 changed, 2 added, 3 repos moved     41 MB
  2026-10-26 12:00  1c93bb7   no changes                               0 B
  2026-10-19 12:00  aa7d4e1   112 changed, 9 added, 1 deleted       204 MB
  2026-10-12 12:00  4e10f92   2 changed                             1.1 MB
--------------------------------------------------------------------------
                              14 backups since 2026-08-02           1.2 GB
```

The `no changes` row is deliberate. A week with nothing to say still gets a commit,
because a gap in the history would be ambiguous between "nothing changed" and "the
backup did not run" - and a year of the second going unnoticed is why this exists.

## 6. One file goes wrong

A bad redirect empties a file inside the `org` repository:

```text
$ > ~/Developer/CoreEngineX/org/notes.md
$ wc -c ~/Developer/CoreEngineX/org/notes.md
       0 ~/Developer/Acme/org/notes.md
```

**This file lives inside a captured repository**, which is the normal case here and
the case that decides how the restore works. Its committed content is not a path in
the store tree at all - it is in the object database under `refs/tycho/*`. Tycho
resolves that automatically, and says which of the three sources answered:

```text
$ tycho history coreenginex --path CoreEngineX/org/notes.md

resolved  CoreEngineX/org  is a captured repository
          notes.md is tracked and clean, so this is that repository's history

when                commit    summary
--------------------------------------------------------------------------
  2026-10-28 16:41  3e01aa9   notes: add the Q4 filing dates
  2026-09-14 09:02  b71f0c2   notes: correct the CRA reference
  2026-08-11 11:20  1c93bb7   notes: first draft
```

Those are the repository's own commits, not backup runs - which is what you want,
because "the version before I broke it" is a commit in your repo, not a Sunday.

```text
$ tycho restore coreenginex -- CoreEngineX/org/notes.md --into ~/rescue
resolved  repository CoreEngineX/org, tracked file, from 3e01aa9
restored  1 file                                              4.1 KB
          ~/rescue/CoreEngineX/org/notes.md

$ diff ~/rescue/CoreEngineX/org/notes.md ~/Developer/CoreEngineX/org/notes.md
$ cp ~/rescue/CoreEngineX/org/notes.md ~/Developer/CoreEngineX/org/notes.md
```

Had the file been uncommitted, untracked or gitignored, the same command would have
answered from the overlay instead and said so - `from the overlay, backup of
2026-11-01 12:00`. That distinction matters: the overlay holds what was on disk at
backup time, while repository history holds what you committed.

**Restore never writes back into your live tree.** It puts files where you name and
you do the copy. A restore that overwrote in place would be one typo away from
turning a one-file problem into a directory-sized one.

Note what the copy does not carry back: **permissions, timestamps and extended
attributes are not restored**. For a markdown file that is irrelevant; for a private
key it means re-securing the file after the copy.

## 7. No internet on backup day

Sunday arrives, the machine is offline:

```text
$ tycho status coreenginex
coreenginex                                    next run  Sun 12:00, in 6d 2h
  15 backups since 2026-08-02, newest today 12:00, store 1.2 GB

  gdrive     ok             pushed today 12:00                      verified
  onedrive   ok             pushed today 12:00                      verified
  t7         behind 1 of 4  last seen 2026-11-02     optional, on next mount
```

Google Drive shows `ok` **because being offline did not stop the push**. The Drive
folder is a path on local disk, so pushing to it is a file write; the Drive client
holds the upload and sends it when the network returns. Nothing in Tycho retries or
reports a problem, because from its side nothing went wrong.

The honest limit: `verified` means every ref is present in the folder at the right
sha, not that Google's servers have it. There is no reliable way to ask a macOS file
provider whether an upload finished. That is a reason to keep more than one remote,
and specifically to keep the external drive, which has no client in between.

The T7 is genuinely behind because it is unplugged, and `behind 1 of 4` shows the
distance to failure. It catches up on mount:

```text
$ # plug the drive in, type nothing
$ tycho status coreenginex
  t7         ok             pushed today 16:04                      verified
```

If it had been a signed-out cloud account, the hourly catch-up would have covered
it. Neither trigger ever captures - **capture happens on the backup schedule and
nowhere else**, so what is in a backup never depends on when you plugged something
in.

And if the machine had been **powered off** rather than asleep across Sunday noon,
launchd would not have fired at all - its catch-up promise covers sleep only. The
next invocation of any agent notices the backup is overdue and says so:

```text
$ tycho status coreenginex
coreenginex                                     OVERDUE  last run 9d ago
  expected weekly, last successful run 2026-10-25 12:00
```

## 8. The machine is destroyed

New laptop, nothing on it. Sign into the Drive account and let the folder sync.

### With the binary

```text
$ cargo install --git https://github.com/CoreEngineX/tycho

$ tycho restore --store ~/"Library/CloudStorage/GoogleDrive-Acct/My Drive/CoreEngineX-Backups/coreenginex.git" \
                --into ~/recovered

reading   coreenginex.git   15 backups, 2026-08-02 to 2026-11-08
using     3e01aa9  backup of 2026-11-08 12:00 -0400  (16:00 UTC)

restored  8,538 files                                          1.53 GB
restored  12 repositories with full history
          CoreEngineX/org                   main aef686f  overlay: 1 untracked
          CoreEngineX/org/handbook      main 1930b99  overlay: clean
          CoreEngineX/products/a sibling project   dev  41c8ee2  overlay: 3 modified
          and 9 more

note      file permissions, timestamps and extended attributes are not
          restored - re-secure anything secret-bearing

done in 4m 12s. ~/recovered
```

`--store` points at a remote rather than a local store and reads no config file at
all, which is the whole recovery case: there is no local store, because there is no
local anything. The per-repository overlay counts come from each `REPO.txt`.

### Without the binary

Everything is plain git, and the full procedure is `disaster-recovery.md`. The
outline:

```text
$ find ~/Library/CloudStorage/GoogleDrive-Acct/My\ Drive/CoreEngineX-Backups -type f -exec cat {} + > /dev/null
$ git clone --mirror ~/"Library/CloudStorage/GoogleDrive-Acct/My Drive/CoreEngineX-Backups/coreenginex.git" ~/store.git
$ git -C ~/store.git fsck
$ printf '* -text -diff -filter -ident -export-subst -export-ignore\n' > ~/store.git/info/attributes
$ mkdir ~/recovered && git -C ~/store.git archive HEAD > ~/store.tar && tar -xf ~/store.tar -C ~/recovered
```

Five things bite if skipped, and all five were found by actually running this:

- **The `find` materialisation pass.** CloudStorage files are dataless placeholders,
  so a copy taken mid-download silently produces a store with missing objects.
- **`--mirror`, never plain `git clone`.** Plain clone takes `refs/heads/*` and
  `refs/tags/*` only, so every captured repository is left behind and you get a
  repository that looks fine.
- **`fsck`, not `git log`.** `log` exits 0 on a store whose objects are missing.
- **`info/attributes` before extracting.** It does not survive a mirror clone, and
  without it a backed-up `.gitattributes` can make `archive` drop files and rewrite
  line endings at exit 0.
- **`archive > file && tar`, never a pipe.** A pipeline reports only the last
  command's status, so a broken store extracts zero files and still exits 0.

`RECOVERY.md` sits beside the repository in the folder with these commands already
filled in for your profile, because in a real disaster every other copy of them was
on the disk that died.

### Back to normal

```text
$ tycho config init          # or restore your own from ~/recovered/.tycho/config.toml
$ tycho run coreenginex --dry-run
$ tycho service install coreenginex
```

The config that produced these backups is itself in the store at
`.tycho/config.toml`, so the definition of what was being protected survives with
the data.

The new machine's first run continues the same history, because the store it clones
is the history. Give it a **different profile name** if the old machine still exists
and still pushes - one profile name means one machine, and two machines pushing the
same name into one folder is a rejected push, not a merge.
