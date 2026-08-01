# Walkthrough: install to disaster and back

The whole lifecycle as terminal sessions - install, first backup, a month of
ordinary use, recovering one damaged file, and recovering from a destroyed machine.

**This is the designed interface, not a recording of working software.** No code
exists yet. It is here so the UX can be argued with before it is built, and so the
build has something concrete to match. The git mechanics underneath every step in
part 6 have been executed for real and are recorded in `disaster-recovery.md`.

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

detected cloud folders, added as commented-out remotes:
  ~/Library/CloudStorage/GoogleDrive-Acct/My Drive
  ~/Library/CloudStorage/OneDrive-Personal

skipped, looks like an institutional account:
  ~/Library/CloudStorage/OneDrive-Work

edit the file, then run: tycho config check
```

The Work account is detected and deliberately left out. A backup tool that
helpfully starts writing company data into a university account would be doing
something nobody asked for.

Edit it into shape:

```toml
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
  { name = "gdrive",   path = "~/Library/CloudStorage/GoogleDrive-*/My Drive/CoreEngineX-Backups" },
  { name = "onedrive", path = "~/Library/CloudStorage/OneDrive-Personal/CoreEngineX-Backups" },
  { name = "t7",       path = "/Volumes/T7/tycho", optional = true },
]

schedule = { weekly = { day = "sunday", at = "12:00" } }
```

```text
$ tycho config check
coreenginex    2 roots, 1 ignore, 3 remotes, weekly Sun 12:00

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

excluded                                          reason
----------------------------------------------------------------------
  ~/Developer/CoreEngineX/scratch                 ignore rule
  **/node_modules                                 default junk
  **/target                                       default junk

----------------------------------------------------------------------
  would write   12 repositories                       8,538    1.53 GB
```

**Do not skip this on a new profile.** The store keeps history forever, so a missing
ignore rule cannot be fixed retroactively - only going forward. This is the moment
to notice that a 38 GB build cache would have been included.

## 4. First backup

```text
$ tycho run coreenginex
  capture   8,538 files in 12 repositories                     1.53 GB
  commit    8f2a10c  backup 2026-08-02 14:22 UTC
  push      gdrive      ok, verified                           1.53 GB
  push      onedrive    ok, verified                           1.53 GB
  push      t7          skipped, not mounted

done in 3m 41s, 1 remote behind
```

Exit code 0. The T7 is optional and unplugged, which is a lag, not a failure.

Put it on a schedule:

```text
$ tycho service install coreenginex
wrote   ~/Library/LaunchAgents/com.coreenginex.tycho.coreenginex.plist
wrote   ~/Library/LaunchAgents/com.coreenginex.tycho.catchup.plist
loaded  both agents

next backup   Sunday 2026-08-09 12:00
catch-up      on every mount, and hourly
```

Two agents. The first captures on the schedule. The second only pushes what already
exists, and exists so an unplugged drive catches up when you plug it in rather than
at the next weekly backup.

## 5. Ordinary use

Plug the T7 in. Nothing is typed - `StartOnMount` fires the catch-up agent:

```text
$ tycho status coreenginex
tycho 1.0.0

coreenginex                                   next run  Sun 12:00, in 6d 21h
  1 backup since 2026-08-02, newest today 14:22, store 1.4 GB

  gdrive     ok          pushed today 14:22            verified
  onedrive   ok          pushed today 14:22            verified
  t7         ok          pushed today 14:31            verified
```

A month later:

```text
$ tycho history coreenginex -n 4

when                commit    summary                              written
--------------------------------------------------------------------------
  today 12:00       3e01aa9   14 changed, 2 added, 3 repos moved     41 MB
  2026-08-23 12:00  b71f0c2   6 changed, 1 deleted                  8.2 MB
  2026-08-16 12:00  aa7d4e1   no changes                                0 B
  2026-08-09 12:00  1c93bb7   112 changed, 9 added                  204 MB
--------------------------------------------------------------------------
                              5 backups since 2026-08-02             1.4 GB
```

The `no changes` row is deliberate. A week with nothing to say still gets a commit,
because a gap in the history would be ambiguous between "nothing changed" and "the
backup did not run" - and a year of the second going unnoticed is why this exists.

## 6. One file goes wrong

A bad redirect empties a file:

```text
$ > ~/Developer/CoreEngineX/org/notes.md
$ wc -c ~/Developer/CoreEngineX/org/notes.md
       0 /Users/you/Developer/CoreEngineX/org/notes.md
```

Find which backups touched it:

```text
$ tycho history coreenginex --path CoreEngineX/org/notes.md

when                commit    summary                              written
--------------------------------------------------------------------------
  today 12:00       3e01aa9   notes.md changed                      4.1 KB
  2026-08-16 12:00  b71f0c2   notes.md changed                      3.8 KB
  2026-08-09 12:00  1c93bb7   notes.md added                        3.2 KB
```

Today's backup ran at 12:00 and the mistake happened after it, so today's copy is
good. Pull just that file out:

```text
$ tycho restore coreenginex CoreEngineX/org/notes.md ~/rescue
restored  1 file from 3e01aa9, backup of today 12:00
          ~/rescue/CoreEngineX/org/notes.md   4.1 KB

$ diff ~/rescue/CoreEngineX/org/notes.md ~/Developer/CoreEngineX/org/notes.md
$ cp ~/rescue/CoreEngineX/org/notes.md ~/Developer/CoreEngineX/org/notes.md
```

**Restore never writes back into your live tree.** It puts files in a destination
you name, and you do the copy. A restore that overwrote in place would be one typo
away from turning a one-file problem into a directory-sized one, and the whole point
of the tool is to not be the second incident.

Had the mistake happened before today's backup, the same command with `--at` picks
the last good one:

```text
$ tycho restore coreenginex --at "2026-08-16 12:00" CoreEngineX/org/notes.md ~/rescue
```

## 7. No internet on backup day

Sunday arrives, the machine is offline:

```text
$ tycho status coreenginex
coreenginex                                   next run  Sun 12:00, in 6d 23h
  6 backups since 2026-08-02, newest today 12:00, store 1.4 GB

  gdrive     ok          pushed today 12:00            verified
  onedrive   ok          pushed today 12:00            verified
  t7         behind 1    last seen 2026-08-24    optional, on next mount
```

Google Drive shows `ok` **because being offline did not stop the push**. The Drive
folder is a path on local disk, so pushing to it is a file write. The Drive client
holds the upload and sends it when the network returns. Nothing in Tycho retries,
waits, or reports a problem, because from its side nothing went wrong.

The honest limit: `verified` means the folder has the commit, not that Google's
servers do. There is no reliable way to ask a macOS file provider whether an upload
finished. This is a reason to keep more than one remote, and specifically to keep the
external drive - there is no sync client between Tycho and that disk.

The T7 is genuinely behind because it is unplugged. It catches up on mount:

```text
$ # plug the drive in, type nothing
$ tycho status coreenginex
  t7         ok          pushed today 16:04            verified
```

If it had been a signed-out cloud account instead, the hourly catch-up agent would
have covered it. Neither trigger ever captures - **capture happens on the backup
schedule and nowhere else**, so what is in a backup never depends on when you
happened to plug something in.

## 8. The machine is destroyed

New laptop, nothing on it. Sign into the Drive account and let the folder sync, or
download it from the web interface.

### With the binary

```text
$ cargo install --git https://github.com/CoreEngineX/tycho

$ tycho restore coreenginex \
    --store "~/Library/CloudStorage/GoogleDrive-*/My Drive/CoreEngineX-Backups/coreenginex.git" \
    ~/recovered

reading   coreenginex.git   6 backups, 2026-08-02 to 2026-08-30
using     3e01aa9  backup of 2026-08-30 12:00

restored  8,538 files                                          1.53 GB
restored  12 repositories with full history
          CoreEngineX/org                   main aef686f  + 1 untracked
          CoreEngineX/org/handbook      main 1930b99  clean
          CoreEngineX/products/a sibling project   dev  41c8ee2  + 3 modified
          ...

done in 4m 12s. ~/recovered
```

`--store` points at a remote rather than a local store, which is the whole recovery
case: there is no local store, because there is no local anything.

### Without the binary

Everything above is plain git, and this path is verified in `disaster-recovery.md`:

```text
$ git clone --mirror ".../CoreEngineX-Backups/coreenginex.git" ~/store.git
$ git -C ~/store.git fsck
$ mkdir ~/recovered && git -C ~/store.git archive HEAD | tar -x -C ~/recovered
```

That is every plain file plus every repository's overlay. Then per repository:

```text
$ git init ~/recovered-repos/handbook
$ git -C ~/recovered-repos/handbook symbolic-ref HEAD refs/heads/__tycho_restore
$ git -C ~/recovered-repos/handbook fetch ~/store.git \
    "+refs/tycho/CoreEngineX/org/handbook/heads/*:refs/heads/*" \
    "+refs/tycho/CoreEngineX/org/handbook/tags/*:refs/tags/*"
$ git -C ~/recovered-repos/handbook checkout main
$ cp -R ~/recovered/CoreEngineX/org/handbook/overlay/. ~/recovered-repos/handbook/
```

Two things that will bite if skipped, both found by actually running this:

- **`--mirror`, never a plain `git clone`.** Plain clone takes `refs/heads/*` and
  `refs/tags/*` only, so every captured repository is silently left behind and you
  get a repo that looks fine.
- **The `symbolic-ref` line.** `git init` leaves HEAD on an unborn `refs/heads/main`
  and git refuses to fetch into a checked-out branch.

`RECOVERY.md` sits beside the repository in the folder with these commands already
filled in for your profile, because in a real disaster every other copy of them was
on the disk that died.

### Back to normal

```text
$ tycho config init          # or restore your own config from ~/recovered
$ tycho run coreenginex --dry-run
$ tycho service install coreenginex
```

The new machine's first run pushes to the same remotes and continues the same
history, because the store it clones is the history.
