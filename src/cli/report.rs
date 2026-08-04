//! Rustc-style structured diagnostics, ported from cex's `cex_util::diag`.
//!
//! [`Diagnostic`] is a value type carrying the fields of a coloured terminal block:
//! severity, title, an optional location, notes, helps, and a sequence of recovery
//! commands. It deliberately implements neither `Display` nor `Error` - rendering
//! happens only in [`Diagnostic::render`].
//!
//! cex carries its diagnostics across an `anyhow::Error` and unwraps them in
//! `main.rs`. Tycho has no `anyhow` and its CLI handlers return [`Exit`] rather than
//! `Result`, so that whole carrier layer - `DiagnosticError`, `render_anyhow`, the
//! `.diag_context()` extension trait - is dropped. [`Diagnostic::emit`] writes the
//! block and hands back the [`Exit`] its severity names, which is what lets a handler
//! read `return report! { .. }`.
//!
//! The four sections answer four different questions, and mixing them is the failure
//! this shape exists to prevent: `-->` says **where**, `note:` says **why**, `help:`
//! says **what to do** in prose, and `recovery:` gives the literal command to type.
//! A `help:` that restates the title is worse than no `help:` at all.

use crate::cli::Exit;
use crate::cli::render::paint;
use std::borrow::Cow;
use std::fmt::Write as _;
use std::path::PathBuf;

const RED_BOLD: &str = "1;31";
const YELLOW_BOLD: &str = "1;33";
const CYAN_BOLD: &str = "1;36";
const GREEN_BOLD: &str = "1;32";
const BOLD: &str = "1";
const BLUE: &str = "34";
const CYAN: &str = "36";
const YELLOW: &str = "33";
const DIM: &str = "2";

/// What kind of block this is, and therefore the prefix, the colour and the exit.
///
/// Two, not cex's four: `Info` needs a `RUST_LOG` gate Tycho does not have, and
/// `Fatal` panics, which no Tycho handler wants. Both map onto an [`Exit`], so the
/// severity and the process's exit code cannot disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    const fn prefix(self) -> (&'static str, &'static str) {
        match self {
            Self::Error => ("error", RED_BOLD),
            Self::Warning => ("warning", YELLOW_BOLD),
        }
    }
}

impl From<Severity> for Exit {
    fn from(severity: Severity) -> Self {
        match severity {
            Severity::Error => Self::Failure,
            Severity::Warning => Self::Warning,
        }
    }
}

/// Where a diagnostic points.
///
/// Tycho's errors have no source-code span; the natural location is a config key, a
/// profile, a remote, a store, or a file. Each variant carries only what its renderer
/// needs, so an unset field cannot be confused with an empty one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Location {
    /// A key path inside the config file (`profile.cex.remotes`).
    ConfigKey(String),
    /// A profile by name.
    Profile(String),
    /// A remote by name.
    Remote(String),
    /// A bare git store, local or on a remote.
    Store(PathBuf),
    /// A file on disk, optionally with a line.
    File { path: PathBuf, line: Option<u32> },
}

#[must_use]
pub fn at_config_key(key: impl AsRef<str>) -> Location {
    Location::ConfigKey(key.as_ref().to_owned())
}

#[must_use]
pub fn at_profile(name: impl AsRef<str>) -> Location {
    Location::Profile(name.as_ref().to_owned())
}

#[must_use]
pub fn at_remote(name: impl AsRef<str>) -> Location {
    Location::Remote(name.as_ref().to_owned())
}

#[must_use]
pub fn at_store(path: impl Into<PathBuf>) -> Location {
    Location::Store(path.into())
}

#[must_use]
pub fn at_file(path: impl Into<PathBuf>) -> Location {
    Location::File {
        path: path.into(),
        line: None,
    }
}

#[must_use]
pub fn at_file_line(path: impl Into<PathBuf>, line: u32) -> Location {
    Location::File {
        path: path.into(),
        line: Some(line),
    }
}

/// One command in the recovery block: what to type, and optionally why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recovery {
    /// Rendered `$ <cmd>` with the `$` dim, the body cyan, and any
    /// angle-bracketed `<placeholder>` yellow.
    pub cmd: Cow<'static, str>,
    /// Rendered on the next line as a dim `# <desc>`, shell-comment style.
    pub desc: Option<Cow<'static, str>>,
}

/// A structured diagnostic: severity and title, plus any number of notes, helps and
/// recovery commands.
///
/// Sections render in a canonical order - location, notes, helps, recoveries -
/// regardless of the order they were added, so two call sites cannot produce two
/// different-looking blocks from the same material.
#[derive(Clone, Debug)]
pub struct Diagnostic {
    severity: Severity,
    title: Cow<'static, str>,
    location: Option<Location>,
    notes: Vec<Cow<'static, str>>,
    helps: Vec<Cow<'static, str>>,
    recoveries: Vec<Recovery>,
}

impl Diagnostic {
    #[must_use]
    pub fn error(title: impl Into<Cow<'static, str>>) -> Self {
        Self::new(Severity::Error, title)
    }

    #[must_use]
    pub fn warning(title: impl Into<Cow<'static, str>>) -> Self {
        Self::new(Severity::Warning, title)
    }

    fn new(severity: Severity, title: impl Into<Cow<'static, str>>) -> Self {
        Self {
            severity,
            title: title.into(),
            location: None,
            notes: Vec::new(),
            helps: Vec::new(),
            recoveries: Vec::new(),
        }
    }

    /// One diagnostic, one location: this replaces any prior one.
    #[must_use]
    pub fn location(mut self, location: Location) -> Self {
        self.location = Some(location);
        self
    }

    /// The *why*. Backtick-wrapped spans render cyan.
    #[must_use]
    pub fn note(mut self, text: impl Into<Cow<'static, str>>) -> Self {
        self.notes.push(text.into());
        self
    }

    /// The *what to do next*, in prose. For a command to copy, use
    /// [`Diagnostic::recovery`] instead.
    #[must_use]
    pub fn help(mut self, text: impl Into<Cow<'static, str>>) -> Self {
        self.helps.push(text.into());
        self
    }

    #[must_use]
    pub fn recovery(mut self, cmd: impl Into<Cow<'static, str>>) -> Self {
        self.recoveries.push(Recovery {
            cmd: cmd.into(),
            desc: None,
        });
        self
    }

    #[must_use]
    pub fn recovery_with(
        mut self,
        cmd: impl Into<Cow<'static, str>>,
        desc: impl Into<Cow<'static, str>>,
    ) -> Self {
        self.recoveries.push(Recovery {
            cmd: cmd.into(),
            desc: Some(desc.into()),
        });
        self
    }

    /// Writes the block to stderr and hands back the [`Exit`] its severity names.
    pub fn emit(self) -> Exit {
        eprint!("{}", self.render());
        self.severity.into()
    }

    /// Renders the block.
    ///
    /// ```text
    /// error: <title>
    ///   --> <location>
    ///   |
    ///   = note: <note>
    ///   |
    ///   help: <help>
    ///   |
    ///   recovery:
    ///     $ <cmd>
    ///         # <desc>
    /// ```
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        let (word, colour) = self.severity.prefix();
        let _ = writeln!(out, "{}: {}", paint(word, colour), paint(&self.title, BOLD));

        let mut emitted = false;

        if let Some(location) = &self.location {
            let _ = writeln!(
                out,
                "  {} {}",
                paint("-->", BLUE),
                render_location(location)
            );
            emitted = true;
        }

        if !self.notes.is_empty() {
            rule(&mut out, emitted);
            for note in &self.notes {
                let mut lines = note.split('\n');
                if let Some(first) = lines.next() {
                    let _ = writeln!(
                        out,
                        "  {} {} {}",
                        paint("=", DIM),
                        paint("note:", DIM),
                        backticked(first)
                    );
                    // Ten columns, so the continuation aligns under the body rather
                    // than under the label.
                    for tail in lines {
                        let _ = writeln!(out, "          {}", backticked(tail));
                    }
                }
            }
            emitted = true;
        }

        if !self.helps.is_empty() {
            rule(&mut out, emitted);
            for help in &self.helps {
                let mut lines = help.split('\n');
                if let Some(first) = lines.next() {
                    let _ = writeln!(out, "  {} {first}", paint("help:", CYAN_BOLD));
                    for tail in lines {
                        let _ = writeln!(out, "        {tail}");
                    }
                }
            }
            emitted = true;
        }

        if !self.recoveries.is_empty() {
            rule(&mut out, emitted);
            let _ = writeln!(out, "  {}", paint("recovery:", GREEN_BOLD));
            for item in &self.recoveries {
                let _ = writeln!(out, "    {} {}", paint("$", DIM), placeheld(&item.cmd));
                if let Some(desc) = &item.desc {
                    let _ = writeln!(out, "        {} {}", paint("#", DIM), paint(desc, DIM));
                }
            }
        }
        out
    }

    #[must_use]
    pub fn severity(&self) -> Severity {
        self.severity
    }

    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    #[must_use]
    pub fn notes(&self) -> &[Cow<'static, str>] {
        &self.notes
    }

    #[must_use]
    pub fn helps(&self) -> &[Cow<'static, str>] {
        &self.helps
    }

    #[must_use]
    pub fn recoveries(&self) -> &[Recovery] {
        &self.recoveries
    }
}

fn rule(out: &mut String, emitted: bool) {
    if emitted {
        let _ = writeln!(out, "  {}", paint("|", DIM));
    }
}

fn render_location(location: &Location) -> String {
    match location {
        Location::ConfigKey(key) => format!("{} {}", paint("config key", DIM), paint(key, CYAN)),
        Location::Profile(name) => format!("{} {}", paint("profile", DIM), paint(name, CYAN)),
        Location::Remote(name) => format!("{} {}", paint("remote", DIM), paint(name, CYAN)),
        Location::Store(path) => format!(
            "{} {}",
            paint("store", DIM),
            paint(&path.display().to_string(), CYAN)
        ),
        Location::File { path, line } => {
            let shown = path.display().to_string();
            match line {
                Some(line) => format!(
                    "{shown}{}{}",
                    paint(":", DIM),
                    paint(&line.to_string(), DIM)
                ),
                None => shown,
            }
        }
    }
}

/// Colours backtick-wrapped spans cyan, so an inline command or key stands out of
/// the prose. An unterminated backtick falls through verbatim rather than swallowing
/// the rest of the line.
fn backticked(text: &str) -> String {
    delimited(text, '`', '`', CYAN, "")
}

/// Colours the command cyan and any `<placeholder>` yellow, which is the cue that
/// the reader has to substitute something before typing it.
fn placeheld(cmd: &str) -> String {
    delimited(cmd, '<', '>', YELLOW, CYAN)
}

/// Walks `text` painting `open..=close` spans one colour and everything between them
/// another. `plain` empty means the surrounding text keeps its own colour.
fn delimited(text: &str, open: char, close: char, inside: &str, plain: &str) -> String {
    let tint = |part: &str| {
        if plain.is_empty() {
            part.to_owned()
        } else {
            paint(part, plain)
        }
    };
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(open) {
        out.push_str(&tint(&rest[..start]));
        let after = &rest[start + open.len_utf8()..];
        let Some(end) = after.find(close) else {
            // Unterminated: the remainder is ordinary text, not an open span.
            out.push_str(&tint(&rest[start..]));
            return out;
        };
        let span_end = start + open.len_utf8() + end + close.len_utf8();
        out.push_str(&paint(&rest[start..span_end], inside));
        rest = &rest[span_end..];
    }
    out.push_str(&tint(rest));
    out
}

/// Builds a [`Diagnostic`], emits it, and evaluates to its [`Exit`].
///
/// The body mirrors the rendered block, which is the point: the source reads in the
/// order the reader sees. cex spells the recovery commands `$ "cmd"` / `# "desc"`;
/// `macro_rules!` cannot match a literal `$`, so the two are joined by `=>` here
/// rather than pulling in a proc-macro crate for the sigil alone.
///
/// ```text
/// return report! {
///     error: "'{name}' is the only remote on profile '{profile}'",
///     at: at_remote(name),
///     note: "a profile with no remote keeps its backup on this machine, which is \
///            the one failure this tool exists to prevent",
///     help: "add the replacement before removing this one",
///     recovery: {
///         "tycho remote add <name> <path>" => "point it at the new destination",
///         "tycho remote rm {name} --local-only" => "or accept local-only backups",
///     },
/// };
/// ```
macro_rules! report {
    ($severity:ident : $title:literal $(, $($rest:tt)*)?) => {{
        let diagnostic = $crate::cli::report::Diagnostic::$severity(format!($title));
        $crate::cli::report::report!(@build diagnostic $(, $($rest)*)?);
        diagnostic.emit()
    }};

    (@build $d:ident $(,)?) => {};

    (@build $d:ident, at: $location:expr $(, $($rest:tt)*)?) => {
        let $d = $d.location($location);
        $crate::cli::report::report!(@build $d $(, $($rest)*)?);
    };
    (@build $d:ident, note: $text:literal $(, $($rest:tt)*)?) => {
        let $d = $d.note(format!($text));
        $crate::cli::report::report!(@build $d $(, $($rest)*)?);
    };
    (@build $d:ident, help: $text:literal $(, $($rest:tt)*)?) => {
        let $d = $d.help(format!($text));
        $crate::cli::report::report!(@build $d $(, $($rest)*)?);
    };
    (@build $d:ident, recovery: { $($cmd:literal $(=> $desc:literal)?),* $(,)? }
        $(, $($rest:tt)*)?) => {
        $( let $d = $crate::cli::report::report!(@one $d, $cmd $(=> $desc)?); )*
        $crate::cli::report::report!(@build $d $(, $($rest)*)?);
    };

    (@one $d:ident, $cmd:literal => $desc:literal) => { $d.recovery_with(format!($cmd), $desc) };
    (@one $d:ident, $cmd:literal) => { $d.recovery(format!($cmd)) };
}

pub(crate) use report;

#[cfg(test)]
mod tests {
    use super::{Diagnostic, Severity, at_profile, at_remote};
    use crate::cli::Exit;

    fn plain(diagnostic: &Diagnostic) -> String {
        strip(&diagnostic.render())
    }

    /// Colour is emphasis on text that already says it, so every assertion below is
    /// on the words rather than on the escapes.
    fn strip(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for skip in chars.by_ref() {
                    if skip.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn a_title_alone_is_one_line() {
        assert_eq!(
            plain(&Diagnostic::error("stores do not match")),
            "error: stores do not match\n"
        );
    }

    #[test]
    fn the_severity_alone_decides_the_exit_code() {
        assert_eq!(Exit::from(Severity::Error), Exit::Failure);
        assert_eq!(Exit::from(Severity::Warning), Exit::Warning);
    }

    /// The separators appear *between* sections and never above the first one, which
    /// is what keeps a title-plus-one-note block from opening with a bare rule.
    #[test]
    fn rules_separate_sections_and_never_lead() {
        let one = Diagnostic::error("t").note("n");
        assert_eq!(plain(&one), "error: t\n  = note: n\n");

        let two = Diagnostic::error("t").location(at_profile("cex")).note("n");
        assert_eq!(
            plain(&two),
            "error: t\n  --> profile cex\n  |\n  = note: n\n"
        );
    }

    #[test]
    fn sections_render_in_canonical_order_whatever_the_builder_order() {
        let built = Diagnostic::error("t")
            .recovery("cmd")
            .help("h")
            .note("n")
            .location(at_remote("ghost-r"));
        assert_eq!(
            plain(&built),
            "error: t\n  --> remote ghost-r\n  |\n  = note: n\n  |\n  help: h\n  \
             |\n  recovery:\n    $ cmd\n"
        );
    }

    #[test]
    fn a_recovery_descriptor_renders_as_a_shell_comment() {
        let built = Diagnostic::error("t").recovery_with("tycho run", "take a backup now");
        assert_eq!(
            plain(&built),
            "error: t\n  recovery:\n    $ tycho run\n        # take a backup now\n"
        );
    }

    /// A note's continuation aligns under the body, not under the `= note:` label.
    #[test]
    fn a_multi_line_note_aligns_its_continuation() {
        let built = Diagnostic::error("t").note("first\nsecond");
        assert_eq!(
            plain(&built),
            "error: t\n  = note: first\n          second\n"
        );
    }

    /// The colouring is a span walk over user text, so the failure that matters is
    /// losing characters, not losing colour.
    #[test]
    fn span_colouring_never_drops_text() {
        let built = Diagnostic::error("t")
            .note("run `tycho run` and an unclosed ` stays put")
            .recovery("tycho remote add <name> <path>");
        let rendered = plain(&built);
        assert!(
            rendered.contains("run `tycho run` and an unclosed ` stays put"),
            "{rendered}"
        );
        assert!(
            rendered.contains("$ tycho remote add <name> <path>"),
            "{rendered}"
        );
    }

    /// A handler reads `return report! { .. }`, so the macro has to be an expression
    /// of type `Exit` rather than a statement that happens to print.
    #[test]
    fn the_macro_is_an_expression_yielding_the_exit() {
        let name = "ghost-r";
        let exit = report! {
            error: "'{name}' is the only remote on this profile",
            at: at_remote(name),
            note: "a profile with no remote keeps its backup on this machine",
            help: "add the replacement before removing this one",
            recovery: {
                "tycho remote add <name> <path>" => "point it at the new destination",
                "tycho remote rm {name} --local-only",
            },
        };
        assert_eq!(exit, Exit::Failure);

        let warned = report! { warning: "nothing to do" };
        assert_eq!(warned, Exit::Warning);
    }
}
