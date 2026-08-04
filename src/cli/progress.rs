//! Layer 5's answer to a run that prints nothing until it finishes.
//!
//! `store::run::execute` cannot print for itself - layer 4 may not depend on layer 5,
//! `lib.rs`'s layer table - so it hands each [`Step`] to a callback instead. This is
//! the only place that turns one into text, and the only place that decides whether
//! to print it at all.

use crate::cli::render::{self, CYAN, DIM, paint};
use crate::store::run::Step;

/// The label column, matching `bootstrap::row`'s `{label:<14}  {value}` shape.
const LABEL: usize = 14;
/// Nests a phase line under the header the way `run_result`'s own rows nest under its
/// commit line.
const INDENT: usize = 2;

/// One line, redrawn over itself for as long as a run is in progress.
pub struct Progress {
    active: bool,
}

impl Default for Progress {
    fn default() -> Self {
        Self::new()
    }
}

impl Progress {
    /// Silent unless stdout is a terminal - asked once here through
    /// [`render::stdout_is_terminal`] rather than a second, possibly disagreeing,
    /// answer to the question `render::decide_colour` already answered for colour.
    #[must_use]
    pub fn new() -> Self {
        Self {
            active: render::stdout_is_terminal(),
        }
    }

    /// Redraws the line for `step`. A no-op off a terminal.
    pub fn report(&mut self, profile: &str, step: &Step) {
        self.report_to(&mut std::io::stdout(), profile, step);
    }

    /// Erases the line, so whatever prints next - `render::run_result`, or a
    /// diagnostic on the failure path - starts at column zero with nothing behind it.
    pub fn clear(&mut self) {
        self.clear_to(&mut std::io::stdout());
    }

    fn report_to(&mut self, out: &mut impl std::io::Write, profile: &str, step: &Step) {
        if !self.active {
            return;
        }
        let (_, painted) = line(profile, step);
        let _ = write!(out, "\r{ERASE}{painted}");
        let _ = out.flush();
    }

    fn clear_to(&mut self, out: &mut impl std::io::Write) {
        if !self.active {
            return;
        }
        let _ = write!(out, "\r{ERASE}");
        let _ = out.flush();
    }
}

/// Erase from the cursor to the end of the line.
///
/// **Padding to a fixed width cannot do this job.** It has to guess how wide the last
/// frame was, and it is wrong in both directions: a frame longer than the pad leaves
/// the previous frame's tail on screen, and a terminal narrower than the pad wraps, so
/// the `\r` returns to the start of the second visual row and the first survives. The
/// terminal knows the answer to both; this asks it.
const ERASE: &str = "\x1b[K";

/// A phase's row: the label field, dim, then its value - or nothing after it, for a
/// phase that has none. Returns the visible width alongside the text to print, because
/// that width is what the caller pads the redraw to, and it has to be measured on the
/// plain half - colour must never change it, the same property `render`'s own column
/// tests hold every table to.
fn row(label: &str, value: Option<(String, String)>) -> (usize, String) {
    let field = format!("{label:<LABEL$}");
    let painted_label = paint(&field, DIM);
    match value {
        None => (
            INDENT + field.chars().count(),
            format!("{:INDENT$}{painted_label}", ""),
        ),
        Some((plain, painted)) => (
            INDENT + field.chars().count() + 2 + plain.chars().count(),
            format!("{:INDENT$}{painted_label}  {painted}", ""),
        ),
    }
}

/// The line for one [`Step`], as (visible width, text to print).
fn line(profile: &str, step: &Step) -> (usize, String) {
    match step {
        Step::Planning => {
            let text = format!("{profile}  planning");
            (text.chars().count(), text)
        }
        Step::Hashing { files } => {
            let value = render::plural(*files, "file");
            row("hashing", Some((value.clone(), value)))
        }
        // Once the last repository lands, `done == total` and saying "19 of 19" would
        // be saying the total twice; every earlier redraw still names which one.
        Step::Capturing { repo, done, total } => {
            let value = if done >= total {
                let text = render::plural(*total, "repository");
                (text.clone(), text)
            } else {
                let head = format!(
                    "{} of {}",
                    render::count(*done),
                    render::plural(*total, "repository")
                );
                // Clamped to what is left of the row rather than to a constant: a
                // wrapped line puts the tail on a second visual row, and `\r` returns
                // to the start of that one, so the first row is never erased.
                let room = render::WIDTH
                    .saturating_sub(INDENT + LABEL + 2 + head.chars().count() + 2)
                    .max(8);
                let shown = render::fit(repo, room);
                (
                    format!("{head}  {shown}"),
                    format!("{head}  {}", paint(&shown, CYAN)),
                )
            };
            row("capturing", Some(value))
        }
        Step::Publishing => row("publishing", None),
        // The size is the store's, not this push's, so it says `store` rather than
        // anything that would read as a transfer estimate. It is here because a push
        // with no detail at all is the phase that reads as a hang, and knowing the
        // order of magnitude is most of the cure.
        Step::Pushing {
            remote,
            store_bytes,
        } => {
            let size = format!("store {}", render::size(*store_bytes));
            let room = render::WIDTH
                .saturating_sub(INDENT + LABEL + 2 + size.chars().count() + 2)
                .max(8);
            let shown = render::fit(remote, room);
            row(
                "pushing",
                Some((
                    format!("{shown}  {size}"),
                    format!("{}  {}", paint(&shown, CYAN), paint(&size, DIM)),
                )),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Progress, line};
    use crate::cli::render::set_colour_for_test;
    use crate::store::run::Step;

    fn rendered(profile: &str, step: &Step) -> String {
        line(profile, step).1
    }

    #[test]
    fn planning_names_the_profile() {
        set_colour_for_test(false);
        assert_eq!(rendered("cex", &Step::Planning), "cex  planning");
    }

    #[test]
    fn hashing_counts_with_thousands_separators() {
        set_colour_for_test(false);
        let text = rendered("cex", &Step::Hashing { files: 4_812 });
        assert_eq!(text, format!("  {:<14}  4,812 files", "hashing"));
    }

    #[test]
    fn capturing_names_the_current_repository_until_the_last_one_lands() {
        set_colour_for_test(false);
        let mid = rendered(
            "cex",
            &Step::Capturing {
                repo: "org/frontend".to_owned(),
                done: 3,
                total: 19,
            },
        );
        assert_eq!(
            mid,
            format!("  {:<14}  3 of 19 repositories  org/frontend", "capturing")
        );

        let done = rendered(
            "cex",
            &Step::Capturing {
                repo: "org/frontend".to_owned(),
                done: 19,
                total: 19,
            },
        );
        assert_eq!(
            done,
            format!("  {:<14}  19 repositories", "capturing"),
            "the last repository does not repeat the total as 19 of 19"
        );
    }

    #[test]
    fn publishing_has_no_value_column() {
        set_colour_for_test(false);
        assert_eq!(
            rendered("cex", &Step::Publishing),
            format!("  {:<14}", "publishing")
        );
    }

    /// The size says `store` and not anything resembling "to send". Git transfers
    /// only the objects the remote lacks, so on every run after the first the store's
    /// size and this push's traffic are nothing alike, and a number presented as the
    /// latter would be wrong on almost every run.
    #[test]
    fn pushing_names_the_remote_and_the_store_size() {
        set_colour_for_test(false);
        let text = rendered(
            "cex",
            &Step::Pushing {
                remote: "ghost-r".to_owned(),
                store_bytes: 292_000_000,
            },
        );
        assert_eq!(text, format!("  {:<14}  ghost-r  store 292 MB", "pushing"));
        assert!(!text.contains("send"), "{text}");
    }

    /// The width returned alongside the text is the plain one, so colour can never
    /// change how much of the redraw a shorter next line has to erase.
    /// A line wider than the terminal wraps, and `\r` then returns to the start of the
    /// **second** visual row - so the first is never erased and the redraw leaves a
    /// trail. Erase-to-end-of-line cannot undo that; only not wrapping can.
    #[test]
    fn no_line_can_be_wide_enough_to_wrap() {
        set_colour_for_test(false);
        let worst = "CoreEngineX/products/photoflick/photoflick-android/deeply/nested/thing";
        for step in [
            Step::Planning,
            Step::Hashing { files: 8_675_309 },
            Step::Capturing {
                repo: worst.to_owned(),
                done: 999,
                total: 1_000,
            },
            Step::Capturing {
                repo: worst.to_owned(),
                done: 1_000,
                total: 1_000,
            },
            Step::Publishing,
            Step::Pushing {
                remote: worst.to_owned(),
                store_bytes: 292_000_000,
            },
        ] {
            let (width, text) = line("a-rather-long-profile-name", &step);
            assert!(
                width <= crate::cli::render::WIDTH,
                "{step:?} renders {width} wide, past {}: {text:?}",
                crate::cli::render::WIDTH
            );
        }
    }

    #[test]
    fn colour_never_changes_the_reported_width() {
        set_colour_for_test(false);
        let (plain_width, plain_text) = line(
            "cex",
            &Step::Pushing {
                remote: "ghost-r".to_owned(),
                store_bytes: 292_000_000,
            },
        );
        set_colour_for_test(true);
        let (painted_width, painted_text) = line(
            "cex",
            &Step::Pushing {
                remote: "ghost-r".to_owned(),
                store_bytes: 292_000_000,
            },
        );
        set_colour_for_test(false);

        assert_eq!(plain_width, painted_width);
        assert_eq!(plain_width, plain_text.chars().count());
        assert!(
            painted_text.contains("\x1b["),
            "expected an escape: {painted_text:?}"
        );
    }

    #[test]
    fn a_redraw_starts_with_a_carriage_return_and_is_flushed() {
        set_colour_for_test(false);
        let mut progress = Progress { active: true };
        let mut buf = Vec::new();
        progress.report_to(&mut buf, "cex", &Step::Planning);
        let text = String::from_utf8(buf).expect("ascii output");
        assert!(text.starts_with('\r'), "{text:?}");
        assert!(text.contains("cex  planning"), "{text:?}");
    }

    /// Off a terminal, `report` and `clear` must write nothing at all - a launchd log
    /// or a pipe gets none of this.
    #[test]
    fn nothing_is_written_when_not_a_terminal() {
        let mut progress = Progress { active: false };
        let mut buf = Vec::new();
        progress.report_to(&mut buf, "cex", &Step::Planning);
        progress.clear_to(&mut buf);
        assert!(buf.is_empty(), "{buf:?}");
    }
}
