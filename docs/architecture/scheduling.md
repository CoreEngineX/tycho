# Scheduling and service lifecycle

Tycho has no resident process. The operating system's scheduler starts a run, it
does its work, and it exits.

## 1. Why the OS scheduler is enough, and where the argument stops

From `man launchd.plist`, on `StartCalendarInterval`:

> Unlike cron which skips job invocations when the computer is asleep, launchd will
> start the job the next time the computer wakes up.

That is the catch-up behaviour a scheduler daemon would have been built to provide,
and a laptop being asleep at noon is the normal case.

**The promise is scoped to sleep, not to power-off.** The man page says nothing
about a machine that was shut down across its scheduled time, and a grep of the page
for boot, power, shutdown or reboot returns nothing about a missed calendar
interval. So the honest position is: launchd covers the common case, and Tycho must
not assume it covers all of them.

**Therefore every invocation performs an overdue check that does not depend on
launchd firing at all.** On every run of any agent - including the hourly catch-up -
Tycho compares now against the last successful run recorded in the state file. If
the gap exceeds the profile's schedule interval plus a grace period, it says so:
`status` shows the profile red, and a notification fires. If the invocation was the
catch-up agent and a backup is overdue, it runs one.

That check is what makes the no-daemon design safe rather than merely cheap. Without
it, a Mac shut down over a weekend silently skips its weekly backup and nothing ever
notices - which is the exact shape of the failure this project exists to correct.

What a resident daemon would have added: an internal timer, an IPC protocol, an
async runtime, a second lifecycle to install and debug, and a new failure mode where
the daemon is dead and backups stop silently. Windows Task Scheduler is the weaker
half of this argument, and section 10 is where it is revisited.

## 2. Schedule configuration

```toml
schedule = { weekly = { day = "sunday", at = "12:00" } }
```

```rust
enum Schedule {
    Daily { at: TimeOfDay },
    Weekly { day: Weekday, at: TimeOfDay },
    Every(Duration),
}
```

Exactly one variant, enforced by the type rather than by validating that two
mutually exclusive keys are not both set. `Daily` and `Weekly` become
`StartCalendarInterval`; `Every` becomes `StartInterval`. No cron parsing and no
dependency to do it. A cron variant can be added if a schedule ever needs one, and
until then the config cannot express a schedule Tycho fails to honour.

## 3. Labels

| Agent | Label |
|---|---|
| Backup, one per profile | `com.coreenginex.tycho.profile.<profile>` |
| Catch-up, one shared | `com.coreenginex.tycho.catchup` |
| Access probe, transient | `com.coreenginex.tycho.probe` |

The label namespaces the software's **publisher**, not the person running it -
`com.apple.*` and `com.google.keystone` run on machines belonging to people who work
at neither. Someone else running Tycho is still running CoreEngineX's Tycho, so the
branded label is correct precisely because strangers will use it. A neutral
namespace would be worse: it claims a name nobody owns, and two unrelated tools
could collide in it.

**The `profile.` infix is load-bearing.** Deriving the backup label directly as
`com.coreenginex.tycho.<profile>` from a user-controlled name means a profile named
`catchup` produces exactly the catch-up agent's label, and `Label` is documented as
uniquely identifying the job - so one silently displaces the other. Either that
profile never gets a scheduled backup, or nothing ever catches up on mount. Putting
the two families in disjoint namespaces removes the collision structurally, and
`config.md` additionally constrains profile names to `[a-z0-9][a-z0-9-]*` with
`catchup` reserved, which also keeps dots and path separators out of a label and a
filename.

Labels live in the per-user GUI domain, so two macOS accounts on one Mac each get
their own agents with no interaction.

## 4. The generated backup agent

`tycho service install` writes the plist and bootstraps it. The plist is generated,
never hand-written, because a hand-written plist that had drifted from what the
script needed is a large part of how the old system failed unnoticed.

**When `install` was given `--config`, that path is written into `ProgramArguments`
as an absolute path.** Found by installing a scratch agent and letting launchd fire
it: without this the agent reads the default config location instead, so on any
machine whose config lives elsewhere it fails on every single firing with a
file-not-found - loudly in its own `.err.log`, and completely invisibly to anyone not
reading that file. launchd also runs the agent from `/`, so a relative path would
resolve against a directory nobody chose.

At `~/Library/LaunchAgents/com.coreenginex.tycho.profile.<profile>.plist`:

```xml
<key>ProgramArguments</key>
<array>
  <string>/Users/…/.cargo/bin/tycho</string>
  <string>run</string>
  <string>coreenginex</string>
</array>
<key>StartCalendarInterval</key>
<dict>
  <key>Weekday</key><integer>0</integer>
  <key>Hour</key><integer>12</integer>
  <key>Minute</key><integer>0</integer>
</dict>
<key>StandardOutPath</key>
<string>/Users/…/Library/Logs/tycho/coreenginex.out.log</string>
<key>StandardErrorPath</key>
<string>/Users/…/Library/Logs/tycho/coreenginex.err.log</string>
<key>RunAtLoad</key>
<false/>
```

`Weekday 0` is Sunday, confirmed against the man page.

`RunAtLoad` is false deliberately. A backup on every login is not what a weekly
schedule means, and the wake-up catch-up plus the overdue check in section 1 cover a
missed run.

**Both output paths are set.** launchd sends stdout to `/dev/null` when
`StandardOutPath` is absent, which is a silent-failure surface in a tool whose whole
thesis is that failure is loud.

**`service install` creates `~/Library/Logs/tycho/` before writing the plist.**
launchd silently drops output when the directory does not exist, so the diagnostic
trail the design leans on would never have been written. `doctor` asserts the
directory exists and is writable.

The absolute path to the binary is resolved and written at install time, so the
plist never depends on a `PATH` launchd does not have.

## 5. The catch-up agent

One shared agent whose only job is `tycho push --all`. It never captures.

```xml
<key>ProgramArguments</key>
<array>
  <string>/Users/…/.cargo/bin/tycho</string>
  <string>push</string>
  <string>--all</string>
</array>
<key>StartOnMount</key>
<true/>
<key>StartInterval</key>
<integer>3600</integer>
```

`StartOnMount` fires on **every** filesystem mount - plugging in the external drive,
but also mounting a DMG, a network share, or an APFS local snapshot, which happens
often. `ThrottleInterval` bounds the resulting churn at one launch per 10 seconds by
default. The job exits before doing any filesystem work when no remote is in
`Behind`, so the common case costs a process spawn and a state-file read.

`StartInterval` firings are **missed while the machine is asleep and are not
coalesced on wake**, unlike `StartCalendarInterval`. So the hourly leg covers a
signed-out account or a sync client that was not running, but it is not a
post-wake guarantee - the overdue check in section 1 is what provides that.

**`RunAtLoad` is deliberately absent here**, though an earlier draft had it. At
login, cloud File Providers may not have mounted, so remote paths do not exist yet
and every remote would be probed as unreachable on every boot. `StartOnMount`
already fires when those domains mount, which is strictly better timing. `remotes.md`
section 4 additionally suppresses state transitions from probes within 60 seconds of
load, as belt and braces.

## 6. Service commands

| Command | Does |
|---|---|
| `service install` | Create the log directory, generate both plists, `launchctl bootstrap gui/$UID`, report the next fire time. Hard error on a profile with no schedule, naming the missing key |
| `service status` | Whether each agent is loaded, its last exit status, and the next scheduled run |
| `service restart` | `bootout` then `bootstrap`, for after a config change |
| `service uninstall` | `bootout` and remove the plists. Never touches the store or any backup |

`service status` surfacing the last exit status matters: `launchctl list` showing a
non-zero exit for the old job was the evidence that revealed a year of silent
failure, and nobody had reason to look. `tycho status` shows it without being asked.

**`launchctl list` reports `LastExitStatus` as a raw `wait(2)` status, not an exit
code.** An ordinary failure comes back as `256`, and a job killed by a signal comes
back as the signal number - so printing the field verbatim showed `exit 256`, a
number no shell ever produced and nobody could look up. It is decoded the way the
shell decodes it, and an exit is distinguished from a signal.

**`doctor` compares each installed agent's schedule against the config's.** Drift
between what the config says and what is actually installed is precisely what killed
the old system, and it is a cheap comparison.

## 7. Full Disk Access

launchd runs the agent in a context macOS TCC restricts, so a profile watching paths
outside the ordinary home directories needs `~/.cargo/bin/tycho` added to Full Disk
Access in System Settings - exactly as `a sibling daemon` in the sibling repository
does.

**An interactive `doctor` cannot measure the agent's grant.** `doctor` runs with the
terminal's TCC grant, which is a different grant from the agent's, so a check that
simply reads a watched root proves nothing about what the agent can do. An earlier
draft described exactly that impossible check.

The only mechanism that measures the real thing is to probe **through launchd**:

```text
tycho doctor --deep
  bootstraps  com.coreenginex.tycho.probe   -> tycho probe-access <root>...
  waits for it to exit, reads its result file, boots it out
```

That is the same lifecycle `service install` already implements, so the cost is
small. Plain `doctor` reports the access state as unverified and names
`--deep` rather than guessing.

This matters because a TCC denial is not a loud failure on its own - it is a read
error on a directory. What makes it loud is the sanity gate in `capture.md` section
3: a root that yields zero capturable entries fails the run outright, so a
TCC-denied root cannot produce a green empty backup.

## 8. Notifications

Desktop notifications are one of the four channels behind "failure is loud", and
they are the only one whose delivery depends on a user-level authorisation that can
silently be off.

`doctor` reports the notification authorisation state, and `doctor --deep` sends a
test notification. A machine in a Focus mode suppresses delivery, which is expected -
which is exactly why the non-zero exit code, the red `status` line and the state-file
record are the real contract, and the notification is a convenience on top.

## 9. Logs

`~/Library/Logs/tycho/<profile>.log`, rotated by size, with launchd's stdout and
stderr going to sibling `.out.log` and `.err.log` files so a crash before Tycho's own
logging starts still leaves a trace. `tycho log -f` tails it.

## 10. Windows

Task Scheduler, driven through `schtasks`, in `platform::schtasks`. `service` talks
to a `#[cfg]`-selected `platform::scheduler`; the `Schedule` type is unchanged and
only its translation differs, as this section predicted. No Windows service and no
`windows-service` crate: there is still no resident process.

The catch-up argument in section 1 rests on the scheduler running a job it missed,
and on Windows that is **`<StartWhenAvailable>true</StartWhenAvailable>`**. It
round-trips through `schtasks /Query /XML`, so it can be read back off a registered
task rather than believed. As on macOS, `Schedule::overdue_by` is what makes the
precise semantics not matter very much.

Four things the actual machine decided, none of which are in the documentation you
would reach for first:

- **A task does not run on battery unless told to.** `DisallowStartIfOnBatteries` and
  `StopIfGoingOnBatteries` default to `true`, and a definition naming neither *gets
  them anyway* - read back off a task registered without them. On a laptop that is a
  weekly backup that silently never happens, which is this project's own failure mode.
  Both are written `false`, and `Power Management` reads empty on an installed task.
- **`schtasks /Create /XML` refuses a UTF-8 declaration**, with `unable to switch the
  encoding` pointing at line 1 column 40. The definition declares UTF-16 and is
  written as UTF-16LE with a BOM, rather than declaring one encoding and carrying
  another. That is the reason `scheduler::encode`/`decode` exist as a seam at all.
- **A `<LogonTrigger>` naming no `<UserId>` means every user**, and registering one
  needs elevation: `ERROR: Access is denied` on an ordinary account. It is dropped,
  which lands in the same place as `catchup_plist` leaving `RunAtLoad` out - at logon
  the cloud File Providers may not have mounted, so every remote would probe as
  unreachable on every boot.
- **Task Scheduler captures no output.** There is no `StandardOutPath` equivalent, so
  the action is `cmd /c ""prog" args >> "log" 2>&1"` - the one quoting form that
  survives a space in either path. Without it a panic before Tycho opens its own log
  leaves nothing at all, and "failure is loud" would be a claim rather than a fact.

There is also **no `StartOnMount`**: Task Scheduler has no "a volume appeared"
trigger. The catch-up task is hourly instead, and the overdue check covers the gap.

`Ended` gained a `NeverRun` variant for this. Task Scheduler reports
`SCHED_S_TASK_HAS_NOT_RUN` (267011) for a task that has not fired yet, and a freshly
installed agent must read as neither success nor failure. launchd conflates the two -
an absent `LastExitStatus` decodes to exit 0 - so this is a distinction macOS could
have drawn and did not.

**Notifications port by the same pattern, and the pattern is the interesting part.**
A CLI has no notification identity of its own on either platform: macOS wants an
installed app bundle, Windows wants a registered `AppUserModelID`, and a binary in
`~/.cargo/bin` has neither. The answer on both is to shell out to the platform's
scripting host and borrow its identity - `osascript` here, `powershell` raising a
`Windows.UI.Notifications` toast under PowerShell's own AUMID there. So the macOS
banner is attributed to Script Editor and the Windows one will be attributed to
PowerShell, and `doctor` says so rather than hiding it.

A crate does not remove that. `notify-rust` hits the identical AUMID requirement on
Windows and binds the deprecated `NSUserNotification` on macOS; it moves the caveat
behind a dependency and an audit surface instead of solving it.

**The Windows arm is now written, and fired.** It was deliberately left unwritten
until there was a machine to run it on, which was the right call: getting it right
took two things no reading would have produced. The body crosses two parsers - XML,
then a PowerShell single-quoted literal - so it is escaped for both, and a control
character is replaced rather than embedded because XML 1.0 cannot carry one. Delivery
was confirmed out of band, by `LastNotificationAddedTime` under the PowerShell AUMID's
registry key moving to the second the test ran; a toast that Windows accepts but never
shows would otherwise look identical.

`platform::notify` remains a `#[cfg]`-selected free function, and the arm for any
third platform still returns a typed `Unsupported`, so a future port finds a failing
call rather than a lie. A free function rather than a trait object because the
implementation is chosen at compile time and never swapped at run time: a `dyn` would
buy indirection and nothing else.
