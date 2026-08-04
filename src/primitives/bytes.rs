//! Layer 0. Byte counts as people read them.
//!
//! Here rather than in `cli::render` because `store::message` writes this into every
//! backup\'s commit message, and layer 4 may not reach up into layer 5. That import
//! was a genuine cycle, and it meant a presentational tweak in the CLI would silently
//! change bytes already committed to history.

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
