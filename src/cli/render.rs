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
