# Configuration and the rule tree

The config file is the single source of truth for what gets backed up. It is
hand-editable, and the commands that modify it preserve comments and formatting.

| Platform | Path |
|---|---|
| macOS | `~/.config/tycho/tycho.toml` |
| Windows | `%APPDATA%\tycho\tycho.toml` |

The macOS path is built from the home directory plus `.config/tycho`, deliberately
and not from the `directories` crate's `config_dir()`, which returns
`~/Library/Application Support` on macOS. A file a human is expected to open and
edit belongs in `~/.config`. The state, store and log paths do use `directories`,
where Apple's conventions are the right ones.

`tycho config path` prints it. `tycho config init` writes a starter file, and
**refuses if one already exists**, naming the path; `--force` overwrites after
writing a `.bak` beside it.

## 1. Why TOML and not JSON

JSON groups nested data more visibly - braces make containment unambiguous - and
that is a real argument, because a profile is exactly the kind of thing you want to
see grouped. Two facts outweigh it:

**JSON has no comments.** This file's most valuable lines are explanations: why a
path is excluded, why a remote is optional, what a schedule is for.

**`toml_edit` preserves comments and formatting on write.** `tycho watch add` edits
this file in place. With `serde_json` the file would be reserialised and every
comment, blank line and hand-chosen ordering lost the first time you used a command
instead of an editor.

YAML was rejected for whitespace sensitivity and its type-coercion surprises.

The valid part of the JSON argument is addressed by section 2.

## 2. Schema

Every table uses serde `deny_unknown_fields`. An unrecognised key is a hard error.
A silently ignored `wacth =` typo is the config-file form of the bug that made this
project necessary.

```toml
version = 1

[settings]
log_level = "info"                  # error | warn | info | debug

[[profile]]
name = "coreenginex"

watch = [
  "~/Developer/CoreEngineX",
  "~/Books",
]

ignore = [
  "~/Developer/CoreEngineX/scratch",
  "**/*.xcarchive",
]

reinclude = [
  "~/Developer/CoreEngineX/scratch/keep",
]

remotes = [
  { name = "gdrive",   path = "~/Library/CloudStorage/GoogleDrive-*/My Drive/CoreEngineX-Backups" },
  { name = "onedrive", path = "~/Library/CloudStorage/OneDrive-Personal/CoreEngineX-Backups" },
  { name = "t7",       path = "/Volumes/T7/tycho", optional = true, behind_tolerance = 4 },
]

schedule = { weekly = { day = "sunday", at = "12:00" } }

# use_default_ignores = true
# store_path = "/Volumes/T7/tycho-stores"
# local_only = false
```

**`version` is required on write and optional on read.** A config carrying a version
higher than the binary understands is rejected with a message naming both versions,
so a downgraded binary says "this config was written by a newer tycho" instead of
"unknown key". Without it, `deny_unknown_fields` turns any forward-compatible
addition into a total backup outage whose error message points at the wrong thing.

**Remotes are an inline array of tables rather than `[[profile.remote]]` sections.**
With section syntax a profile's remotes appear after it as separate blocks, and
which profile they attach to depends on position in the file. Inline tables keep
everything about a profile in one contiguous block you can move as a unit, which is
the property JSON's braces give you.

`tycho config check` echoes the parsed structure back for the same reason:

```text
coreenginex    2 roots, 2 ignores, 1 reinclude, 3 remotes, weekly Sun 12:00
```

### Field reference

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `version` | int | required on write | Config schema version |
| `settings.log_level` | enum | `info` | Verbosity of the log file |
| `profile.name` | string | required | Matches `[a-z0-9][a-z0-9-]*`. See section 3 |
| `profile.watch` | array | required, non-empty | Roots to back up. See section 4 |
| `profile.ignore` | array | `[]` | Paths or globs excluded. See section 5 |
| `profile.reinclude` | array | `[]` | Paths re-included beneath an ignore. See section 5 |
| `profile.remotes` | array | `[]` | Destinations. See `remotes.md` |
| `profile.schedule` | table | none | Exactly one of `daily`, `weekly`, `every`. See `scheduling.md` |
| `profile.schedule.every` | string | - | A whole number and one of `s`, `m`, `h`, `d` - `"6h"`. Becomes launchd's `StartInterval` in seconds |
| `profile.use_default_ignores` | bool | `true` | Apply the built-in junk list |
| `profile.store_path` | path | state dir | **Directory** into which `<profile>.git` is created. See section 6 |
| `profile.local_only` | bool | `false` | Acknowledges a profile with no remotes. See section 3 |
| `profile.remote.name` | string | required | Unique within the profile. Same charset as `profile.name` |
| `profile.remote.path` | path | required | Folder holding the bare repo |
| `profile.remote.optional` | bool | `false` | An unreachable optional remote is behind, not failed |
| `profile.remote.behind_tolerance` | int | `4` | Runs a remote may lag before it becomes a failure |

There is deliberately **no `notify_on_failure` key**. Failure is loud, always. A
setting whose only function is to silence the loudest signal would contradict the
reason this project exists. If notifications ever need suppressing on a headless
machine, that argues for a `--quiet` flag on one invocation, not a persistent
setting a future you forgets is set.

### Path expansion

`~` expands to the home directory. **Environment variables expand only from an
allow-list of `HOME` and `USER`, and an unset or empty variable is a hard config
error, never an empty string.**

That restriction is not fussiness. A launchd agent runs with a nearly empty
environment, so a root written as `$WORKSPACE/code` resolves interactively and
expands to nothing under the agent - the backup silently covers a different, smaller
tree than the one you tested. That is the original war story with a new cause.
`doctor` re-resolves every root under the agent's environment and compares it
against the interactive resolution.

After expansion a path must be absolute; a relative path is a config error. The
`AbsPath` constructor enforces this, so no code downstream can hold an unexpanded or
relative path.

**A `..` component is a config error rather than something Tycho resolves.**
Resolving it lexically is wrong whenever a component is a symlink, and leaving it in
place breaks the containment checks in section 6, which compare path prefixes -
`~/A/../B` does not compare as being outside `~/A`. Write the path out in full.

## 3. Profiles: what they are and whether you need two

A profile is **one set of watched paths, sharing one store, one schedule, one set of
destinations, and one restore boundary**. Profiles are independent - nothing is
shared, not the store, not the remotes, not the lock.

`profile.name` is constrained to `[a-z0-9][a-z0-9-]*` and 64 characters because it
is not just a label: it becomes a directory name in every remote, part of a launchd
label, and part of the store filename. No dots (they make a launchd label
ambiguous), no path separators, no `catchup` (reserved - see `scheduling.md`). The
length cap keeps a too-long name a config error rather than a cryptic filesystem
error at store creation.

You need exactly one today. The second person's machine does not need a second
profile in *your* config; they have their own machine with their own config file
containing their own single profile. Multi-machine is not multi-profile.

A second profile is worth adding only when one of these is true:

| Reason | Example |
|---|---|
| **The data belongs somewhere else** | Company work goes to the CoreEngineX Google Drive. Personal or school material must not, and no ignore rule can express "back this up but to a different account" |
| **The cadence genuinely differs** | Source trees are worth capturing daily. A 40 GB photo archive is not |
| **Restore should not drag everything along** | Recovering a work directory should not require reasoning about personal files in the same commit |
| **The store belongs on a different disk** | A large archive's store on the T7, everything else on the internal disk |

If none apply, a second profile is two stores, two schedules, two launchd agents and
two things to check, for no benefit. Add roots to the profile you have.

**Two profiles can share a destination.** Pointing both at `/Volumes/T7/tycho` gives
that folder `coreenginex.git` and `personal.git` side by side, fully independent.
The only shared resource is disk space, which `doctor` reports per volume. So
"different destinations" as a reason for a second profile means a different *account
or drive*, not a different folder.

**A profile with no remotes is a hard error unless `local_only = true`.** An empty
`remotes` array means the backup never leaves the machine, which contradicts the
requirement this project was built on - local-only history is treated as already
dead. Making it explicit means the state is chosen rather than defaulted into, and
`run` prints "no remotes configured, this backup exists only on this machine" rather
than an unqualified success.

**A profile with no schedule** is a `config check` warning, `status` renders its
next run as `never, manual only`, and `service install` on it is a hard error naming
the missing key.

## 4. Watched roots and aliases

A **watched root** is a top-level directory you handed to Tycho. Everything beneath
it is captured unless a rule excludes it.

An **alias** is the short name that root gets inside the store. Files are stored
under `<alias>/<path relative to the root>`:

| On your disk | In the store |
|---|---|
| `~/Developer/CoreEngineX/org/notes.md` | `CoreEngineX/org/notes.md` |
| `~/Books/receipts/2026-07.pdf` | `Books/receipts/2026-07.pdf` |

`store.md` section 5 is the normative source for the full store tree layout,
including where captured-repository metadata lives. This table shows plain files
only.

The alias defaults to the root's last path component, so you normally never think
about it.

**Why aliases exist.** The alternative is storing the absolute path, which embeds
your username in every path in every cloud folder, makes commit messages unreadable,
and means restoring onto a machine with a different home directory needs path
surgery. The alias is the root's identity independent of where it sits, so moving
`~/Books` to `~/Documents/Books` and updating the config keeps the same history for
the same content.

**Renaming the directory is the trap, and it is the opposite case from moving it.**
Since the alias defaults to the last component, renaming `~/Books` to `~/Library-of`
changes the alias, and the next backup records a total delete of `Books/` plus a
total re-add under `Library-of/`. Nothing is lost - the old content stays in history
- but the continuity the alias exists to provide is gone.

Tycho detects it rather than leaving you to discover it. `config check` and `run`
compare the config's aliases against those present in the store's `HEAD` tree; when
one alias disappears from the config while another appears, that is a rename
candidate and it warns:

```text
warn  alias 'Books' was in the last backup and is not in the config.
      This run will record a complete delete of Books/ and a complete re-add.
      To keep the history continuous, pin the alias:
        { path = "~/Library-of", name = "Books" }
```

**Naming one yourself.** Two roots can end in the same component:

```toml
watch = ["~/work/docs", "~/personal/docs"]     # both want the alias "docs"
```

`config check` refuses this rather than guessing, and you disambiguate:

```toml
watch = [
  { path = "~/work/docs",     name = "work-docs" },
  { path = "~/personal/docs", name = "personal-docs" },
]
```

**Alias charset.** The alias becomes both a tree path component and part of a git
refname, so it is constrained and validated: percent-encode any byte outside
`[A-Za-z0-9._-]`, reject a leading `.` and a `.lock` suffix, reject `/` in an
explicit alias outright, and validate the finished refname with
`git check-ref-format`. `.tycho` is reserved and is a config error, because the
store tree uses it for metadata.

This matters more than it looks: a root at `~/.config/nvim` defaults to the alias
`nvim`, but a *repository* discovered at a path containing a dot-component still has
to encode cleanly, and an unvalidated alias makes every repository capture beneath
it fail at fetch time rather than at config time.

In code, a watch entry is a sum type rather than a struct with an optional name:

```rust
enum WatchEntry {
    Bare(AbsPath),
    Named { path: AbsPath, name: RootAlias },
}
```

Auto-disambiguation was rejected: a generated `docs-2` would move if you reordered
the config, and stored paths must be stable across runs or history stops lining up.

**A watch entry may name a file, not only a directory.** That is the escape hatch
for re-including a single file that a glob ignore would otherwise exclude.

**A watched root nested inside another watched root is a hard error.** There is no
case for it that `reinclude` does not cover better, and allowing it raises the
question of which alias the inner content is stored under - a question with no good
answer.

## 5. Rule resolution

The previous version of this section specified two algorithms that disagreed. This
one specifies one.

### The three rule kinds

| Kind | Written as | Tier |
|---|---|---|
| Explicit path | an entry in `watch`, `ignore` or `reinclude` beginning with `~`, `$` or `/` | 3, strongest |
| Glob | any other entry in `ignore` | 2 |
| Default junk ignore | the built-in list below | 1, weakest |

`reinclude` exists as its own array because a re-inclusion is not a root: it has no
alias of its own and its content is stored under the **enclosing root's** alias. An
earlier schema overloaded `watch` for both jobs, which left the store path of a
re-included file undefined.

### The algorithm

For a candidate path, evaluate every rule against the path itself and against each
of its ancestors. A match has a **depth**: the number of path components it matched
at. Then:

1. **The deepest match wins.**
2. **Ties break by tier**: explicit path beats glob, glob beats junk.
3. **Two explicit path rules at equal depth on the same path is a config error**,
   caught by `config check` - it is statically decidable and there is no sensible
   default.
4. **Two globs at equal depth**, or a glob and a junk rule at equal depth, need no
   error: every rule at tier 1 or 2 is an ignore, so they agree on the outcome.

A file is captured if and only if the winning rule is a watch or a reinclude.

The consequence worth stating plainly, because it is where the old text went wrong:
**a junk or glob ignore matching at depth N is defeated only by a *deeper* explicit
rule, never by an ancestor watch.** A watch at depth 3 does not rescue a file that
`**/node_modules` ignores at depth 7. To keep such a file you name it, at a depth
below the ignore.

### Truth table

These are the test vectors, and `capture.md`'s test matrix implements them.

| # | Rules in play | Candidate | Winner | Captured |
|---|---|---|---|---|
| 1 | watch `~/A` (d1) | `~/A/x.md` (d2) | watch, d1 | yes |
| 2 | watch `~/A` (d1), ignore `~/A/s` (d2) | `~/A/s/t.bin` | ignore, d2 | no |
| 3 | + reinclude `~/A/s/keep` (d3) | `~/A/s/keep/k.pem` | reinclude, d3 | yes |
| 4 | watch `~/A` (d1), junk `target` | `~/A/p/target/x.o` (junk at d3) | junk, d3 | no |
| 5 | + reinclude `~/A/p/target` (d3) | `~/A/p/target/x.o` | reinclude d3 beats junk d3 by tier | yes |
| 6 | watch `~/A`, glob `**/*.xcarchive` | `~/A/b/Foo.xcarchive` (glob at d3) | glob, d3 | no |
| 7 | + reinclude of the file itself (d3) | `~/A/b/Foo.xcarchive` | reinclude d3 beats glob d3 by tier | yes |
| 8 | watch `~/A`, ignore `~/A/s`, glob `*.log` | `~/A/s/keep/a.log` with reinclude `~/A/s/keep` (d3) | glob matches at d4, deeper than the reinclude | no |

Case 8 is the one people get wrong. A glob matching the filename matches at the
file's own depth, which is deeper than any directory rule above it. To keep
`a.log` you reinclude the file itself.

**The same rule explains a question this design will be asked.** Case 5's rule set
lists only the junk rule `target`; with the *whole* default list in play, `*.o`
matches `~/A/p/target/x.o` at depth 4 and beats the reinclude at depth 3, so the
object files stay excluded while everything else under `target/` comes back. That is
case 8 applied consistently rather than an exception to it. Re-including a directory
re-includes the directory, not the deeper patterns inside it.

### Default junk ignores

Applied unless `use_default_ignores = false`:

```text
node_modules  target  build  .build  dist  .next  .nuxt  .svelte-kit
DerivedData  .gradle  Pods  __pycache__  .venv  venv
*.o  *.pyc  *.class  .DS_Store  Thumbs.db  *.xcuserstate
```

Load-bearing rather than cosmetic: `~/.build_caches/cargo` on this machine is 38 GB,
and committing it once puts it in history permanently.

### Redundancy detection

`tycho watch add PATH` checks whether an ancestor watch already covers the path with
no intervening ignore. If so it reports "already covered by `<ancestor>`" and changes
nothing. If an ignore does intervene, the correct entry is a `reinclude`, and the
command says so.

## 6. Store location

`store_path` names a **directory**, into which `<profile>.git` is created - matching
both the default layout and how remotes work. It is not a path to the repository
itself.

Four rules, each a hard error in `config check`:

- **Two profiles may not resolve to the same `store_path`.** The lock is per profile
  and explicitly does not exclude another profile, so a shared store would be two
  writers with no mutual exclusion.
- **The store path, every remote path, the state directory, the log directory and
  the config file's own directory must not lie inside any watched root**, and no
  watched root inside them. Otherwise the backup captures itself, unboundedly. It is
  a handful of prefix comparisons.
- **The parent directory must already exist.** Tycho never creates it with
  `mkdir -p`. If the nearest existing ancestor is a mountpoint that is not currently
  mounted, the run fails with "store volume not mounted" rather than initialising a
  second, empty store on the boot disk that then backs up happily and reports
  success.
- **A run refuses to create a store from nothing when the state file records that
  this profile already had one.** That is the same failure caught from the other
  side, and it also covers a store deleted by accident.

`config check` warns when `store_path` is not on the boot volume, and `doctor` has a
store-reachability row.

## 7. The config file is itself backed up

Every run captures the resolved config into the store as `.tycho/config.toml`. The
definition of what a backup contains travels with the backup, and `history` shows
when a rule changed.

Without it, a recovery restores the data but not the description of what was meant
to be in it - and on a machine that has just been replaced, that description exists
nowhere else.

## 8. Validation

`tycho config check` reports every problem at once rather than stopping at the
first.

| Check | Result |
|---|---|
| Unknown key | error, names the key and the table |
| `version` newer than this binary | error, names both versions |
| Duplicate profile name | error |
| Invalid profile name charset, or the reserved name `catchup` | error |
| Duplicate remote name within a profile | error |
| Empty `watch` | error |
| Relative path after expansion | error |
| `..` component in a path | error, name the path in full |
| Unset or empty environment variable in a path | error |
| Environment variable outside the allow-list | error |
| Invalid remote name charset | error |
| Alias collision | error, suggests the `{ path, name }` form |
| Invalid alias after encoding, or the reserved alias `.tycho` | error |
| Two explicit path rules at equal depth on one path | error |
| Watched root nested inside another watched root | error |
| Duplicate `store_path` across profiles | error |
| Store, remote, state, log or config path inside a watched root | error |
| Store parent missing, or on an unmounted volume | error |
| Profile with no remotes and no `local_only = true` | error |
| More than one of `daily`, `weekly`, `every` | error |
| Invalid glob | error |
| Watched root does not exist | warning, the run skips it and records it |
| Ignore path does not exist | warning |
| Ignore path not under any watched root, so it can never fire | warning |
| Redundant watch | warning |
| Alias present in the last backup and absent from the config | warning, rename candidate |
| Profile with no schedule | warning |
| `store_path` not on the boot volume | warning |
| Remote glob matches nothing | warning, resolved at run time |
| Config file not inside any watched root | warning |

Exit code 0 with warnings, 1 with any error.

`--dry-run` additionally reports **every rule that matched nothing**, which is how a
typo'd ignore path surfaces before it silently commits gigabytes.

## 9. In-place editing

`tycho watch add|rm`, `tycho ignore add|rm` and the `reinclude` equivalents edit the
file through `toml_edit`, preserving comments, ordering and whitespace.

`rm` removes the entry, its trailing comment, and any comment lines immediately
preceding it that are not separated from it by a blank line. Everything else stays
put.

The file remains something you can own and hand-edit; the commands are a
convenience, not the interface.

## 10. Cross-platform naming

Tycho captures every name faithfully - the store is a git repository and has no such
limits. The constraint appears at **restore** time on Windows, so `restore` reports
what it could not write rather than aborting, and `doctor` warns when a watched tree
contains names that cannot be restored there. That is knowable years before anyone
needs the restore, which is the only time the warning is useful.

**What follows was measured on Windows 11 build 22635, not read from
documentation.** The first version of this section was read from documentation and
was wrong in the direction that matters: it said Windows "cannot represent" these
names, implying a refusal. Most of them are not refused. They are silently accepted
as **a different name**, which is worse, because a refusal is a report and a rename
is not.

| Class | What actually happens |
| --- | --- |
| `<>"\|?*` | Refused, `errno 22`. A real error the caller sees. |
| Control characters `0x01`-`0x1F` | Refused, `InvalidFilename`. A newline or tab in a name is not storable, which the old text did not mention at all. |
| `\` and `/` | Refused - they are separators. See `store.md` section 2 for why this one is load-bearing. |
| Trailing dot or space | **Silently stripped.** `trailing.` and `trailing ` both become `trailing`, so two distinct source names collide on one destination. |
| `:` | **Silently redirects into an alternate data stream.** `colon:.md` creates a 0-byte file `colon` carrying a stream `colon:.md:$DATA` holding the content. `dir /r` is what shows it. |
| `CON`, `PRN`, `AUX`, `COM1`-`COM9`, `LPT1`-`LPT9`, and their `.ext` forms | **Created as ordinary files.** The reservation is not enforced by `CreateFileW` on this build. It *is* still enforced by `cmd` redirection, so `echo x > COM1` reaches the device while a program writing the same path gets a file. |
| Bare `NUL` | The one true device case: the write succeeds, goes to the null device, and no file appears. Silent, complete data loss. |
| Beyond 260 characters | Depends on two independent settings, below. |

The 260-character limit is **not** a filesystem property. It is off on this machine:
`HKLM\SYSTEM\CurrentControlSet\Control\FileSystem\LongPathsEnabled` is `1`, and a
506-character path was created without complaint.

Git has a **separate** switch, `core.longpaths`, and it is unset by default. With it
off, `git add` on a directory past the limit prints `warning: could not open
directory ...: Filename too long`, **exits 0**, and stages nothing:

```text
$ git -c core.longpaths=false add -A
warning: could not open directory 'aaaa.../': Filename too long
$ git ls-files | wc -l
0
$ git -c core.longpaths=true add -A && git ls-files | wc -l
1
```

That is a captured repository silently losing files at exit 0, which is the failure
class this project exists to prevent, so **`doctor` checks `core.longpaths` on
Windows and says so when it is unset**. The OS setting is worth reporting too, but it
is the git one that decides what a captured repository contains.
