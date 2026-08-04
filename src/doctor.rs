//! Layer 5. Every health question in one place, one check per row.
//!
//! `doctor` follows the same exit policy as everything else, so the one command that
//! aggregates health cannot report a failure while exiting 0.

pub mod artifacts;

use crate::config::{Config, Profile};
use crate::platform::{Agent, Loaded};
use crate::platform::{log_dir, notify};
use crate::remote;
use crate::service;
use crate::state::State;
use crate::store::Store;
use crate::sys::process::{Timeout, command};
use std::collections::BTreeMap;

/// One check's verdict. Three states, because "I did not measure this" is a real
/// answer and reporting it as `ok` would be a lie the whole design is against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    Warn,
    Fail,
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        })
    }
}

/// One row. Evidence beside the verdict, always - a bare `warn` is something you
/// learn to ignore.
#[derive(Clone, Debug)]
pub struct Check {
    pub name: String,
    pub verdict: Verdict,
    pub evidence: String,
}

impl Check {
    fn new(name: &str, verdict: Verdict, evidence: impl Into<String>) -> Self {
        Self {
            name: name.to_owned(),
            verdict,
            evidence: evidence.into(),
        }
    }
}

/// A titled group of checks.
#[derive(Clone, Debug)]
pub struct Section {
    pub title: String,
    pub checks: Vec<Check>,
}

/// The whole report.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub sections: Vec<Section>,
}

impl Report {
    #[must_use]
    pub fn counts(&self) -> (usize, usize) {
        let all = self.sections.iter().flat_map(|section| &section.checks);
        (
            all.clone()
                .filter(|check| check.verdict == Verdict::Fail)
                .count(),
            all.filter(|check| check.verdict == Verdict::Warn).count(),
        )
    }
}

/// What `doctor` was asked to look at.
#[derive(Clone, Debug, Default)]
pub struct Scope {
    /// Send a test notification and measure the agent's Full Disk Access grant
    /// through launchd. Both are actions rather than readings, which is why they are
    /// opt-in.
    pub deep: bool,
    /// Limit the remote checks to one name.
    pub remote: Option<String>,
}

/// Runs every check.
#[must_use]
pub fn run(config: &Config, state: &State, scope: &Scope) -> Report {
    let mut report = Report::default();
    report.sections.push(environment(config, scope));
    for profile in &config.profiles {
        report.sections.push(profile_section(profile, state, scope));
    }
    report.sections.push(volumes(config));
    report
}

/// Nothing to ask on a platform with no path-length limit to switch off.
#[cfg(not(windows))]
fn long_paths() -> Option<Check> {
    None
}

/// The agent's Full Disk Access grant, which plain `doctor` cannot measure.
///
/// An interactive `doctor` runs with the terminal's TCC grant, a different grant from
/// the agent's, so reading a watched root here proves nothing about what the agent can
/// do. Only a probe through the scheduler measures the real thing, which is what
/// `--deep` does.
#[cfg(target_os = "macos")]
fn full_disk_access() -> Option<Check> {
    Some(Check::new(
        "full disk access",
        Verdict::Warn,
        "not measurable from a terminal, use --deep",
    ))
}

/// TCC is Apple's, and there is no equivalent to report.
///
/// Windows has no per-application grant over the user's own files, so the row would
/// warn on every machine forever and say nothing - and a health check whose rows are
/// always yellow teaches people to skim it. What *can* deny a scheduled run there is
/// an ACL, and that fails the run loudly.
#[cfg(not(target_os = "macos"))]
fn full_disk_access() -> Option<Check> {
    None
}

/// Whether git will open a path past 260 characters.
///
/// This is a `Warn` and not a `Fail` because most trees never reach the limit, and a
/// row that is red on every machine is a row people learn to ignore. It earns its
/// place because of *how* it fails: with `core.longpaths` off, `git add` on a
/// directory past the limit prints `Filename too long`, **exits 0**, and stages
/// nothing - so a captured repository loses files while the run stays green. That is
/// the failure class this project exists to prevent, and it is invisible without
/// being told.
///
/// `config.md` section 10 records the measurement. The Windows-wide
/// `LongPathsEnabled` is a separate switch and does not stand in for this one: git
/// consults its own.
#[cfg(windows)]
fn long_paths() -> Option<Check> {
    let enabled = command(
        "git",
        &["config", "--get", "core.longpaths"],
        Timeout::QUICK,
    )
    .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "true");

    Some(if enabled {
        Check::new("core.longpaths", Verdict::Ok, "true")
    } else {
        Check::new(
            "core.longpaths",
            Verdict::Warn,
            "unset; git skips paths past 260 characters at exit 0. \
             git config --global core.longpaths true",
        )
    })
}

/// Whether Spotlight is indexing a removable backup volume, which can destroy it.
///
/// Observed on a 512 GB exFAT drive that stopped mounting on macOS: `fsck_exfat`
/// reported nineteen faults and **every one of them was a duplicate name inside
/// `.Spotlight-V100/Store-V2/`** - `0.indexBigDates`, `live.0.indexTermIds` and so on.
/// None were in user data. It gave up at `Cannot create a generated name for
/// 0.indexBigDates` and declared the volume unrepairable, and the volume has not
/// mounted since, read-only or otherwise.
///
/// exFAT keeps no journal, so an interrupted directory write is permanent rather than
/// replayed at mount. Spotlight rewrites its index constantly and invisibly, which
/// keeps that window open on a drive people unplug.
///
/// **Asked of `mdutil`, not inferred from `.Spotlight-V100` existing.** An earlier
/// version of this check tested for the directory, which is wrong twice over: macOS
/// recreates it on mount whatever the indexing state, and `.metadata_never_index` -
/// which this check used to recommend - does not stop indexing on a volume that is
/// already mounted. Measured: with that file in place and the directory freshly
/// deleted, `mdutil -s` still reported `Indexing enabled` and 91 index files were back
/// within minutes. Testing the symptom rather than the cause is how the check came to
/// report success for the wrong reason.
///
/// `mdutil -s` reads state and needs no privilege; turning indexing off does, which is
/// why the remedy is printed rather than performed.
#[cfg(target_os = "macos")]
fn spotlight(folder: &std::path::Path, name: &str) -> Option<Check> {
    let volume = crate::sys::volume::mount_point(folder).ok()??;
    if volume == std::path::Path::new("/") {
        return None;
    }
    // A journaled volume replays an interrupted write at mount, so Spotlight there is
    // wasted IO rather than a hazard - and a row that warns where there is no hazard
    // is the kind people learn to skim. This fired on an APFS drive while claiming
    // exFAT keeps no journal, which was the check asserting a reason it had not
    // checked.
    if crate::sys::volume::is_journaled(&volume).ok()? {
        return None;
    }
    let out = command(
        "mdutil",
        &["-s", &volume.display().to_string()],
        Timeout::QUICK,
    )
    .ok()?;
    // Only a positive answer counts. A volume mdutil cannot speak for is not evidence
    // of anything, and a row that fires on "I could not tell" is a row people skim.
    if !String::from_utf8_lossy(&out.stdout).contains("Indexing enabled") {
        return None;
    }
    Some(Check::new(
        &format!("{name} spotlight"),
        Verdict::Warn,
        format!(
            "Spotlight is indexing {}, which keeps no journal - so an interrupted index \
             write stays broken and can corrupt the volume past repair. \
             sudo mdutil -i off {} && sudo mdutil -E {}",
            volume.display(),
            volume.display(),
            volume.display()
        ),
    ))
}

/// Spotlight is Apple's, and so is the failure.
#[cfg(not(target_os = "macos"))]
fn spotlight(_folder: &std::path::Path, _name: &str) -> Option<Check> {
    None
}

fn environment(config: &Config, scope: &Scope) -> Section {
    let mut checks = Vec::new();

    checks.push(match command("git", &["--version"], Timeout::QUICK) {
        Ok(out) if out.status.success() => Check::new(
            "git",
            Verdict::Ok,
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .trim_start_matches("git version ")
                .to_owned(),
        ),
        _ => Check::new(
            "git",
            Verdict::Fail,
            "not runnable; nothing works without it",
        ),
    });

    checks.push(Check::new(
        "config",
        Verdict::Ok,
        format!(
            "{}, no errors",
            crate::cli::render::plural(config.profiles.len(), "profile")
        ),
    ));

    if let Some(check) = long_paths() {
        checks.push(check);
    }

    checks.push(match log_dir() {
        Ok(dir) => {
            let path = dir.as_path();
            // launchd silently drops output when this does not exist, so a missing
            // one means the diagnostic trail the design leans on is not being written.
            if path.is_dir() {
                let writable = std::fs::OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(path.join(".tycho-doctor-probe"))
                    .inspect(|_| {
                        let _ = std::fs::remove_file(path.join(".tycho-doctor-probe"));
                    })
                    .is_ok();
                if writable {
                    Check::new(
                        "log directory",
                        Verdict::Ok,
                        format!("{}, writable", path.display()),
                    )
                } else {
                    Check::new(
                        "log directory",
                        Verdict::Fail,
                        format!("{} is not writable", path.display()),
                    )
                }
            } else {
                Check::new(
                    "log directory",
                    Verdict::Warn,
                    format!(
                        "{} does not exist; service install creates it",
                        path.display()
                    ),
                )
            }
        }
        Err(error) => Check::new("log directory", Verdict::Fail, error.to_string()),
    });

    checks.push(if notify::available() {
        if scope.deep {
            match notify::notify(notify::Urgency::Info, "doctor --deep test notification") {
                Ok(()) => Check::new("notifications", Verdict::Ok, "test notification sent"),
                Err(error) => Check::new("notifications", Verdict::Fail, error.to_string()),
            }
        } else {
            // Delivery cannot be measured without sending one, and a health check
            // that sends a banner nobody asked for is a health check people disable.
            Check::new(
                "notifications",
                Verdict::Warn,
                "available, delivery not tested, use --deep",
            )
        }
    } else {
        Check::new(
            "notifications",
            Verdict::Warn,
            "no mechanism on this platform; the exit code is the contract",
        )
    });

    if let Some(check) = full_disk_access() {
        checks.push(check);
    }

    Section {
        title: "environment".to_owned(),
        checks,
    }
}

fn profile_section(profile: &Profile, state: &State, scope: &Scope) -> Section {
    let mut checks = Vec::new();
    let agent = Agent::Backup(profile.name.to_string());

    checks.push(match service::inspect(agent) {
        Ok(installed) => {
            let state_word = match installed.loaded {
                Loaded::No => None,
                Loaded::Yes { last, .. } => Some(last),
            };
            match state_word {
                None => Check::new(
                    "agent",
                    Verdict::Warn,
                    "not loaded; nothing is scheduled, so nothing runs by itself",
                ),
                Some(last) if last.is_clean() => {
                    Check::new("agent", Verdict::Ok, "loaded, last exit 0")
                }
                // The evidence that revealed a year of silent failure in the old
                // system, shown without being asked for.
                Some(last) => Check::new("agent", Verdict::Fail, format!("loaded, {last}")),
            }
        }
        Err(error) => Check::new("agent", Verdict::Fail, error.to_string()),
    });

    // Drift between what the config says and what is actually installed is precisely
    // what killed the old system, and it is a cheap comparison.
    checks.push(
        match service::inspect(Agent::Backup(profile.name.to_string())) {
            Ok(installed) => match installed.matches(profile.schedule) {
                None => Check::new(
                    "agent schedule",
                    Verdict::Warn,
                    "nothing installed to compare",
                ),
                Some(true) => Check::new("agent schedule", Verdict::Ok, "matches config"),
                Some(false) => Check::new(
                    "agent schedule",
                    Verdict::Fail,
                    format!(
                        "installed {} but the config says {}",
                        installed
                            .scheduled
                            .map_or_else(|| "nothing".to_owned(), crate::cli::render::say),
                        profile
                            .schedule
                            .map_or_else(|| "nothing".to_owned(), crate::cli::render::say)
                    ),
                ),
            },
            Err(error) => Check::new("agent schedule", Verdict::Fail, error.to_string()),
        },
    );

    checks.push(match overdue(profile, state) {
        None => Check::new("schedule", Verdict::Ok, "not overdue"),
        Some(over) => Check::new(
            "schedule",
            Verdict::Fail,
            format!(
                "overdue by {}",
                crate::cli::render::until(i64::try_from(over.as_secs()).unwrap_or(i64::MAX))
            ),
        ),
    });

    let store = crate::platform::store_path(profile.name.as_str(), profile.store_path.as_ref())
        .ok()
        .and_then(|path| Store::open_to_read(&path).ok());
    checks.push(match &store {
        Some(store) => match store.span() {
            Ok(Some(span)) => Check::new(
                "store",
                Verdict::Ok,
                format!(
                    "{}, newest {}",
                    crate::cli::render::plural(span.backups, "backup"),
                    crate::cli::render::day(&span.newest)
                ),
            ),
            Ok(None) => Check::new("store", Verdict::Warn, "exists but holds no backups yet"),
            Err(error) => Check::new("store", Verdict::Fail, error.to_string()),
        },
        None => Check::new("store", Verdict::Warn, "not created yet; run once"),
    });

    if let Some(store) = &store {
        checks.push(match store.repo().for_each_ref("refs/") {
            Ok(refs) => Check::new(
                "refs",
                Verdict::Ok,
                crate::cli::render::plural(refs.len(), "ref"),
            ),
            Err(error) => Check::new("refs", Verdict::Fail, error.to_string()),
        });

        // Cheap by default. A full fsck reads every object, and on a cloud remote
        // that would download the entire backup - see remotes.md section 3.
        let tier = if scope.deep {
            vec!["fsck", "--no-progress"]
        } else {
            vec!["fsck", "--connectivity-only", "--no-progress"]
        };
        checks.push(match store.repo().git().run(&tier, Timeout::WORK) {
            Ok(out) if out.status.success() => Check::new(
                "objects",
                Verdict::Ok,
                if scope.deep {
                    "full fsck clean"
                } else {
                    "connectivity clean"
                },
            ),
            Ok(out) => Check::new(
                "objects",
                Verdict::Fail,
                String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .next()
                    .unwrap_or("fsck reported problems")
                    .to_owned(),
            ),
            Err(error) => Check::new("objects", Verdict::Fail, error.to_string()),
        });
    }

    for configured in &profile.remotes {
        if scope
            .remote
            .as_deref()
            .is_some_and(|wanted| wanted != configured.name.as_str())
        {
            continue;
        }
        let name = configured.name.to_string();
        let current = state.remote(profile.name.as_str(), &name);
        let tolerance = crate::store::run::tolerance(configured);

        checks.push(match &current {
            crate::remote::state::RemoteState::Failed { reason, .. } => {
                Check::new(&name, Verdict::Fail, reason.to_string())
            }
            crate::remote::state::RemoteState::Behind { runs, .. } => Check::new(
                &name,
                Verdict::Warn,
                format!(
                    "behind {runs} of {tolerance}{}",
                    if configured.optional {
                        ", optional"
                    } else {
                        ""
                    }
                ),
            ),
            crate::remote::state::RemoteState::Unseen => {
                Check::new(&name, Verdict::Warn, "never pushed")
            }
            crate::remote::state::RemoteState::Synced { .. } => {
                Check::new(&name, Verdict::Ok, "all refs present, verified")
            }
        });

        if let Ok(folder) = remote::resolve(&configured.path) {
            // The store is refused on a volume like this; a remote is only warned
            // about. Refusing it would ban the cross-platform external drive outright,
            // because exFAT is the only filesystem macOS and Windows can both write and
            // it records no ownership - and no offsite copy is worse than an exposed
            // one. The choice is the user's; saying nothing would take it from them.
            if crate::sys::volume::records_ownership(&folder).is_ok_and(|records| !records) {
                checks.push(Check::new(
                    &format!("{name} privacy"),
                    Verdict::Warn,
                    format!(
                        "{} records no ownership, so this backup is readable by anyone \
                         with the disk; encrypt the volume or accept it",
                        folder.display()
                    ),
                ));
            }

            if let Some(check) = spotlight(&folder, &name) {
                checks.push(check);
            }

            let found = artifacts::scan(&folder);
            if !found.is_empty() {
                let first = &found[0];
                checks.push(Check::new(
                    &format!("{name} artifacts"),
                    Verdict::Fail,
                    format!(
                        "{}: {} ({})",
                        crate::cli::render::plural(found.len(), "file"),
                        first.path.file_name().unwrap_or_default().to_string_lossy(),
                        first.kind
                    ),
                ));
            }
        }
    }

    Section {
        title: profile.name.to_string(),
        checks,
    }
}

/// Grouped by disk rather than by profile, because that is how the constraint works:
/// several profiles can share a drive, and the store keeps full history, so free
/// space is what they contend for.
fn volumes(config: &Config) -> Section {
    let mut wanted: BTreeMap<String, usize> = BTreeMap::new();
    for profile in &config.profiles {
        if let Ok(store) =
            crate::platform::store_path(profile.name.as_str(), profile.store_path.as_ref())
        {
            *wanted.entry(volume_of(store.as_path())).or_default() += 1;
        }
        for configured in &profile.remotes {
            if let Ok(folder) = remote::resolve(&configured.path) {
                *wanted.entry(volume_of(&folder)).or_default() += 1;
            }
        }
    }

    let checks = wanted
        .into_iter()
        .map(|(volume, users)| match free_bytes(&volume) {
            Some(free) => {
                // Ten gigabytes is not a threshold with a story behind it; it is
                // enough to notice before a store that grows monotonically stops
                // being able to write.
                let verdict = if free < 10_000_000_000 {
                    Verdict::Warn
                } else {
                    Verdict::Ok
                };
                Check::new(
                    &volume,
                    verdict,
                    format!(
                        "{} free, {}",
                        crate::cli::render::size(free),
                        crate::cli::render::plural(users, "user")
                    ),
                )
            }
            None => Check::new(&volume, Verdict::Warn, "not mounted"),
        })
        .collect();

    Section {
        title: "volumes".to_owned(),
        checks,
    }
}

/// The deepest existing ancestor's mount point, so a store on an unplugged drive is
/// reported against the drive rather than against `/`.
fn volume_of(path: &std::path::Path) -> String {
    let mut current = path;
    loop {
        if current.exists() {
            return command(
                "df",
                &["-P", &current.display().to_string()],
                Timeout::QUICK,
            )
            .ok()
            .and_then(|out| {
                let text = String::from_utf8_lossy(&out.stdout);
                let line = text.lines().nth(1)?;
                line.split_whitespace().last().map(str::to_owned)
            })
            .unwrap_or_else(|| current.display().to_string());
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => return path.display().to_string(),
        }
    }
}

fn free_bytes(volume: &str) -> Option<u64> {
    let out = command("df", &["-Pk", volume], Timeout::QUICK).ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().nth(1)?;
    let blocks: u64 = line.split_whitespace().nth(3)?.parse().ok()?;
    Some(blocks * 1024)
}

fn overdue(profile: &Profile, state: &State) -> Option<std::time::Duration> {
    let schedule = profile.schedule?;
    let last = state
        .profiles
        .get(profile.name.as_str())
        .into_iter()
        .flatten()
        .find(|run| run.outcome != crate::state::Outcome::Failed)
        .and_then(|run| run.when.parse::<jiff::Timestamp>().ok())
        .map(|stamp| stamp.to_zoned(jiff::tz::TimeZone::system()));
    let now = jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::system());
    schedule.overdue_by(last.as_ref(), &now)
}
