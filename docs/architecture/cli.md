# CLI

Colored, aligned, human-first. Not a TUI - no alternate screen, no key handling,
nothing that stops being useful when piped into a log file.

## 1. Commands

```text
tycho run [PROFILE] [--all] [--dry-run]
tycho push [PROFILE] [--all]
tycho status [PROFILE] [--check]
tycho history [PROFILE] [-n N] [--path PATH]
tycho restore PROFILE [--at TIME] [--bundle] [PATH ...] DEST
tycho watch add|rm|list [PROFILE] [PATH]
tycho ignore add|rm|list [PROFILE] [PATTERN]
tycho config check|path|init
tycho service install|uninstall|status|restart [PROFILE]
tycho doctor
tycho log [PROFILE] [-f]
```

| Command | Does |
| --- | --- |
| `run` | Capture, commit, push. `--all` covers every profile and is what launchd invokes. `--dry-run` prints the plan and stops before touching the store |
| `push` | Push what the store already holds to any remote that is behind. Never captures. Exits immediately with nothing pending, so it is cheap enough to trigger on every mount and hourly |
| `status` | Per profile: next scheduled run, store size and backup count, one line per remote. `--check` exits non-zero on any yellow or red |
| `history` | The store's commits, rendered. `--path` limits it to backups that touched one file, which is how you find the backup to restore from |
| `restore` | Recover to a destination directory. See section 7 |
| `watch` / `ignore` | Rule management with redundancy detection, editing the config in place |
| `config` | Validate, locate, or create the config file |
| `service` | launchd agent lifecycle for both the backup agent and the catch-up agent. See `scheduling.md` |
| `doctor` | Environment, service, remotes, and object database health in one command |
| `log` | Tail the log file without needing to know where it lives |

`run` and `push` are the only commands that write anything outside the config file,
and `push` writes only to remotes.

## 2. Output rules

Every command that prints more than one row uses the same column model, so the
whole tool reads as one thing rather than a pile of ad-hoc formatters.

- **A fixed column spec per table.** Widths are declared once and shared by the
  header, the separator rule and every row - a header label sits directly over its
  own column, never approximately over it.
- **Text left, numbers right.** Sizes and counts right-align so digits line up on
  their ones place and are comparable by eye down the column.
- **Rows indent two spaces, section headers do not.** The indent is what makes a
  section scannable without any box drawing.
- **A rule spans exactly the table's width**, so the table's right edge is visible
  and consistent across sections.
- **Total width 76 columns**, which fits an 80-column terminal with room for a
  wrapped prompt.
- **State words are lowercase and short**: `ok`, `behind 3`, `failed`, `warn`,
  `fail`. Shouting is what colour is for.
- **Meaning never depends on colour.** Green, yellow and red add emphasis to a word
  that already says the same thing, because the output is read in log files and
  through pipes as often as in a terminal.

Colour switches off automatically when stdout is not a terminal, and both
`NO_COLOR` and `--no-color` are honoured.

## 3. `tycho status`

```text
tycho 1.0.0

coreenginex                                    next run  Sun 12:00, in 2d 4h
  214 backups since 2026-08-02, newest today 06:00, store 1.2 GB

  gdrive     ok          pushed today 06:00                         verified
  onedrive   ok          pushed today 06:00                         verified
  t7         behind 3    last seen 2026-07-29        optional, on next mount

second-company                              next run  today 18:00, in 5h 12m
  12 backups since 2026-07-20, newest today 06:00, store 84 MB

  onedrive   failed      push rejected today 06:00
                         tycho log second-company
```

The profile name and its next run sit on one line, pushed to opposite edges, so a
glance down the left edge lists your profiles and a glance down the right edge
tells you when each next fires. The store summary is a subtitle underneath rather
than a row, because it describes the profile rather than being one destination
among several.

Remotes are the aligned table: name, state, what happened and when, then a
right-aligned annotation. `verified` appears only when a post-push head comparison
actually passed - never as decoration - which is what distinguishes it from `ok`,
meaning the push command returned success.

A failed remote's follow-up command sits under the detail column rather than in
the annotation, because it is a thing to type, not a thing to read.

**`status` must render when everything else is broken.** It reads the state file
and the store directly, so a corrupt config, a missing remote or an uninstalled
service cannot stop it printing. That is precisely the moment somebody runs it.

## 4. `tycho history`

```text
when                commit    summary                              written
--------------------------------------------------------------------------
  today 06:00       8f2a10c   4 changed, 1 added, 2 repos moved      38 MB
  yesterday 06:00   1c93bb7   no changes                               0 B
  2026-07-31 06:00  aa7d4e1   112 changed, 9 added, 1 deleted       204 MB
  2026-07-30 06:00  4e10f92   2 changed                             1.1 MB
--------------------------------------------------------------------------
                              214 backups since 2026-08-02          1.2 GB
```

`written` is new objects for that run, not the size of everything backed up, which
is what makes the `no changes` row read as `0 B` and immediately obvious.

Recent timestamps render as `today` and `yesterday` and older ones as dates, since
the recent ones are what you scan for and the old ones are what you cite.

## 5. `tycho doctor`

```text
environment
--------------------------------------------------------------------------
  git                     ok        2.55.0
  config                  ok        2 profiles, no errors
  full disk access        warn      unverified, run doctor --deep

coreenginex
--------------------------------------------------------------------------
  service                 ok        loaded, last exit 0, next Sun 12:00
  store                   ok        fsck clean, 1.2 GB, 214 backups
  gdrive                  ok        reachable, head matches, fsck clean
  onedrive                ok        reachable, head matches, fsck clean
  t7                      warn      not mounted, behind 3, optional

second-company
--------------------------------------------------------------------------
  service                 ok        loaded, last exit 1, next today 18:00
  onedrive                fail      push rejected, remote ahead by 2
  sync artifacts          fail      found 'main (1).pack' beside the repo

2 failures, 2 warnings
```

One check per row, one verdict word per check, and the evidence beside it. The
`service` row carrying `last exit 1` is deliberate: a non-zero last exit shown by
`launchctl list` was the evidence that revealed a year of silent failure in the old
system, and nobody had a reason to go looking for it. Here it is on screen without
being asked for.

## 6. Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success. Includes a run where an optional remote is behind |
| 1 | Failure: a run failed, a required remote is unreachable, config has errors, or `status --check` found yellow or red |
| 2 | Usage error, which clap emits |

`status --check` is the shape a monitoring hook wants: no output to parse, just an
exit code.

## 7. Restore

```text
tycho restore coreenginex --at "2026-07-22 18:00" ~/recovered
tycho restore coreenginex --at "3 days ago" CoreEngineX/org/handbook ~/recovered
tycho restore coreenginex --bundle CoreEngineX/org/handbook ~/recovered
tycho restore coreenginex --store <path to a remote> ~/recovered
```

`--at` accepts an absolute timestamp or a relative expression and selects the
newest backup at or before that moment. Without it, the latest backup is used.

Positional paths before the destination limit what is restored, using store-relative
paths as shown by `history` and `status`. Without them the whole profile comes back.

`--bundle` writes a git bundle per captured repository instead of a checkout, for
handing history to another machine.

`--store` reads a remote directly instead of the local store, which is the disaster
case: on a replacement machine there is no local store, only a folder in a cloud
account. It accepts the same glob a remote path does.

**Restore never writes into your live tree, and never into an existing non-empty
destination without `--force`.** It puts files somewhere you name and you do the
copy yourself. A restore that overwrote in place would be one typo away from turning
a one-file problem into a directory-sized one, which is how a recovery becomes the
second incident.

`docs/walkthrough.md` shows the single-file recovery and the whole-machine recovery
as complete sessions.

## 8. Dry run

```text
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

Three tables, because the three questions are separate: how much is coming, what
repositories were found and what state they are in, and what the rules threw away.
The totals row reuses the `roots` column spec so its numbers land in the same
columns as the per-root numbers above them.

The `excluded` table is the one that earns this command. The store keeps history
forever, so an ignore rule that should have been there and was not cannot be fixed
after the fact, only going forward. This is where you find that out.

## 9. `tycho config check`

```text
coreenginex       2 roots, 2 ignores, 3 remotes, weekly Sun 12:00
second-company    1 root, 0 ignores, 1 remote, daily 18:00

ok, no errors
```

The echo is the point. Reading your own config back in summarised form is how a
remote attached to the wrong profile, or a schedule you thought you set, becomes
visible. On failure every problem is reported at once rather than stopping at the
first:

```text
coreenginex
  error   alias collision: ~/work/docs and ~/personal/docs both resolve to 'docs'
          give one an explicit name: { path = "~/work/docs", name = "work-docs" }
  error   unknown key 'wacth' in profile table
  warn    watched root does not exist: ~/Archive
  warn    redundant watch: ~/Developer/CoreEngineX/org is already covered
          by ~/Developer/CoreEngineX

2 errors, 2 warnings
```
