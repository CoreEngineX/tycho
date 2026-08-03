//! Reading what `--at` means.
//!
//! Absolute or relative, and **interpreted in local time unless it carries an
//! explicit offset**. That default is the whole point: somebody typing
//! `--at "2026-10-19 12:00"` means the clock on their wall, and silently reading it
//! as UTC would hand them the wrong backup by three or four hours without saying so.

use jiff::{Timestamp, Zoned, tz::TimeZone};

#[derive(Debug, thiserror::Error)]
pub enum WhenError {
    #[error("'{0}' is not a time; try '2026-10-19 12:00', '2026-10-19', or '3 days ago'")]
    Unreadable(String),
    #[error(transparent)]
    Jiff(#[from] jiff::Error),
}

/// Resolves `--at` against a reference point.
///
/// `now` is a parameter rather than read inside, so a relative expression is testable
/// without waiting for the clock.
///
/// # Errors
///
/// If the text is not a time in any accepted form.
pub fn parse(text: &str, now: &Zoned) -> Result<Timestamp, WhenError> {
    let trimmed = text.trim();
    let unreadable = || WhenError::Unreadable(trimmed.to_owned());

    // An explicit offset or `Z` means the person said which zone they meant.
    if let Ok(stamp) = trimmed.parse::<Timestamp>() {
        return Ok(stamp);
    }

    if let Some(relative) = relative(trimmed, now)? {
        return Ok(relative);
    }

    // Everything else is a wall clock, in the reader's own zone. A bare date is tried
    // first because jiff reads one as midnight, and `--at 2026-10-19` meaning
    // 00:00 would select the backup from the *previous* day under an at-or-before
    // rule - the one case where being wrong is silent rather than an error.
    let tz = now.time_zone().clone();
    if !trimmed.contains(':')
        && let Ok(date) = trimmed.parse::<jiff::civil::Date>()
    {
        return Ok(date.at(23, 59, 59, 0).to_zoned(tz)?.timestamp());
    }
    if let Ok(civil) = trimmed.replace(' ', "T").parse::<jiff::civil::DateTime>() {
        return Ok(civil.to_zoned(tz)?.timestamp());
    }
    Err(unreadable())
}

/// `3 days ago`, `2 weeks ago`, `yesterday`, `today`.
fn relative(text: &str, now: &Zoned) -> Result<Option<Timestamp>, WhenError> {
    let lower = text.to_ascii_lowercase();
    match lower.as_str() {
        "now" => return Ok(Some(now.timestamp())),
        "today" => return Ok(Some(now.timestamp())),
        "yesterday" => {
            return Ok(Some(
                now.checked_sub(jiff::Span::new().days(1))?.timestamp(),
            ));
        }
        _ => {}
    }

    let mut words = lower.split_whitespace();
    let (Some(count), Some(unit), Some("ago"), None) =
        (words.next(), words.next(), words.next(), words.next())
    else {
        return Ok(None);
    };
    let Ok(count) = count.parse::<i64>() else {
        return Ok(None);
    };

    let span = jiff::Span::new();
    let span = match unit.trim_end_matches('s') {
        "second" | "sec" => span.seconds(count),
        "minute" | "min" => span.minutes(count),
        "hour" => span.hours(count),
        "day" => span.days(count),
        "week" => span.weeks(count),
        "month" => span.months(count),
        "year" => span.years(count),
        _ => return Ok(None),
    };
    Ok(Some(now.checked_sub(span)?.timestamp()))
}

/// What restore echoes before extracting anything, in **both** zones - so a person
/// reading it in one and a log read in the other agree about which backup this was.
#[must_use]
pub fn both_zones(stamp: &Timestamp) -> String {
    let local = stamp.to_zoned(TimeZone::system());
    let utc = stamp.to_zoned(TimeZone::UTC);
    format!(
        "{}  ({} UTC)",
        local.strftime("%F %H:%M %z"),
        utc.strftime("%F %H:%M")
    )
}

#[cfg(test)]
mod tests {
    use super::{WhenError, both_zones, parse};
    use jiff::{Zoned, tz::TimeZone};

    fn now() -> Zoned {
        "2026-11-08T14:30:00"
            .parse::<jiff::civil::DateTime>()
            .expect("a civil datetime")
            .to_zoned(TimeZone::get("America/Toronto").expect("a zone"))
            .expect("no gap")
    }

    fn at(text: &str) -> String {
        parse(text, &now())
            .expect(text)
            .to_zoned(now().time_zone().clone())
            .strftime("%F %H:%M")
            .to_string()
    }

    /// The default that matters. Toronto is -0400 that day, so reading this as UTC
    /// would select a backup four hours off without saying anything.
    #[test]
    fn a_bare_wall_clock_is_local_time() {
        assert_eq!(at("2026-10-19 12:00"), "2026-10-19 12:00");

        let utc = parse("2026-10-19T12:00:00Z", &now()).expect("an offset was given");
        assert_eq!(
            utc.to_zoned(now().time_zone().clone())
                .strftime("%F %H:%M")
                .to_string(),
            "2026-10-19 09:00",
            "an explicit offset is honoured rather than reinterpreted"
        );
    }

    /// `at or before` on a bare date must mean that day, not the instant it began.
    #[test]
    fn a_bare_date_means_the_end_of_that_day() {
        assert_eq!(at("2026-10-19"), "2026-10-19 23:59");
    }

    #[test]
    fn relative_expressions_count_back_from_now() {
        assert_eq!(at("3 days ago"), "2026-11-05 14:30");
        assert_eq!(at("2 weeks ago"), "2026-10-25 14:30");
        assert_eq!(at("6 hours ago"), "2026-11-08 08:30");
        assert_eq!(at("1 month ago"), "2026-10-08 14:30");
        assert_eq!(at("yesterday"), "2026-11-07 14:30");
    }

    /// Singular and plural both, because people type both.
    #[test]
    fn the_unit_may_be_singular_or_plural() {
        assert_eq!(at("1 day ago"), at("1 days ago"));
    }

    #[test]
    fn nonsense_says_what_it_would_have_accepted() {
        let error = parse("last tuesdayish", &now()).expect_err("not a time");
        assert!(matches!(error, WhenError::Unreadable(_)));
        assert!(error.to_string().contains("3 days ago"), "{error}");
    }

    /// The echo carries both zones, so the person reading it and a log read elsewhere
    /// agree about which backup this was.
    #[test]
    fn the_echo_names_both_zones() {
        let stamp = parse("2026-10-19T15:00:00Z", &now()).expect("a time");
        let text = both_zones(&stamp);
        assert!(text.contains("2026-10-19"), "{text}");
        assert!(text.ends_with("UTC)"), "{text}");
    }
}
