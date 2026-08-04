# CLI

Colored, aligned, human-first. Not a TUI - no alternate screen, no key handling,
nothing that stops being useful when piped into a log file.

## 1. Commands

```text
tycho run [PROFILE] [--all] [--dry-run] [--quick] [--allow-shrink] [--config PATH]
tycho push [PROFILE] [--all]
tycho status [PROFILE] [--check] [--strict]
tycho history [PROFILE] [-n N] [--path PATH]
tycho restore [PROFILE] [--store PATH] [--at TIME] [--bundle] [--force] [-- PATH ...] --into DEST
tycho watch add|rm|list [-p PROFILE] [PATH]
tycho ignore add|rm|list [-p PROFILE] [PATTERN]
tycho reinclude add|rm|list [-p PROFILE] [PATH]
tycho config check|path|init [--force] [--config PATH]
tycho service install|uninstall|status|restart [PROFILE]
tycho doctor [--deep] [--remote NAME]
tycho probe-access PATH...
tycho log [PROFILE] [-f]

global: --no-color
```

| Command | Does |
| --- | --- |
| `run` | Capture, commit, push. `--dry-run` prints the plan and stops before touching the store. `--quick` omits the repository table, the expensive half. `--allow-shrink` accepts a large drop in entry count |
| `push` | Push what the store already holds to any remote that is behind. Never captures. Exits 0 immediately if the profile lock is held |
| `status` | Per profile: next scheduled run, store size and backup count, one line per remote. `--check` for monitoring |
| `history` | The store's commits, rendered. `--path` limits it to backups that touched one path |
| `restore` | Recover to a destination directory. See section 7 |
| `watch` / `ignore` / `reinclude` | Rule management, editing the config in place |
| `config` | Validate, locate, or create the config file |
| `service` | Scheduler lifecycle for the per-profile backup agents and the shared catch-up agent - launchd on macOS, Task Scheduler on Windows |
| `doctor` | Environment, service, remotes, volumes and object-database health |
| `probe-access` | Internal. Run by `doctor --deep` through the scheduler to measure the agent's own Full Disk Access grant. macOS only, since TCC is. Not for direct use |
| `log` | Tail the log file without needing to know where it lives |

`--all` conflicts with an explicit PROFILE. **`--all` is the manual and scripted
form; the installed agents run `tycho run <profile>` one per profile**, per
`scheduling.md`.

`-p/--profile` is a flag on `watch`, `ignore` and `reinclude` rather than a
positional, because `tycho watch add PATH` and `tycho watch add PROFILE PATH` are
otherwise indistinguishable to a parser and to a reader.

### What each command writes

| Command | Writes to |
| --- | --- |
| `run` | store, state file, remotes, log |
| `push` | remotes, state file, and the store, since `gc --auto` runs |
| `restore` | the destination directory only |
| `config init`, `watch`, `ignore`, `reinclude` | the config file |
| `service` | `~/Library/LaunchAgents`, and the log directory on install |
| `status`, `history`, `doctor`, `log`, `config check|path` | nothing |

`push` touching the store is why it takes the same profile lock as `run`.

## 2. Output rules

Every command that prints more than one row uses the same column model, so the tool
reads as one thing rather than a pile of ad-hoc formatters.

- **A fixed column spec per table**, shared by header, rule and rows, so a header
  label sits directly over its own column.
- **Text left, numbers right**, so digits line up on their ones place.
- **Rows indent two spaces, section headers do not.**
- **A rule spans exactly the table's width.**
- **Total width 74 columns**, which fits an 80-column terminal.
- **State words are lowercase and short.** Shouting is what colour is for.
- **Meaning never depends on colour.** Green, yellow and red add emphasis to a word
  that already says the same thing, because this output is read through pipes and in
  log files as often as in a terminal.

Colour switches off automatically when stdout is not a terminal, and both `NO_COLOR`
and `--no-color` are honoured.

The examples below all come from one fixed scenario - Monday 2026-11-02 09:14 local,
`coreenginex` on a weekly Sunday-12:00 schedule since 2026-08-02, `second-company`
daily at 18:00 since 2026-09-21. **That scenario is the renderer's golden-test
fixture**, so the documentation and the tests cannot drift apart.

## 3. `tycho status`

```text
tycho 1.0.0

coreenginex                                    next run  Sun 12:00, in 6d 2h
  14 backups since 2026-08-02, newest yesterday 12:00, store 1.2 GB

  gdrive     ok             pushed yesterday 12:00                  verified
  onedrive   ok             pushed yesterday 12:00                  verified
  t7         behind 3 of 4  last seen 2026-10-11     optional, on next mount

second-company                              next run  today 18:00, in 8h 46m
  6 backups since 2026-09-21, newest yesterday 18:00, store 84 MB

  onedrive   failed         push rejected yesterday 18:00
                            tycho log second-company
```

Profile name and next run sit on one line at opposite edges, so a glance down the
left lists your profiles and a glance down the right says when each fires. The store
summary is a subtitle rather than a row, because it describes the profile rather
than being one destination among several.

`behind 3 of 4` shows the lag against that remote's `behind_tolerance`, so the
distance to failure is visible rather than implied.

`verified` appears only when the full ref-set comparison in `remotes.md` section 3
actually passed - never as decoration. It is what distinguishes a verified push from
`ok`, which means the push command returned success.

**The store size is the value recorded in the state file at the end of the last
run**, not a live measurement. Walking the object database on a command people run
casually would make `status` slow in proportion to the size of the backup, and on a
remote it would materialise dataless files. `doctor` computes the live figure.

**`status` must render when everything else is broken**, so its degraded modes are
specified rather than hoped for. With an unreadable config it prints a
`config unreadable` banner, omits the next-run column, and shows remotes from the
last run record labelled `as of <timestamp>`. The state file is an observation log,
never a competing definition of what should exist.

## 4. `tycho history`

```text
when                commit    summary                              written
--------------------------------------------------------------------------
  yesterday 12:00   8f2a10c   14 changed, 2 added, 3 repos moved     41 MB
  2026-10-26 12:00  1c93bb7   no changes                               0 B
  2026-10-19 12:00  aa7d4e1   112 changed, 9 added, 1 deleted       204 MB
  2026-10-12 12:00  4e10f92   2 changed                             1.1 MB
--------------------------------------------------------------------------
                              14 backups since 2026-08-02           1.2 GB
```

`written` is new objects for that run, rounded to the displayed unit - so the
`no changes` row reads `0 B` honestly even though the run did write a commit object
of a couple of hundred bytes. It is read out of each commit's own message rather
than from the state file, so this command works from a bare clone on a replacement
machine; `store.md` section 8 covers why.

Recent timestamps render as `today` and `yesterday`, older ones as dates.

`history --path` has **two answer shapes** and says which it is showing, because a
path can live in either half of the store:

- a **store path** - a plain file, or an overlay entry - answers from the store's own
  commits, as above
- a path **inside a captured repository** answers from that repository's history
  under `refs/tycho/<key>/*`, showing that repository's commits rather than backup
  runs, with a header naming the repository

Section 7 specifies how a path is resolved to one or the other.

## 5. `tycho doctor`

```text
environment
--------------------------------------------------------------------------
  git                     ok        2.55.0
  config                  ok        2 profiles, no errors
  log directory           ok        ~/Library/Logs/tycho, writable
  notifications           warn      authorised, delivery not tested, use --deep
  full disk access        warn      not measured, use --deep

coreenginex
--------------------------------------------------------------------------
  agent                   ok        loaded, last exit 0, next Sun 12:00
  agent schedule          ok        matches config
  store                   ok        1.2 GB, 14 backups, HEAD resolves
  refs                    ok        3,412 refs, packed
  gdrive                  ok        all refs present, verified
  onedrive                ok        all refs present, verified
  t7                      warn      behind 3 of 4, optional

second-company
--------------------------------------------------------------------------
  agent                   fail      loaded, last exit 1, next today 18:00
  onedrive                fail      push rejected, remote ahead by 2
  sync artifacts          fail      onedrive: pack file 'main (1).pack'

volumes
--------------------------------------------------------------------------
  /                       ok        84 GB free
  /Volumes/T7             warn      12 GB free, 2 profiles, 41 GB stored

3 failures, 4 warnings
```

One check per row, one verdict word, evidence beside it.

The `agent` row carrying `last exit 1` is deliberate: a non-zero last exit shown by
`launchctl list` was the evidence that revealed a year of silent failure in the old
system, and nobody had reason to look. **`agent schedule`** compares the installed
plist against the config, because drift between them is what killed that system.

`volumes` is grouped by disk rather than by profile because that is how the
constraint works - several profiles can share a drive, and the store keeps full
history, so free space is what they contend for.

Two rows say `use --deep` rather than guessing. Full Disk Access cannot be measured
from an interactive process, and notification delivery cannot be measured without
sending one; `scheduling.md` sections 7 and 8 describe both probes. `--deep` also
upgrades the object check from `fsck --connectivity-only` to a full one.

The `schedule` row is the overdue check from `scheduling.md` section 1, and it is red
on its own: a backup that did not happen is worse than one that happened and could
not be pushed. A profile that has never run successfully is overdue from the moment
it is configured.

**Colour is emphasis, never meaning.** Every verdict is the word `ok`, `warn` or
`fail` before it is a colour, so the report reads identically through a pipe, in a
log file, and to somebody who cannot distinguish the two. `--no-color`, `NO_COLOR`
and a stdout that is not a terminal each turn it off - and under launchd stdout is a
file, so an agent's log never contains an escape sequence.

## 6. Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success, including a run where an optional remote is merely unplugged |
| 1 | Failure: a run failed, a required remote is unreachable, the config has errors, `status --check` / `doctor` found red, or a `restore` did not write every path its backup holds |
| 3 | Yellow only: `status --check --strict`, `doctor`, or a `restore` that refused an overlay file rather than resolving it |
| 2 | Usage error, which clap emits |

**The two `restore` rows are the distinction, not a duplication.** A refused overlay
file is yellow because the material is still in the staging tree and can be copied out
by hand. A path the extraction could not write is not anywhere, so it is red. That
separation is the reason 3 exists at all, and `restore` originally exited 0 for the
red case while reserving 3 for the yellow one - a script that deletes the original on
success read that as success.

`status --check` exits non-zero on **red only** by default; `--strict` makes yellow
non-zero too. This is what makes `run` and `status --check` agree: an optional remote
merely unplugged is yellow and exits 0 in both, while an optional remote past its
`behind_tolerance` is red in both. Without that alignment a weekly monitor cries wolf
every time a drive is unplugged, and a monitor that cries wolf is a monitor people
turn off.

`doctor` follows the same policy, so the one command that aggregates health cannot
report a failure while exiting 0.

## 7. Restore

```text
tycho restore coreenginex --at "2026-10-19 12:00" --into ~/recovered
tycho restore coreenginex --at "3 days ago" -- CoreEngineX/org/notes.md --into ~/rescue
tycho restore coreenginex --bundle -- CoreEngineX/org/handbook --into ~/rescue
tycho restore --store "…/CoreEngineX-Backups/coreenginex.git" --into ~/recovered
```

The destination is the named flag `--into`, and paths come after `--`. A trailing
positional destination cannot be parsed unambiguously against a variable-length path
list, and a restore that guessed which argument was the destination would be a
restore that could write to the wrong place.

`--into` is required. There is no default and no fallback.

**`--store` reads no config file at all.** It points at a store or a remote directly,
and PROFILE becomes optional because the store names itself. That is the disaster
case: on a replacement machine there is no config, no state file and no local store,
and a restore that required a profile from a config would be unusable exactly when it
is needed. `disaster-recovery.md` exercises this form.

`--at` accepts an absolute timestamp or a relative expression, **interpreted in local
time unless it carries an explicit offset**, and selects the newest backup at or
before that moment. Restore echoes the resolved backup in both zones before
extracting anything:

```text
using  8f2a10c  backup of 2026-11-01 12:00 -0300  (2026-11-01 15:00 UTC)
```

### Resolving a path

A path given to `restore` or `history --path` can name three different things, and
the rule is mechanical:

1. **Find the longest prefix that is a captured repository key**, by looking for
   `.tycho/repos/<prefix>/REPO.txt` in the store tree. If there is no such prefix,
   the path is a plain file: extract it from the store tree and stop.
2. **If a prefix matched, try the overlay first**:
   `.tycho/repos/<key>/overlay/<rest>`. That is where an uncommitted, untracked or
   gitignored file lives, and it is the version that was on disk at backup time.
3. **Otherwise the file is tracked and clean**, so it has no path in the store tree
   at all - its content is in the object database under `refs/tycho/<key>/*`. Find it
   with `git log` over that repository's captured refs and extract it with
   `git cat-file blob <commit>:<rest>`.

Step 3 is not an edge case. A clean tracked file inside a captured repository is the
**normal** case, and on this machine nearly every watched path is inside one. An
earlier version of this document had no such rule, which meant the single-file
restore in the walkthrough could not work at all.

Restore reports which of the three answered, since "recovered from the overlay" and
"recovered from repository history" mean different things about how current the
content is.

### What restore does and does not give back

**Restore never writes into your live tree, and never into an existing non-empty
destination without `--force`.** It puts files somewhere you name and you do the copy
yourself. A restore that overwrote in place would be one typo away from turning a
one-file problem into a directory-sized one.

Contents are byte-exact. **Metadata is not restored**: permissions beyond the
execute bit, ownership, timestamps, extended attributes, Finder tags and ACLs are all
lost, because git does not store them. A file that was `0600` comes back `0644`.
`store.md` section 7 has the full list, and it matters most for anything
secret-bearing - a restored private key is world-readable until you fix it.

The overlay is applied with a copy that **does not follow symlinks and refuses type
mismatches**, reporting each conflict. A plain recursive copy writes through a
symlink in the checkout to its target, fabricating a file that never existed on the
source machine.

A restore that reports a conflict exits 3. Everything else landed; one awkward
filename does not cost you the other nine hundred.

**The staging tree at `<dest>/.tycho/` is kept, always.** `git archive` extracts it
alongside your files - `.tycho/config.toml` and one `REPO.txt` and `overlay/` per
captured repository - and restore leaves it there. It costs the overlay's size twice
over, and it is the only reason a reported conflict can be settled by hand: the file
the copy declined to write is still sitting in `overlay/`, so you can look at both
versions and put the right one in place. Delete the staging tree and the conflict
becomes unrecoverable from the destination, forcing a second full restore just to see
what was refused. Restore deletes nothing.

### Cross-platform restore

Windows cannot represent reserved device names (`CON`, `AUX`, `NUL`, `COM1`-`COM9`,
`LPT1`-`LPT9`), trailing dots and spaces, the characters `<>:"|?*`, or paths beyond
260 characters without the extended-length prefix.

The store holds them all faithfully. On Windows, `restore` **reports a per-file skip
list and continues** rather than aborting or failing silently. `doctor` warns when a
watched tree contains such names, which is knowable years before anyone needs the
restore.

## 8. Dry run

```text
roots                                               files             size
--------------------------------------------------------------------------
  CoreEngineX   ~/Developer/CoreEngineX                31          32.6 KB
  Books         ~/Books                                126           340 MB

repositories                              head              state
--------------------------------------------------------------------------
  CoreEngineX/org                         main aef686f      2 modified
  CoreEngineX/org/handbook            main 1930b99      clean
  CoreEngineX/products/pager/pager-daemon detached ca905eb  clean
                                                            and 10 more

excluded                                          reason
--------------------------------------------------------------------------
  ~/Developer/CoreEngineX/scratch                 ignore rule
  node_modules                                    default junk
  target                                          default junk
  ~/Developer/CoreEngineX/scrach                  matched nothing

--------------------------------------------------------------------------
  to read      157 files                                            340 MB
               20 repositories                                      251 MB
```

Three tables, because the three questions are separate: how much is coming, what
repositories were found and in what state, and what the rules threw away.

**`matched nothing` is the row that earns this command.** A typo'd ignore path is
otherwise a silent no-op that commits gigabytes into permanent history, so every rule
a person wrote that matched nothing is listed. The built-in junk list is exempt: most
of its twenty patterns match nothing on any given tree, and listing them would bury
the one row that matters.

**The read total is split, because the two halves are read differently.** Loose files
are read from disk; a repository is read from its object database. On this machine
the first number is tiny and the second is nearly everything, since almost every
watched path is inside a repository - a total that counted only loose files would
understate the run by three orders of magnitude.

There is deliberately no "to write" estimate after a first run. Knowing how many new
objects a run produces means knowing which blobs the store already has, which means
hashing them - and D4 rejects the mtime cache that would approximate it. A fabricated
number is worse than an absent one.

The repository table lists ten and truncates with `and N more`, so the count and the
rows agree.

`--dry-run` is not free: it is a full stat walk plus three git invocations per
repository - head, state and `count-objects`. `--quick` omits the repository table,
which is the expensive half, when you only want the exclusion list.

## 9. `tycho config check`

```text
coreenginex       2 roots, 2 ignores, 1 reinclude, 3 remotes, weekly Sun 12:00
second-company    1 root, 0 ignores, 0 reincludes, 1 remote, daily 18:00

ok, no errors
```

The echo is the point: reading your own config back in summarised form is how a
remote attached to the wrong profile, or a schedule you thought you set, becomes
visible.

On failure every problem is reported at once rather than stopping at the first:

```text
coreenginex
  error   alias collision: ~/work/docs and ~/personal/docs both resolve to 'docs'
          give one an explicit name: { path = "~/work/docs", name = "work-docs" }
  error   unknown key 'wacth' in profile table
  error   profile has no remotes; set local_only = true to confirm this is intended
  warn    watched root does not exist: ~/Archive
  warn    ignore path matched nothing: ~/Developer/CoreEngineX/scrach
  warn    alias 'Books' was in the last backup and is not in the config

3 errors, 3 warnings
```
