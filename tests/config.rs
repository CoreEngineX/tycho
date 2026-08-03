//! `config.md` section 8's validation table, one test per row that is decidable
//! without a filesystem.

use std::path::Path;
use tycho::config::{
    Config, ConfigError, DiagnosticKind, Parsed, Schedule, Severity, TimeOfDay, Weekday,
    parse_interval, parse_with,
};

fn env(name: &str) -> Option<String> {
    match name {
        "HOME" => Some("/h".to_owned()),
        "USER" => Some("tester".to_owned()),
        _ => None,
    }
}

fn parse(text: &str) -> Parsed {
    parse_with(text, Some(Path::new("/h")), env).expect("the text is TOML")
}

/// A minimal valid profile, so each test can add exactly the one thing it is about.
fn valid(extra: &str) -> String {
    format!(
        "version = 1\n\n[[profile]]\nname = \"work\"\nwatch = [\"~/A\"]\nlocal_only = true\n{extra}"
    )
}

fn kinds(parsed: &Parsed) -> Vec<DiagnosticKind> {
    parsed
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.kind.clone())
        .collect()
}

fn errors(parsed: &Parsed) -> Vec<DiagnosticKind> {
    parsed
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .map(|diagnostic| diagnostic.kind.clone())
        .collect()
}

fn assert_has(parsed: &Parsed, wanted: &DiagnosticKind) {
    assert!(
        kinds(parsed).contains(wanted),
        "expected {wanted:?}, got {:#?}",
        kinds(parsed)
    );
}

#[test]
fn the_documented_example_parses_without_errors() {
    let text = r#"
version = 1

[settings]
log_level = "info"

[[profile]]
name = "coreenginex"

watch = [
  "~/Developer/CoreEngineX",
  "~/Books",
]

ignore = [
  "~/Developer/CoreEngineX/scratch",
  "**/*.xcarchive",
]

reinclude = [
  "~/Developer/CoreEngineX/scratch/keep",
]

remotes = [
  { name = "gdrive",   path = "~/CloudStorage/GoogleDrive/CoreEngineX-Backups" },
  { name = "onedrive", path = "~/CloudStorage/OneDrive/CoreEngineX-Backups" },
  { name = "t7",       path = "/Volumes/T7/tycho", optional = true, behind_tolerance = 4 },
]

schedule = { weekly = { day = "sunday", at = "12:00" } }
"#;
    let parsed = parse(text);
    assert!(
        !parsed.has_errors(),
        "the documented example should be clean: {:#?}",
        errors(&parsed)
    );

    let profile = &parsed.config.profiles[0];
    assert_eq!(profile.name.as_str(), "coreenginex");
    // The counts `config check` echoes back: 2 roots, 2 ignores, 1 reinclude,
    // 3 remotes, weekly Sun 12:00.
    assert_eq!(profile.watch.len(), 2);
    assert_eq!(profile.ignore_paths.len() + profile.ignore_globs.len(), 2);
    assert_eq!(profile.reinclude.len(), 1);
    assert_eq!(profile.remotes.len(), 3);
    assert_eq!(
        profile.schedule,
        Some(Schedule::Weekly {
            day: Weekday::Sunday,
            at: TimeOfDay {
                hour: 12,
                minute: 0
            }
        })
    );
    assert!(profile.use_default_ignores, "the default is on");
    assert!(profile.remotes[2].optional);
    assert_eq!(profile.remotes[0].behind_tolerance, 4);
}

#[test]
fn every_unknown_key_is_reported_rather_than_the_first() {
    let parsed = parse(&valid("wacth = [\"~/B\"]\nremotez = []\n"));
    for key in ["wacth", "remotez"] {
        assert_has(
            &parsed,
            &DiagnosticKind::UnknownKey {
                table: "[[profile]]".to_owned(),
                key: key.to_owned(),
            },
        );
    }
}

#[test]
fn a_newer_version_says_so_rather_than_naming_a_key() {
    let error = parse_with("version = 99\n", Some(Path::new("/h")), env)
        .expect_err("a newer config is refused");
    let ConfigError::Version { found, understood } = error else {
        panic!("expected a version error, got {error}");
    };
    assert_eq!(found, 99);
    assert_eq!(understood, 1);
}

#[test]
fn profile_names_are_checked_against_the_documented_charset() {
    for (name, ok) in [("work", true), ("Work", false), ("catchup", false)] {
        let text = format!(
            "version = 1\n[[profile]]\nname = \"{name}\"\nwatch = [\"~/A\"]\nlocal_only = true\n"
        );
        let parsed = parse(&text);
        assert_eq!(!parsed.has_errors(), ok, "{name}: {:#?}", errors(&parsed));
    }
}

#[test]
fn duplicate_names_are_errors_on_both_profiles_and_remotes() {
    let text = "version = 1
[[profile]]
name = \"work\"
watch = [\"~/A\"]
local_only = true
[[profile]]
name = \"work\"
watch = [\"~/B\"]
local_only = true
";
    assert_has(
        &parse(text),
        &DiagnosticKind::DuplicateProfile {
            name: "work".to_owned(),
        },
    );

    let parsed = parse(&valid(
        "remotes = [{ name = \"gd\", path = \"/r/one\" }, { name = \"gd\", path = \"/r/two\" }]\n",
    ));
    assert_has(
        &parsed,
        &DiagnosticKind::DuplicateRemote {
            name: "gd".to_owned(),
        },
    );
}

#[test]
fn a_profile_with_no_watch_and_one_with_no_remotes_are_both_errors() {
    let text = "version = 1\n[[profile]]\nname = \"work\"\nwatch = []\nlocal_only = true\n";
    assert_has(&parse(text), &DiagnosticKind::EmptyWatch);

    let text = "version = 1\n[[profile]]\nname = \"work\"\nwatch = [\"~/A\"]\n";
    assert_has(&parse(text), &DiagnosticKind::NoRemotes);
}

#[test]
fn every_way_a_path_can_be_unusable_is_an_error() {
    for bad in ["A/relative", "~/A/../B", "$WORKSPACE/code"] {
        let text = format!(
            "version = 1\n[[profile]]\nname = \"work\"\nwatch = [\"{bad}\"]\nlocal_only = true\n"
        );
        let parsed = parse(&text);
        assert!(
            errors(&parsed)
                .iter()
                .any(|kind| matches!(kind, DiagnosticKind::BadPath { .. })),
            "{bad} should be a bad path: {:#?}",
            errors(&parsed)
        );
    }
}

#[test]
fn an_alias_collision_is_an_error_that_suggests_the_named_form() {
    let parsed =
        parse(&valid("").replace("watch = [\"~/A\"]", "watch = [\"~/w/docs\", \"~/p/docs\"]"));
    assert_has(
        &parsed,
        &DiagnosticKind::AliasCollision {
            alias: "docs".to_owned(),
            first: "/h/w/docs".to_owned(),
            second: "/h/p/docs".to_owned(),
        },
    );
    let hint = parsed
        .diagnostics
        .iter()
        .find(|diagnostic| matches!(diagnostic.kind, DiagnosticKind::AliasCollision { .. }))
        .and_then(tycho::config::Diagnostic::hint)
        .expect("a collision carries a hint");
    assert!(hint.contains("name = "), "{hint}");
}

#[test]
fn naming_the_roots_resolves_the_collision() {
    let parsed = parse(&valid("").replace(
        "watch = [\"~/A\"]",
        "watch = [{ path = \"~/w/docs\", name = \"work-docs\" }, { path = \"~/p/docs\", name = \"personal-docs\" }]",
    ));
    assert!(!parsed.has_errors(), "{:#?}", errors(&parsed));
}

#[test]
fn a_reserved_or_unusable_alias_is_an_error() {
    for (name, _) in [(".tycho", 0), (".hidden", 0), ("a/b", 0)] {
        let parsed = parse(&valid("").replace(
            "watch = [\"~/A\"]",
            &format!("watch = [{{ path = \"~/A\", name = \"{name}\" }}]"),
        ));
        assert!(
            errors(&parsed)
                .iter()
                .any(|kind| matches!(kind, DiagnosticKind::InvalidAlias { .. })),
            "{name} should be refused: {:#?}",
            errors(&parsed)
        );
    }
}

#[test]
fn a_watched_root_inside_another_is_an_error() {
    let parsed = parse(&valid("").replace("watch = [\"~/A\"]", "watch = [\"~/A\", \"~/A/inner\"]"));
    assert_has(
        &parsed,
        &DiagnosticKind::NestedWatchedRoot {
            inner: "/h/A/inner".to_owned(),
            outer: "/h/A".to_owned(),
        },
    );
}

#[test]
fn a_path_that_is_both_ignored_and_reincluded_is_an_error() {
    let parsed = parse(&valid("ignore = [\"~/A/s\"]\nreinclude = [\"~/A/s\"]\n"));
    assert_has(
        &parsed,
        &DiagnosticKind::ConflictingRules {
            path: "/h/A/s".to_owned(),
        },
    );
}

#[test]
fn the_store_or_a_remote_inside_a_watched_root_is_an_error() {
    let parsed = parse(&valid("store_path = \"~/A/stores\"\n"));
    assert!(
        errors(&parsed)
            .iter()
            .any(|kind| matches!(kind, DiagnosticKind::PathInsideWatchedRoot { .. })),
        "{:#?}",
        errors(&parsed)
    );

    let parsed = parse(&valid(
        "remotes = [{ name = \"r\", path = \"~/A/backups\" }]\n",
    ));
    assert!(
        errors(&parsed)
            .iter()
            .any(|kind| matches!(kind, DiagnosticKind::PathInsideWatchedRoot { .. })),
        "{:#?}",
        errors(&parsed)
    );
}

#[test]
fn two_profiles_sharing_a_store_path_is_an_error() {
    let text = "version = 1
[[profile]]
name = \"one\"
watch = [\"~/A\"]
local_only = true
store_path = \"/s\"
[[profile]]
name = \"two\"
watch = [\"~/B\"]
local_only = true
store_path = \"/s\"
";
    assert!(
        errors(&parse(text))
            .iter()
            .any(|kind| matches!(kind, DiagnosticKind::DuplicateStorePath { .. })),
    );
}

#[test]
fn an_invalid_glob_is_an_error() {
    let parsed = parse(&valid("ignore = [\"[\"]\n"));
    assert!(
        errors(&parsed)
            .iter()
            .any(|kind| matches!(kind, DiagnosticKind::InvalidGlob { .. })),
        "{:#?}",
        errors(&parsed)
    );
}

#[test]
fn schedules_are_exactly_one_of_the_three() {
    let parsed = parse(&valid("schedule = { daily = { at = \"18:00\" } }\n"));
    assert_eq!(
        parsed.config.profiles[0].schedule,
        Some(Schedule::Daily {
            at: TimeOfDay {
                hour: 18,
                minute: 0
            }
        })
    );

    let parsed = parse(&valid("schedule = { every = \"6h\" }\n"));
    assert_eq!(
        parsed.config.profiles[0].schedule,
        Some(Schedule::Every(std::time::Duration::from_secs(21_600)))
    );

    // Two keys in one schedule table is structural, so serde refuses the document.
    let text = valid(
        "schedule = { daily = { at = \"18:00\" }, weekly = { day = \"sunday\", at = \"12:00\" } }\n",
    );
    assert!(
        parse_with(&text, Some(Path::new("/h")), env).is_err(),
        "a schedule with two keys must not parse"
    );
}

#[test]
fn a_bad_time_or_day_is_an_error_not_a_default() {
    for bad in ["25:00", "12:99", "noon"] {
        let parsed = parse(&valid(&format!(
            "schedule = {{ daily = {{ at = \"{bad}\" }} }}\n"
        )));
        assert!(
            errors(&parsed)
                .iter()
                .any(|kind| matches!(kind, DiagnosticKind::InvalidSchedule { .. })),
            "{bad} should be refused"
        );
    }
    let parsed = parse(&valid(
        "schedule = { weekly = { day = \"someday\", at = \"12:00\" } }\n",
    ));
    assert!(
        errors(&parsed)
            .iter()
            .any(|kind| matches!(kind, DiagnosticKind::InvalidSchedule { .. }))
    );
}

#[test]
fn intervals_take_a_whole_number_and_one_unit() {
    assert_eq!(
        parse_interval("6h").expect("valid"),
        std::time::Duration::from_secs(21_600)
    );
    assert_eq!(
        parse_interval("30m").expect("valid"),
        std::time::Duration::from_secs(1_800)
    );
    assert_eq!(
        parse_interval("2d").expect("valid"),
        std::time::Duration::from_secs(172_800)
    );
    for bad in ["6", "h", "0h", "-1h", "6y", "six h"] {
        assert!(parse_interval(bad).is_err(), "{bad} should be refused");
    }
}

#[test]
fn warnings_are_warnings_and_do_not_make_the_config_invalid() {
    // No schedule, and an ignore that can never fire.
    let parsed = parse(&valid("ignore = [\"/elsewhere\"]\n"));
    assert!(!parsed.has_errors(), "{:#?}", errors(&parsed));
    assert_has(&parsed, &DiagnosticKind::NoSchedule);
    assert_has(
        &parsed,
        &DiagnosticKind::IgnoreOutsideEveryRoot {
            path: "/elsewhere".to_owned(),
        },
    );
}

#[test]
fn a_config_with_several_problems_reports_all_of_them_at_once() {
    let text = "version = 1
[[profile]]
name = \"work\"
wacth = [\"~/typo\"]
watch = [\"~/w/docs\", \"~/p/docs\"]
";
    let parsed = parse(text);
    let found = errors(&parsed);
    assert!(
        found
            .iter()
            .any(|kind| matches!(kind, DiagnosticKind::UnknownKey { .. })),
        "{found:#?}"
    );
    assert!(
        found
            .iter()
            .any(|kind| matches!(kind, DiagnosticKind::AliasCollision { .. })),
        "{found:#?}"
    );
    assert!(
        found.iter().any(|kind| *kind == DiagnosticKind::NoRemotes),
        "{found:#?}"
    );
    assert!(found.len() >= 3, "expected several errors, got {found:#?}");
}

#[test]
fn a_default_config_is_the_empty_one() {
    assert_eq!(Config::default().profiles.len(), 0);
}
