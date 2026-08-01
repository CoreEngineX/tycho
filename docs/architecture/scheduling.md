# Scheduling and service lifecycle

Tycho has no resident process. The operating system's scheduler starts
`tycho run --all`, it does its work, and it exits.

## 1. Why the OS scheduler is enough

From `man launchd.plist`:

> Unlike cron which skips job invocations when the computer is asleep, launchd will
> start the job the next time the computer wakes up.

That is the catch-up behaviour a scheduler daemon would have been built to provide.
The machine being asleep at the scheduled time is the normal case for a laptop, and
launchd already handles it.

What a resident daemon would have added: an internal timer, an IPC protocol, an
async runtime, a second lifecycle to install and debug, and a new failure mode
where the daemon is dead and backups silently stop. What it would have bought:
nothing that launchd does not already do. Windows Task Scheduler is the weaker half
of this argument and is revisited when Windows support lands.

## 2. Schedule configuration

```toml
[profile.schedule]
weekly = { day = "sunday", at = "12:00" }
```

```rust
enum Schedule {
    Daily { at: TimeOfDay },
    Weekly { day: Weekday, at: TimeOfDay },
    Every(Duration),
}
```

Exactly one variant, enforced by the type rather than by validating that two
mutually exclusive keys are not both set. Each maps onto a launchd key directly:
`Daily` and `Weekly` become `StartCalendarInterval`, `Every` becomes
`StartInterval`. No cron parsing, and no dependency to do it. A cron variant can be
added if a schedule ever needs one, and until then the config cannot express a
schedule Tycho fails to honour.

## 3. Generated agent

`tycho service install` writes the plist and bootstraps it. The plist is generated,
never hand-written, because a hand-written plist that had drifted from what the
script actually needed is a large part of how the old system failed unnoticed.

Label: `com.coreenginex.tycho.<profile>`, at
`~/Library/LaunchAgents/com.coreenginex.tycho.<profile>.plist`.

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
<key>StandardErrorPath</key>
<string>/Users/…/Library/Logs/tycho/coreenginex.err.log</string>
<key>RunAtLoad</key>
<false/>
```

`RunAtLoad` is false deliberately. A backup on every login is not what a weekly
schedule means, and the wake-up catch-up already covers a missed run.

The absolute path to the binary is written at install time from the resolved
location, so the plist never depends on a `PATH` that launchd does not have.

### Full Disk Access

launchd runs the agent in a context macOS TCC restricts. A profile watching paths
outside the ordinary home directories needs `~/.cargo/bin/tycho` added to Full Disk
Access in System Settings, exactly as `a sibling daemon` in this same repository does.
`tycho doctor` checks for the symptom - readable by hand, unreadable under the agent
- and says so, rather than leaving a daemon that runs and quietly captures nothing.

## 4. Service commands

| Command | Does |
|---|---|
| `service install` | Generate the plist, `launchctl bootstrap gui/$UID`, report the next fire time |
| `service status` | Whether the agent is loaded, its last exit status, and the next scheduled run |
| `service restart` | `bootout` then `bootstrap`, for after a config change |
| `service uninstall` | `bootout` and remove the plist. Never touches the store or any backup |

`service status` surfacing the last exit status matters: `launchctl list` showing a
non-zero exit for the old job was the evidence that revealed a year of silent
failure, and nobody had reason to look. `tycho status` shows it without being asked.

## 5. Logs

`~/Library/Logs/tycho/<profile>.log`, rotated by size, with launchd's stderr going
to a sibling `.err.log` so a crash before Tycho's own logging starts still leaves a
trace. `tycho log -f` tails it.

## 6. Windows, later

Task Scheduler with a logon trigger and a calendar trigger, or a Windows service via
the `windows-service` crate. The decision needs the actual machine, since the
argument for the OS scheduler rests on wake-up catch-up behaviour that has to be
verified rather than assumed. Until then the `Schedule` type stays the same and only
its translation changes.
