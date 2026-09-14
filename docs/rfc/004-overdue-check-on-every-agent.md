# RFC 004 -- Make the overdue check real

| Field             | Value                                            |
|-------------------|--------------------------------------------------|
| **Status**        | `Implemented`                                    |
| **Ticket**        | `(none)` -- see RFC 002 D4                       |
| **Branch**        | `rfc/overdue-catch-up`                           |
| **Contract docs** | `docs/architecture/scheduling.md`                |
| **Start date**    | `2026-09-14`                                     |
| **Updated**       | `2026-09-14`                                     |

---

## Summary

`scheduling.md` section 1 promises three things about a missed backup: every agent
invocation checks whether the profile is overdue, a notification fires when it is, and
the catch-up agent runs a backup. None of the three is implemented. `overdue()` is
called only by `status`, which is a human typing a command. A profile stopped running on
2026-09-06 and nothing said so for eight days. This RFC connects the check to the hourly
catch-up agent, with a guard the doc did not anticipate.

---

## Why this RFC, why now

The scheduled agent for `cex` last produced output on 2026-09-06 03:00. On 2026-09-14
launchd reported `runs = 13`, `last exit code = 78: EX_CONFIG`, and the job had not
spawned since. `tycho service cex restart` cleared it. Eight days, no capture, no
notification, no banner - the only way to find out was to type `tycho status`.

`scheduling.md` section 1 already says this must not happen, and states the mechanism:

> **Therefore every invocation performs an overdue check that does not depend on
> launchd firing at all.** On every run of any agent - including the hourly catch-up -
> Tycho compares now against the last successful run recorded in the state file. If the
> gap exceeds the profile's schedule interval plus a grace period, it says so: `status`
> shows the profile red, and a notification fires. If the invocation was the catch-up
> agent and a backup is overdue, it runs one.
>
> That check is what makes the no-daemon design safe rather than merely cheap.

`Schedule::overdue_by`'s own doc comment repeats it: *"Every invocation of every agent
runs this."* Measured against the code, all three clauses are false:

| Promised | Actual |
|---|---|
| Every agent invocation checks | `overdue()` has exactly two callers, `status` (line 499) and `snapshot` (line 524). Both are on the `status` path. `run` never calls it; `push` never calls it |
| A notification fires | `notify` has one caller, `cli::run::announce`, reached only at the end of a completed run. There is no overdue notification anywhere |
| The catch-up agent runs one | The catch-up agent runs `tycho push --all`, which calls `store::run::catch_up` - push only. It never captures |

**The hourly catch-up agent was running fine the entire eight days.** Its log updated
every hour through the outage. The one component that kept working is the one the
documentation nominates to notice, and it was not wired to.

**Why it can't wait:** this is the failure the project exists to correct, stated in its
own words in section 1, and the mechanism against it is documentation only. The incident
is not hypothetical; it already happened, on the machine that wrote the doc.

**What this RFC does not claim.** The cause of the launchd wedge is not established. The
job carried `properties = ... needs LWCR update | managed LWCR | has LWCR` alongside exit
78, which suggested a stale code requirement after `__bootstrap` replaced the binary. That
hypothesis was tested and **failed to reproduce**: replacing the binary leaves the
property unset, before a spawn and after one. So the cause stays open, and this RFC fixes
the silence rather than the wedge - which is the right layer anyway, since the next cause
will be a different one.

---

## Scope

### In

- `one_push` evaluates the overdue check, so the hourly catch-up agent performs it.
- A notification when a profile is overdue and nothing has attempted a run.
- The catch-up agent runs a backup in that case, as section 1 says it should.
- A guard section 1 did not anticipate: at most one catch-up attempt per schedule
  interval. See Reference-level.
- `scheduling.md` gains the guard; the two doc comments that assert the unimplemented
  behaviour are corrected to describe what the code does.

### Out (deferred)

- The cause of `EX_CONFIG`. Not reproducible, so there is nothing to fix yet.
- `doctor` decoding an agent's exit code into a remedy. Worth doing, unrelated to the
  silence: `doctor` already reported `agent fail exit 78` correctly and nobody was
  looking at it. Making an unread line more readable is not the fix.
- The `Outcome::Failed` interaction described under Edge cases, which is RFC-worthy on
  its own and predates this.

---

## Guide-level explanation

Nothing new to call. The hourly agent gains behaviour:

```text
$ tycho push --all          # what com.coreenginex.tycho.catchup runs
cex  overdue by 8d 12h, and nothing has attempted a run this interval
     starting one now
cex  f1d734f
  captured   162 added
```

and a desktop notification saying the same, once per interval rather than once per hour.

---

## Reference-level explanation

### Behaviour and state

Two different questions are being asked of the state file, and conflating them is what
makes the naive version dangerous:

- **"Is the backup stale?"** - answered from the last **successful** run. This is the
  existing `overdue()`, unchanged, and it is what turns `status` red.
- **"Should I start one right now?"** - answered from the last **attempted** run,
  whatever its outcome. New.

The second is load-bearing. `overdue()` skips `Outcome::Failed` records, so a profile
whose runs all fail stays overdue forever. Wiring "overdue means run one" to an hourly
agent without the second question means a full backup every hour, indefinitely, for any
profile with a failing remote. That is today's `cex` exactly: `ghost` is red, so every
run is `Failed`, so `overdue()` is permanently `Some`. The doc's sentence, implemented
literally, would have started an hourly capture loop on this machine.

So the catch-up agent acts only when **both** hold: the profile is overdue, **and**
nothing has attempted a run within one schedule interval. One attempt per interval,
whatever happens to it.

```rust
/// The most recent attempt, successful or not.
///
/// Distinct from the last success `overdue` reads: a profile whose runs all fail is
/// permanently overdue, so "overdue, therefore run one" on an hourly agent is an
/// hourly full backup forever unless attempts are counted too.
fn attempted_within(profile: &Profile, state: &State, window: Duration) -> bool;
```

When the guard is not satisfied, the failing runs are already notifying through
`announce`, so nothing is lost by staying quiet.

### Invariants

- The catch-up agent starts at most one capture per schedule interval per profile.
- A profile that is overdue because its runs are failing gets no extra notification -
  the run's own failure notification already covers it, and two channels saying the same
  thing is the red row people learn to skim.
- The lock is unchanged: `one_run` takes it, and a run already in progress is `Ok(None)`.
- `overdue()` keeps reading the last success. `status` semantics do not move.

---

## Migration and rollout

`(not applicable)` -- no schema change. The guard is derived from records the state file
already holds, deliberately: a `last_notified` field was the first design and was dropped
once the attempt record turned out to answer the same question without new persisted
state.

**Rollback:** revert. The agents return to push-only.

---

## Edge cases

| Case | Behaviour |
|------|-----------|
| Scheduler wedged, no runs at all | Overdue, no attempt this interval: notify and run. The incident case |
| Runs happening and failing | Overdue, but attempted: silent here, `announce` already notifies per run |
| A run in progress when catch-up fires | `one_run` gets `Ok(None)` from the lock and returns success |
| Profile has never run | `overdue_by` returns `Some(interval)`, no attempt exists: notify and run |
| Profile has no schedule | `overdue()` returns `None` at the first `?`. Catch-up stays push-only |
| Machine off for a week | First catch-up invocation after boot notices and runs |
| Two profiles, one overdue | Independent: `push --all` loops per profile |
| The catch-up run itself fails | It recorded an attempt, so the next hour is quiet. The failure notified through `announce` |

---

## Privacy, security, and cost notes

- **Privacy / Security:** `(none)`.
- **Cost:** in the normal case the check is two comparisons against records already
  loaded, and the agent exits as before. In the abnormal case it starts one backup that
  should have happened anyway. The guard is what bounds the abnormal case to one per
  interval rather than one per hour.

---

## Drawbacks

- **An hourly agent can now start a full backup.** That is the point, and it is still a
  behaviour change to a job that previously only pushed. A capture started at an
  unexpected hour will read the watched tree and write to the store. The interval guard
  bounds it; nothing bounds how long a single catch-up run takes.
- **The guard is a heuristic about intent.** "Something attempted a run this interval"
  is not the same as "a run is scheduled to happen soon". A profile whose agent fires
  and immediately fails for an unrelated reason still suppresses the catch-up for that
  interval.
- **It fixes the symptom, not the wedge.** If launchd wedges again the backup still
  depends on the catch-up agent staying healthy. Two independent jobs failing together
  is less likely than one, which is the whole argument, but it is not a guarantee.

---

## Alternatives considered

### A. Make `doctor` decode exit 78 and print a remedy

Cheap and genuinely useful. Rejected as the fix: `doctor` already reported
`agent fail loaded, exit 78` accurately throughout the outage and nobody ran it. A
diagnostic nobody reads is not a safety net, and improving its wording does not change
that. Kept as deferred work on its own merits.

### B. Have `__bootstrap` restart the agents after replacing the binary

The original plan, and it is what cleared the wedge by hand. Rejected: the mechanism it
assumes could not be reproduced, so it would be a change justified by a guess. It also
only covers wedges caused by bootstrap, and the next one will not be.

### C. Add a `last_notified` timestamp to the state file and throttle on it

The first design here. Rejected once the attempt record was found to answer the same
question: a new persisted field to avoid re-deriving something the file already records
is state that can go stale and disagree.

### D. Implement section 1 literally, with no attempt guard

What the documentation actually says. Rejected on measurement, not taste: `cex` is
overdue right now because `ghost` is red, so the literal reading starts a full capture
every hour forever on this machine today.

---

## Prior art and related work

- **Our platform, on this exact question.** `man launchd.plist` is already quoted in
  `scheduling.md`: launchd starts a missed calendar job on wake, and says nothing about
  power-off. The project read that correctly and specified its own check. The gap is
  between the specification and the code, so the prior art here is the doc itself.
- **What launchd does not guarantee, and the absence is the finding.** There is no
  documented signal to a job that launchd has stopped spawning it - a wedged job is
  invisible from inside itself. That is why the watchdog has to be a *different* job, and
  it is why this RFC puts the check in the catch-up agent rather than anywhere in the
  profile agent's own path.
- **How adjacent schedulers draw the same line.** systemd timers carry
  `Persistent=true`, which runs a missed timer on next boot, and anacron exists purely to
  answer "the machine was off, run what was missed". Both make the missed-run recovery a
  property of the scheduler. macOS gives only the sleep half, so the recovery has to be
  built in the application - which is what section 1 concluded.
- **The general problem is a watchdog with a liveness signal**, and the standard shape is
  exactly this: a second, independent process checks a timestamp written by the first.
  The known failure of that shape is the watchdog acting too eagerly on a stuck signal,
  which is the hourly-forever case Alternative D would have shipped, and the interval
  guard is the standard answer to it.

---

## Future possibilities

- `doctor` decoding agent exit codes (Alternative A) becomes the second layer once this
  first one exists.
- If the catch-up agent is going to start captures, it is the natural place for any
  future "the machine has been off for a month" recovery behaviour.

---

## Decisions (resolved)

### D1. Fix the silence, or fix the wedge

**Chosen:** the silence (author, from a failed reproduction). The wedge's cause did not
reproduce under test, and a fix aimed at an unverified mechanism would be guesswork. The
silence is demonstrable, documented as already-solved, and cause-independent.

### D2. Notify only, or notify and run

**Chosen:** both, because section 1 already specifies both and half of it would leave the
doc still wrong. Made safe by D3.

### D3. Guard the catch-up run on last attempt rather than last success

**Chosen:** last attempt (author). Measured: `overdue()` ignores `Outcome::Failed`, and
`cex` is `Failed` on every run while `ghost` is red, so the unguarded version starts an
hourly capture loop on this machine as it stands today.

### D4. No new state field

**Chosen:** derive from the existing run records (author). Rejected: Alternative C.

---

## Open questions

`(none)`

---

## Files that will change

| File                                             | Change  | Note                                             |
|--------------------------------------------------|---------|--------------------------------------------------|
| `src/cli/run.rs`                                 | edit    | `attempted_within`; `one_push` checks and acts   |
| `src/cli/run.rs`                                 | edit    | `overdue`'s doc comment describes what it does   |
| `src/config.rs`                                  | edit    | `overdue_by`'s doc comment loses the false claim |
| `docs/architecture/scheduling.md`                | edit    | section 1 gains the one-attempt-per-interval guard |
| `docs/rfc/004-overdue-check-on-every-agent.md`   | **NEW** | this RFC                                         |

---

## Verification checklist

### Automated

- [x] `scripts/ci-check.sh` green, `REAL EXIT CODE: 0` read outside a pipe
- [x] A test asserts a profile overdue with no recent attempt is acted on --
      `a_profile_nothing_has_attempted_is_overdue_and_unattempted`
- [x] A test asserts a profile overdue **with** a recent failed attempt is not --
      `a_profile_whose_runs_all_fail_is_overdue_but_has_been_attempted`, the hourly-loop
      guard

### Manual

- [x] `tycho push --all` on this machine returned in 0.677s and started nothing, with
      `cex` showing `OVERDUE by 12d 0h` -- the guard holding on live state
- [x] Forcing the stale case (state records backdated 9 days, state file backed up
      first) printed `cex  overdue by 7d 18h, nothing attempted this interval -
      starting one`, notified, and captured. The run wrote a fresh record, and the next
      `push --all` started nothing -- the guard re-engaging on its own

---

## Sources

- [`man launchd.plist`](https://keith.github.io/xcode-man-pages/launchd.plist.5.html) -- the sleep-only catch-up promise section 1 is built on
- [`systemd.timer` -- `Persistent=`](https://www.freedesktop.org/software/systemd/man/latest/systemd.timer.html) -- the adjacent platform making missed-run recovery a scheduler property
