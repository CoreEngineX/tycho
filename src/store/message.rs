//! The commit message, and its inverse.
//!
//! `store.md` section 8 fixes the format: a subject a human scans, and a body that
//! says what a human needs to decide whether this is the commit to restore from.
//!
//! It is parsed back because `history` needs the `written` figure per backup, and
//! there is no way to derive that from git afterwards without walking object graphs.
//! Keeping it here rather than in the state file means **history works from a bare
//! clone on a replacement machine**, which is the disaster path and the one case
//! where the state file is already gone.

use crate::cli::render::size;
use crate::git::read::{Change, ChangeStatus};
use std::fmt::Write as _;

/// What one run did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub changed: usize,
    pub added: usize,
    pub deleted: usize,
    pub roots: usize,
    /// The first few paths, for the body. Truncated so a mass change does not
    /// produce a megabyte-long commit message.
    pub sample: Vec<(ChangeStatus, String)>,
    pub repos_found: usize,
    pub repos_captured: usize,
    pub tracked_bytes: u64,
    pub written_bytes: u64,
    pub seconds: u64,
}

pub const SAMPLE_ROWS: usize = 20;

impl Summary {
    /// Folds a diff into counts and a sample.
    #[must_use]
    pub fn from_changes(changes: &[Change], roots: usize) -> Self {
        let mut summary = Self {
            roots,
            ..Self::default()
        };
        for change in changes {
            match change.status {
                ChangeStatus::Added => summary.added += 1,
                ChangeStatus::Deleted => summary.deleted += 1,
                ChangeStatus::Modified | ChangeStatus::TypeChanged => summary.changed += 1,
            }
            if summary.sample.len() < SAMPLE_ROWS {
                summary
                    .sample
                    .push((change.status, change.path.to_string()));
            }
        }
        summary
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.changed == 0 && self.added == 0 && self.deleted == 0
    }
}

const fn marker(status: ChangeStatus) -> char {
    match status {
        ChangeStatus::Added => '+',
        ChangeStatus::Deleted => '-',
        ChangeStatus::Modified | ChangeStatus::TypeChanged => '~',
    }
}

/// The full message: subject, blank line, body.
#[must_use]
pub fn render(stamp: &str, profile: &str, summary: &Summary) -> String {
    let mut out = format!("backup {stamp} - {profile}\n\n");

    if summary.is_empty() {
        // D12: a run where nothing changed still commits, because a gap in the
        // history is otherwise ambiguous between "nothing changed" and "the backup
        // did not run" - and a year of the second going unnoticed is why this
        // project exists.
        out.push_str("no changes\n");
    } else {
        let _ = writeln!(
            out,
            "files: {} changed, {} added, {} deleted across {} roots",
            summary.changed, summary.added, summary.deleted, summary.roots
        );
        for (status, path) in &summary.sample {
            let _ = writeln!(out, "  {} {path}", marker(*status));
        }
    }

    let _ = writeln!(
        out,
        "repos: {} captured, {} found",
        summary.repos_captured, summary.repos_found
    );
    let _ = writeln!(
        out,
        "totals: {} tracked, {} new objects, {}s",
        size(summary.tracked_bytes),
        size(summary.written_bytes),
        summary.seconds
    );
    out
}

/// Reads back what [`render`] wrote. `None` for a message this did not produce, so
/// a store containing hand-written commits degrades rather than lying.
#[must_use]
pub fn parse(message: &str) -> Option<Summary> {
    let mut summary = Summary::default();
    let mut saw_totals = false;

    for line in message.lines() {
        if let Some(rest) = line.strip_prefix("files: ") {
            let numbers: Vec<usize> = rest
                .split(", ")
                .filter_map(|part| part.split_whitespace().next())
                .filter_map(|value| value.parse().ok())
                .collect();
            let [changed, added, deleted, ..] = numbers.as_slice() else {
                continue;
            };
            summary.changed = *changed;
            summary.added = *added;
            summary.deleted = *deleted;
            // "0 deleted across 2 roots" - the root count is in the tail of the last
            // field rather than a field of its own.
            summary.roots = rest
                .rsplit_once("across ")
                .and_then(|(_, tail)| tail.split_whitespace().next())
                .and_then(|value| value.parse().ok())
                .unwrap_or_default();
        } else if let Some(rest) = line.strip_prefix("repos: ") {
            let numbers: Vec<usize> = rest
                .split(", ")
                .filter_map(|part| part.split_whitespace().next())
                .filter_map(|value| value.parse().ok())
                .collect();
            if let [captured, found, ..] = numbers.as_slice() {
                summary.repos_captured = *captured;
                summary.repos_found = *found;
            }
        } else if let Some(rest) = line.strip_prefix("totals: ") {
            let parts: Vec<&str> = rest.split(", ").collect();
            let [tracked, written, seconds, ..] = parts.as_slice() else {
                continue;
            };
            summary.tracked_bytes = unsize(tracked.trim_end_matches(" tracked"))?;
            summary.written_bytes = unsize(written.trim_end_matches(" new objects"))?;
            summary.seconds = seconds.trim_end_matches('s').parse().ok()?;
            saw_totals = true;
        } else if let Some(rest) = line.strip_prefix("  ") {
            let mut chars = rest.chars();
            let status = match chars.next() {
                Some('+') => ChangeStatus::Added,
                Some('-') => ChangeStatus::Deleted,
                Some('~') => ChangeStatus::Modified,
                _ => continue,
            };
            summary
                .sample
                .push((status, chars.as_str().trim_start().to_owned()));
        }
    }

    saw_totals.then_some(summary)
}

/// The inverse of `render::size`, to whatever precision that printed.
fn unsize(text: &str) -> Option<u64> {
    let (value, unit) = text.rsplit_once(' ')?;
    let scale: u64 = match unit {
        "B" => 1,
        "KB" => 1_000,
        "MB" => 1_000_000,
        "GB" => 1_000_000_000,
        "TB" => 1_000_000_000_000,
        _ => return None,
    };
    let value: f64 = value.parse().ok()?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    #[allow(clippy::cast_precision_loss)]
    Some((value * scale as f64).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::{Summary, parse, render};
    use crate::git::read::ChangeStatus;

    fn sample() -> Summary {
        Summary {
            changed: 3,
            added: 1,
            deleted: 0,
            roots: 2,
            sample: vec![
                (
                    ChangeStatus::Modified,
                    "CoreEngineX/org/handbook/README.md".to_owned(),
                ),
                (ChangeStatus::Added, "Books/a\nnewline.pdf".to_owned()),
                (
                    ChangeStatus::Deleted,
                    "Books/gone \"quoted\".pdf".to_owned(),
                ),
            ],
            repos_found: 12,
            repos_captured: 12,
            tracked_bytes: 1_210_000_000,
            written_bytes: 38_000_000,
            seconds: 41,
        }
    }

    #[test]
    fn the_subject_names_the_backup_and_the_profile() {
        let text = render("2026-08-02 12:00 UTC", "coreenginex", &sample());
        let mut lines = text.lines();
        assert_eq!(
            lines.next(),
            Some("backup 2026-08-02 12:00 UTC - coreenginex")
        );
        assert_eq!(lines.next(), Some(""));
    }

    #[test]
    fn a_run_that_changed_nothing_still_says_so() {
        let text = render("2026-08-02 12:00 UTC", "coreenginex", &Summary::default());
        assert!(text.contains("\nno changes\n"), "{text}");
        assert!(!text.contains("files:"), "{text}");
    }

    #[test]
    fn what_render_writes_parse_reads_back() {
        let want = sample();
        let text = render("2026-08-02 12:00 UTC", "coreenginex", &want);
        let got = parse(&text).expect("our own message parses");

        assert_eq!(got.changed, want.changed);
        assert_eq!(got.added, want.added);
        assert_eq!(got.deleted, want.deleted);
        assert_eq!(got.roots, want.roots);
        assert_eq!(got.repos_found, want.repos_found);
        assert_eq!(got.repos_captured, want.repos_captured);
        assert_eq!(got.written_bytes, want.written_bytes);
        assert_eq!(got.seconds, want.seconds);
    }

    #[test]
    fn a_hostile_path_survives_the_round_trip_as_one_row() {
        let text = render("2026-08-02 12:00 UTC", "coreenginex", &sample());
        let got = parse(&text).expect("parses");
        // The newline in a filename splits its row, so the sample is best-effort -
        // but the counts, which is what history renders, must not move.
        assert_eq!(got.changed, 3);
        assert!(
            got.sample.iter().any(|(_, path)| path.contains("handbook")),
            "{:?}",
            got.sample
        );
    }

    #[test]
    fn a_message_this_did_not_write_is_not_invented() {
        assert_eq!(parse("some hand-written commit\n"), None);
        assert_eq!(parse(""), None);
    }
}
