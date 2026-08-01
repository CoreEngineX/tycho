# CLI

Colored, aligned, human-first. Not a TUI - no alternate screen, no key handling,
nothing that stops being useful when piped into a log file.

## 1. Commands

```text
tycho run [PROFILE] [--all] [--dry-run]
tycho status [PROFILE] [--check]
tycho history [PROFILE] [-n N]
tycho restore PROFILE [--at TIME] [--bundle] [PATH ...] DEST
tycho watch add|rm|list [PROFILE] [PATH]
tycho ignore add|rm|list [PROFILE] [PATTERN]
tycho config check|path|init
tycho service install|uninstall|status|restart [PROFILE]
tycho doctor
tycho log [PROFILE] [-f]
```

| Command | Does |
|---|---|
| `run` | Capture, commit, push. `--all` covers every profile and is what launchd invokes. `--dry-run` prints the plan and stops before touching the store |
| `status` | Per profile: next scheduled run, store size and backup count, one line per remote. `--check` exits non-zero on any yellow or red |
| `history` | The store's commits, rendered. Equivalent to reading `git log` in the store, which also works |
| `restore` | Recover to a destination directory. See section 4 |
| `watch` / `ignore` | Rule management with redundancy detection, editing the config in place |
| `config` | Validate, locate, or create the config file |
| `service` | launchd agent lifecycle. See `scheduling.md` |
| `doctor` | Environment, service, remotes, and object database health in one command |
| `log` | Tail the log file without needing to know where it lives |

## 2. Status output

```text
tycho 1.0.0

coreenginex           next run: Sunday 12:00, in 2d 4h
  store: 214 backups, oldest 2026-08-02, newest today 06:00, 1.2 GB
  -> gdrive      OK        pushed today 06:00     verified
  -> onedrive    OK        pushed today 06:00     verified
  -> t7          BEHIND 3  last seen 2026-07-29   optional, catches up on mount

second-company        next run: today 18:00
  -> onedrive    FAILED    push rejected today 06:00
                           run: tycho log second-company
```

Green for OK, yellow for BEHIND, red for FAILED, dim for de-emphasis. "verified"
appears only when a post-push head comparison actually passed, never as decoration.

Colour is off automatically when stdout is not a terminal, and `NO_COLOR` and
`--no-color` are both respected.

**`status` must work when everything else is broken.** It reads the state file and
the store directly. A corrupt config, a missing remote, an uninstalled service -
none of them may prevent `status` from rendering. That is precisely the moment
somebody runs it.

## 3. Exit codes

| Code | Meaning |
|---|---|
| 0 | Success. Includes a run where an optional remote is behind |
| 1 | Failure: a run failed, a required remote is unreachable, config has errors, or `status --check` found yellow or red |
| 2 | Usage error, which clap emits |

`status --check` is the shape a monitoring hook wants: no output parsing, just an
exit code.

## 4. Restore

```text
tycho restore coreenginex --at "2026-07-22 18:00" ~/recovered
tycho restore coreenginex --at "3 days ago" org/handbook ~/recovered
tycho restore coreenginex --bundle org/handbook ~/recovered
```

`--at` accepts an absolute timestamp or a relative expression and selects the
newest backup at or before that moment. Without it, the latest backup is used.

Positional paths before the destination limit what is restored, using store-relative
paths as shown by `history` and `status`. Without them the whole profile comes back.

`--bundle` writes a git bundle per captured repository instead of a checkout, for
handing history to another machine.

Restore never writes into an existing non-empty destination without `--force`.
Recovering a backup on top of live data is how a recovery turns into a second
incident.

## 5. Dry run output

```text
tycho run coreenginex --dry-run

roots
  CoreEngineX   ~/Developer/CoreEngineX     8,412 files   1.19 GB
  Books         ~/Books                        126 files   340 MB

repositories  12
  CoreEngineX/org                     main @ aef686f   1 untracked
  CoreEngineX/org/handbook        main @ 1930b99   clean
  CoreEngineX/products/a sibling project     dev  @ 41c8ee2   3 modified, 2 ignored

excluded by rules
  ~/Developer/CoreEngineX/scratch                     ignore
  **/node_modules                                     default junk
  **/target                                           default junk

would write   1.53 GB across 8,538 files
```

This is the command to run before the first real backup of a profile. The store
keeps history forever, so an ignore rule that should have been there and was not is
not fixable after the fact - only forward.
