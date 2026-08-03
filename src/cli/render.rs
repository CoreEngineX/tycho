//! Rendering, to the column model in `cli.md` section 2.
//!
//! A fixed column spec per table shared by header, rule and rows; text left and
//! numbers right so digits line up on their ones place; rows indent two spaces and
//! section headers do not; a rule spans exactly the table's width.

use crate::capture::Inspection;
use crate::config::{Diagnostic, Severity};
use crate::plan::{Plan, REPO_TABLE_ROWS, RepoHead};
use std::fmt::Write as _;

/// Total width, which fits an eighty-column terminal.
pub const WIDTH: usize = 74;

/// `8,412`.
#[must_use]
pub fn count(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// `1.19 GB`, `340 MB`, `0 B`. Three significant figures, decimal units, matching
/// the examples in `cli.md`.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        return format!("{bytes} B");
    }
    let places = if value < 10.0 {
        2
    } else if value < 100.0 {
        1
    } else {
        0
    };
    format!("{value:.places$} {}", UNITS[unit])
}

fn rule(out: &mut String) {
    let _ = writeln!(out, "{}", "-".repeat(WIDTH));
}

/// `1 file`, `4 files`, `1 repository`, `12 repositories`.
#[must_use]
pub fn plural(value: usize, word: &str) -> String {
    if value == 1 {
        return format!("{} {word}", count(value));
    }
    match word.strip_suffix('y') {
        Some(stem) => format!("{} {stem}ies", count(value)),
        None => format!("{} {word}s", count(value)),
    }
}

/// Keeps a value inside its column. A path's tail is the informative half, so an
/// over-long one loses its head rather than its name.
fn fit(text: &str, width: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= width {
        return text.to_owned();
    }
    let kept: String = chars[chars.len() - (width - 1)..].iter().collect();
    format!("~{kept}")
}

/// Paths render with `~` rather than the home directory spelled out, which is what
/// keeps a column model possible and what `cli.md`'s examples show.
fn abbreviate(path: &str) -> String {
    std::env::home_dir()
        .and_then(|home| {
            path.strip_prefix(home.to_string_lossy().as_ref())
                .map(|rest| format!("~{rest}"))
        })
        .unwrap_or_else(|| path.to_owned())
}

fn short(head: &RepoHead) -> String {
    match head {
        RepoHead::Branch { name, sha } => format!("{name} {}", sha.short()),
        RepoHead::Detached { sha } => format!("detached {}", sha.short()),
        RepoHead::Unborn => "unborn".to_owned(),
    }
}

/// The three tables of `cli.md` section 8, because the three questions are separate:
/// how much is coming, what repositories were found and in what state, and what the
/// rules threw away.
#[must_use]
pub fn dry_run(plan: &Plan, repos: &[(String, Inspection)], quick: bool) -> String {
    let mut out = String::new();

    let _ = writeln!(out, "{:<45}{:>12}{:>17}", "roots", "files", "size");
    rule(&mut out);
    for root in &plan.roots {
        let _ = writeln!(
            out,
            "  {:<43}{:>12}{:>17}",
            format!(
                "{:<14}{}",
                root.alias,
                fit(&abbreviate(&root.root.to_string()), 29)
            ),
            count(root.files),
            size(root.bytes)
        );
    }

    if !quick {
        let _ = writeln!(out);
        let _ = writeln!(out, "{:<42}{:<18}state", "repositories", "head");
        rule(&mut out);
        for (key, inspection) in repos.iter().take(REPO_TABLE_ROWS) {
            let _ = writeln!(
                out,
                "  {:<40}{:<18}{}",
                fit(key, 39),
                short(&inspection.head),
                inspection.state()
            );
        }
        // Truncated so the count and the rows agree.
        if repos.len() > REPO_TABLE_ROWS {
            let _ = writeln!(
                out,
                "  {:<40}{:<18}and {} more",
                "",
                "",
                repos.len() - REPO_TABLE_ROWS
            );
        }
    }

    let _ = writeln!(out);
    let _ = writeln!(out, "{:<50}reason", "excluded");
    rule(&mut out);
    for (rule_text, reason) in &plan.excluded {
        let _ = writeln!(
            out,
            "  {:<48}{}",
            fit(&abbreviate(rule_text), 47),
            reason.label()
        );
    }

    let _ = writeln!(out);
    rule(&mut out);
    let objects: u64 = repos.iter().map(|(_, item)| item.objects).sum();
    let _ = writeln!(
        out,
        "  {:<13}{:<40}{:>19}",
        "to read",
        format!("{} files", count(plan.files())),
        size(plan.bytes())
    );
    if !quick {
        let _ = writeln!(
            out,
            "  {:<13}{:<40}{:>19}",
            "",
            format!("{} repositories", count(plan.repo_count())),
            size(objects)
        );
    }
    out
}

/// `config check`'s echo, and its findings. Reading your own config back in
/// summarised form is how a remote attached to the wrong profile becomes visible.
#[must_use]
pub fn config_check(summaries: &[String], diagnostics: &[Diagnostic]) -> String {
    let mut out = String::new();
    for summary in summaries {
        let _ = writeln!(out, "{summary}");
    }

    let errors = diagnostics
        .iter()
        .filter(|item| item.severity == Severity::Error)
        .count();
    let warnings = diagnostics.len() - errors;

    if diagnostics.is_empty() {
        let _ = writeln!(out, "\nok, no errors");
        return out;
    }

    let mut profile = None;
    for diagnostic in diagnostics {
        if profile.as_ref() != Some(&diagnostic.profile) {
            let _ = writeln!(out);
            if let Some(name) = &diagnostic.profile {
                let _ = writeln!(out, "{name}");
            }
            profile = Some(diagnostic.profile.clone());
        }
        let label = match diagnostic.severity {
            Severity::Error => "error",
            Severity::Warning => "warn",
        };
        let _ = writeln!(out, "  {label:<7} {}", diagnostic.kind);
        if let Some(hint) = diagnostic.hint() {
            let _ = writeln!(out, "          {hint}");
        }
    }

    let _ = writeln!(
        out,
        "\n{} error{}, {} warning{}",
        errors,
        if errors == 1 { "" } else { "s" },
        warnings,
        if warnings == 1 { "" } else { "s" }
    );
    out
}

/// What a completed run says for itself.
#[must_use]
pub fn run_result(profile: &str, done: &crate::store::run::Completed) -> String {
    let summary = &done.summary;
    let mut out = format!("{profile}  {}\n", done.commit.short());
    let _ = writeln!(
        out,
        "  {:<13}{:<40}{:>19}",
        "captured",
        counted(summary),
        size(summary.tracked_bytes)
    );
    let _ = writeln!(
        out,
        "  {:<13}{:<40}{:>19}",
        "written",
        format!("in {}s", summary.seconds),
        size(summary.written_bytes)
    );
    out
}

/// `cli.md` section 4. `written` is read back out of each commit's own message, so
/// this renders from a bare clone with no state file - which is the disaster path.
#[must_use]
pub fn history(backups: &[crate::store::Backup]) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:<20}{:<10}{:<34}{:>10}",
        "when", "commit", "summary", "written"
    );
    rule(&mut out);

    let mut total = 0;
    for backup in backups {
        let (line, written) = backup.summary.as_ref().map_or_else(
            || ("not written by tycho".to_owned(), None),
            |summary| {
                let text = counted(summary);
                (text, Some(summary.written_bytes))
            },
        );
        total += written.unwrap_or_default();
        let _ = writeln!(
            out,
            "  {:<18}{:<10}{:<34}{:>10}",
            when(&backup.when),
            backup.commit.short(),
            fit(&line, 33),
            written.map_or_else(|| "-".to_owned(), size)
        );
    }

    rule(&mut out);
    let _ = writeln!(
        out,
        "  {:<18}{:<10}{:<34}{:>10}",
        "",
        "",
        format!("{} backups", count(backups.len())),
        size(total)
    );
    out
}

/// One destination's line in `status`.
#[derive(Clone, Debug)]
pub struct RemoteRow {
    pub name: String,
    /// `ok`, `behind 3 of 4`, `failed`, `unseen`. Meaning never depends on colour.
    pub word: String,
    pub detail: String,
    /// `verified`, or what to expect next. Empty is fine.
    pub note: String,
    /// What to type next when something is wrong.
    pub hint: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ProfileStatus {
    pub name: String,
    /// `Sun 12:00, in 6d 2h`, or absent when the profile has no schedule or the
    /// config could not be read.
    pub next_run: Option<String>,
    pub backups: usize,
    pub since: Option<String>,
    pub newest: Option<String>,
    /// **Read out of the state file**, never measured. Walking the object database on
    /// a command people run casually would make `status` slow in proportion to the
    /// size of the backup, and on a cloud remote it would materialise dataless files.
    pub store_bytes: u64,
    pub remotes: Vec<RemoteRow>,
}

const REMOTE_NAME: usize = 10;
const REMOTE_WORD: usize = 15;
const REMOTE_DETAIL: usize = 24;
const REMOTE_NOTE: usize = WIDTH - 2 - REMOTE_NAME - REMOTE_WORD - REMOTE_DETAIL;

/// `cli.md` section 3. Profile name and next run sit at opposite edges, so a glance
/// down the left lists your profiles and a glance down the right says when each
/// fires.
#[must_use]
pub fn status(profiles: &[ProfileStatus], banner: Option<&str>) -> String {
    let mut out = String::new();
    if let Some(banner) = banner {
        let _ = writeln!(out, "{banner}\n");
    }

    for profile in profiles {
        match &profile.next_run {
            Some(next) => {
                let right = format!("next run  {next}");
                let width = WIDTH.saturating_sub(profile.name.chars().count());
                let _ = writeln!(out, "{}{right:>width$}", profile.name);
            }
            None => {
                let _ = writeln!(out, "{}", profile.name);
            }
        }
        let _ = writeln!(out, "  {}", subtitle(profile));

        out.push('\n');
        if profile.remotes.is_empty() {
            let _ = writeln!(out, "  local only, no remotes configured");
        }
        for remote in &profile.remotes {
            let _ = writeln!(
                out,
                "  {:<REMOTE_NAME$}{:<REMOTE_WORD$}{:<REMOTE_DETAIL$}{:>REMOTE_NOTE$}",
                fit(&remote.name, REMOTE_NAME - 1),
                fit(&remote.word, REMOTE_WORD - 1),
                fit(&remote.detail, REMOTE_DETAIL - 1),
                fit(&remote.note, REMOTE_NOTE)
            );
            if let Some(hint) = &remote.hint {
                let _ = writeln!(out, "{}{hint}", " ".repeat(2 + REMOTE_NAME + REMOTE_WORD));
            }
        }
        out.push('\n');
    }
    out
}

fn subtitle(profile: &ProfileStatus) -> String {
    let mut parts = vec![format!(
        "{} backup{}",
        count(profile.backups),
        if profile.backups == 1 { "" } else { "s" }
    )];
    if let Some(since) = &profile.since {
        parts.push(format!("since {since}"));
    }
    let mut line = parts.join(" ");
    if let Some(newest) = &profile.newest {
        let _ = write!(line, ", newest {newest}");
    }
    let _ = write!(line, ", store {}", size(profile.store_bytes));
    line
}

/// `6d 2h`, `8h 46m`, `12m`. Two units, because the third never changes a decision.
#[must_use]
pub fn until(seconds: i64) -> String {
    if seconds <= 0 {
        return "now".to_owned();
    }
    let (days, hours, minutes) = (
        seconds / 86_400,
        (seconds % 86_400) / 3_600,
        (seconds % 3_600) / 60,
    );
    if days > 0 {
        return format!("{days}d {hours}h");
    }
    if hours > 0 {
        return format!("{hours}h {minutes}m");
    }
    format!("{minutes}m")
}

/// The wall-clock half of the next-run column, in the reader's own time zone.
#[must_use]
pub fn upcoming(next: &jiff::Zoned) -> String {
    let now = jiff::Timestamp::now().to_zoned(next.time_zone().clone());
    let days = next
        .date()
        .since(now.date())
        .map_or(99, |span| span.get_days());
    let clock = next.strftime("%H:%M").to_string();
    let label = match days {
        0 => format!("today {clock}"),
        1 => format!("tomorrow {clock}"),
        2..=6 => next.strftime("%a %H:%M").to_string(),
        _ => next.strftime("%F %H:%M").to_string(),
    };
    let seconds = (next.timestamp().as_second()) - jiff::Timestamp::now().as_second();
    format!("{label}, in {}", until(seconds))
}

/// What a restore produced.
#[must_use]
pub fn restored(into: &std::path::Path, done: &crate::restore::Done) -> String {
    let mut out = String::new();
    if let Some(commit) = done.commit {
        let _ = writeln!(out, "using     {}", commit.short());
    }
    for (path, resolved) in &done.resolved {
        let _ = writeln!(
            out,
            "resolved  {:<28}{}",
            fit(&path.display().to_string(), 27),
            resolved.source()
        );
    }
    if let Some(bundle) = &done.bundle {
        let _ = writeln!(
            out,
            "bundled   {}",
            abbreviate(&bundle.display().to_string())
        );
        return out;
    }

    let _ = writeln!(
        out,
        "restored  {:<40}{:>24}",
        plural(done.files, "file"),
        size(done.bytes)
    );
    if !done.repos.is_empty() {
        let _ = writeln!(
            out,
            "restored  {} with full history",
            plural(done.repos.len(), "repository")
        );
        for repo in done.repos.iter().take(REPO_TABLE_ROWS) {
            let _ = writeln!(
                out,
                "          {:<34}{:<12}{}",
                fit(&repo.key, 33),
                repo.head.as_deref().unwrap_or("no commits"),
                overlay_note(repo)
            );
        }
        if done.repos.len() > REPO_TABLE_ROWS {
            let _ = writeln!(
                out,
                "          and {} more",
                done.repos.len() - REPO_TABLE_ROWS
            );
        }
    }

    // Named rather than resolved, and the file it declined to write is still in the
    // staging tree under `.tycho/`, which is why that tree is never deleted.
    for repo in &done.repos {
        for conflict in &repo.conflicts {
            let _ = writeln!(
                out,
                "conflict  {:<34}{}",
                fit(&conflict.path.display().to_string(), 33),
                conflict.reason
            );
        }
    }

    let _ = writeln!(
        out,
        "\nnote      file permissions, timestamps and extended attributes are not\n\
         \x20         restored - re-secure anything secret-bearing\n\n\
         done      {}",
        abbreviate(&into.display().to_string())
    );
    out
}

fn overlay_note(repo: &crate::restore::Rebuilt) -> String {
    match (repo.overlay, repo.stashes) {
        (0, 0) => "overlay: clean".to_owned(),
        (files, 0) => format!("overlay: {}", count(files)),
        (0, stashes) => format!("{} stashed", count(stashes)),
        (files, stashes) => format!("overlay: {}, {} stashed", count(files), count(stashes)),
    }
}

/// `history --path`. The header names which half of the store answered, because a
/// list of commits is meaningless without knowing whether they are backup runs or a
/// repository's own history.
#[must_use]
pub fn path_history(
    path: &std::path::Path,
    resolved: &crate::restore::resolve::Resolved,
    commits: &[crate::git::read::Commit],
) -> String {
    let mut out = format!(
        "resolved  {}\n",
        fit(&path.display().to_string(), WIDTH - 10)
    );
    let _ = writeln!(out, "          {}\n", resolved.source());

    let _ = writeln!(out, "{:<20}{:<10}summary", "when", "commit");
    rule(&mut out);
    for commit in commits {
        let _ = writeln!(
            out,
            "  {:<18}{:<10}{}",
            when(&commit.when),
            commit.oid.short(),
            fit(&commit.subject, WIDTH - 30)
        );
    }
    out
}

/// Only the non-zero parts, so a run that only changed files reads `2 changed`
/// rather than `2 changed, 0 added, 0 deleted`.
#[must_use]
pub fn counted(summary: &crate::store::message::Summary) -> String {
    if summary.is_empty() {
        return "no changes".to_owned();
    }
    let mut parts = Vec::new();
    for (value, word) in [
        (summary.changed, "changed"),
        (summary.added, "added"),
        (summary.deleted, "deleted"),
    ] {
        if value > 0 {
            parts.push(format!("{} {word}", count(value)));
        }
    }
    parts.join(", ")
}

/// Just the date, for a lag measured in days where the hour is noise.
#[must_use]
pub fn day(rfc3339: &str) -> String {
    rfc3339.parse::<jiff::Timestamp>().map_or_else(
        |_| rfc3339.to_owned(),
        |stamp| {
            stamp
                .to_zoned(jiff::tz::TimeZone::system())
                .strftime("%F")
                .to_string()
        },
    )
}

/// Recent stamps read as `today` and `yesterday`, older ones as dates - and all of
/// them in local time, because that is the clock the person reading them lives on.
#[must_use]
pub fn moment(rfc3339: &str) -> String {
    when(rfc3339)
}

fn when(rfc3339: &str) -> String {
    let Ok(stamp) = rfc3339.parse::<jiff::Timestamp>() else {
        return rfc3339.to_owned();
    };
    let zoned = stamp.to_zoned(jiff::tz::TimeZone::system());
    let today = jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::system())
        .date();
    let days = today.since(zoned.date()).map_or(99, |span| span.get_days());
    match days {
        0 => format!("today {}", zoned.strftime("%H:%M")),
        1 => format!("yesterday {}", zoned.strftime("%H:%M")),
        _ => zoned.strftime("%F %H:%M").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{count, size};

    #[test]
    fn counts_carry_thousands_separators() {
        assert_eq!(count(0), "0");
        assert_eq!(count(126), "126");
        assert_eq!(count(8_412), "8,412");
        assert_eq!(count(8_538), "8,538");
        assert_eq!(count(1_234_567), "1,234,567");
    }

    #[test]
    fn sizes_match_the_documented_examples() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(999), "999 B");
        assert_eq!(size(1_190_000_000), "1.19 GB");
        assert_eq!(size(340_000_000), "340 MB");
        assert_eq!(size(1_100_000), "1.10 MB");
        assert_eq!(size(41_000_000), "41.0 MB");
        assert_eq!(size(204_000_000), "204 MB");
    }
}
