//! Layer 5. In-place config rewriting for the `watch`, `ignore` and `reinclude`
//! commands. Separate from `config` so no filesystem write sits inside the pure core.
//!
//! **The file's most valuable lines are the explanations**: why a path is excluded,
//! why a remote is optional, what a schedule is for. Reserialising through `toml`
//! would lose every comment, blank line and hand-chosen ordering the first time
//! somebody used a command instead of an editor - which is the whole reason
//! `config.md` chose TOML over JSON and `toml_edit` to write it.
//!
//! The file remains something you can own and hand-edit; these commands are a
//! convenience, not the interface.

use std::path::{Path, PathBuf};
use toml_edit::{Array, DocumentMut, Item, Value};

#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is not readable as TOML: {source}")]
    Malformed {
        path: String,
        #[source]
        source: toml_edit::TomlError,
    },
    #[error("no profile named '{0}' in this config")]
    NoProfile(String),
    #[error("the config has no profiles, so there is nothing to add a rule to")]
    NoProfiles,
    #[error("name a profile with -p: {0}")]
    Ambiguous(String),
    #[error("'{value}' is not in {list}")]
    NotPresent { value: String, list: String },
}

/// Which list a rule goes in. A sum type rather than a string, because these are the
/// only three keys and a typo would silently create a fourth that nothing reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum List {
    Watch,
    Ignore,
    Reinclude,
}

impl List {
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Watch => "watch",
            Self::Ignore => "ignore",
            Self::Reinclude => "reinclude",
        }
    }
}

/// A config file open for editing.
#[derive(Debug)]
pub struct Editing {
    document: DocumentMut,
    path: PathBuf,
}

impl Editing {
    /// # Errors
    ///
    /// If the file cannot be read or is not TOML.
    pub fn open(path: &Path) -> Result<Self, EditError> {
        let text = std::fs::read_to_string(path).map_err(|source| EditError::Io {
            context: format!("reading {}", path.display()),
            source,
        })?;
        let document = text
            .parse::<DocumentMut>()
            .map_err(|source| EditError::Malformed {
                path: path.display().to_string(),
                source,
            })?;
        Ok(Self {
            document,
            path: path.to_path_buf(),
        })
    }

    /// Writes the document back, comments and all.
    ///
    /// # Errors
    ///
    /// If the file cannot be written.
    pub fn save(&self) -> Result<(), EditError> {
        crate::sys::fs::write_atomic(&self.path, self.document.to_string().as_bytes()).map_err(
            |source| EditError::Io {
                context: format!("writing {}", self.path.display()),
                source,
            },
        )
    }

    #[must_use]
    pub fn text(&self) -> String {
        self.document.to_string()
    }

    /// Names every profile in the file, in file order.
    #[must_use]
    pub fn profiles(&self) -> Vec<String> {
        self.document
            .get("profile")
            .and_then(Item::as_array_of_tables)
            .map(|tables| {
                tables
                    .iter()
                    .filter_map(|table| Some(table.get("name")?.as_str()?.to_owned()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Every entry currently in one of a profile's lists.
    #[must_use]
    pub fn entries(&self, profile: usize, list: List) -> Vec<String> {
        self.document
            .get("profile")
            .and_then(Item::as_array_of_tables)
            .and_then(|tables| tables.get(profile))
            .and_then(|table| table.get(list.key()))
            .and_then(Item::as_array)
            .map(|array| {
                array
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Resolves which profile a command means: the one named, or the only one there.
    ///
    /// # Errors
    ///
    /// If the name is not present, or none was given and there is more than one.
    pub fn which(&self, wanted: Option<&str>) -> Result<usize, EditError> {
        let names = self.profiles();
        match wanted {
            Some(name) => names
                .iter()
                .position(|found| found == name)
                .ok_or_else(|| EditError::NoProfile(name.to_owned())),
            None => match names.len() {
                0 => Err(EditError::NoProfiles),
                1 => Ok(0),
                _ => Err(EditError::Ambiguous(names.join(", "))),
            },
        }
    }

    /// Appends a value, creating the array if it is not there yet.
    ///
    /// Returns whether anything changed, so a repeat says so rather than rewriting an
    /// identical file.
    ///
    /// # Errors
    ///
    /// If the profile does not exist.
    pub fn add(&mut self, profile: usize, list: List, value: &str) -> Result<bool, EditError> {
        let array = self.array(profile, list)?;
        if array.iter().any(|item| item.as_str() == Some(value)) {
            return Ok(false);
        }
        array.push(value);

        // One entry per line. These lists are read far more than they are written, and
        // a wrapped single line is unreadable at ten entries - but only the entry just
        // added is reformatted, so a hand-chosen layout elsewhere survives.
        if let Some(added) = array.iter_mut().last() {
            added.decor_mut().set_prefix("\n  ");
        }
        array.set_trailing_comma(true);
        Ok(true)
    }

    /// Removes a value and its own decoration.
    ///
    /// `toml_edit` carries an entry's leading comment as that entry's prefix, so
    /// removing it takes the comment lines immediately above with it - which is what
    /// `config.md` section 9 asks for. A comment separated by a blank line belongs to
    /// what follows it rather than to this entry, and stays put.
    ///
    /// # Errors
    ///
    /// If the profile does not exist, or the value is not in the list.
    pub fn remove(&mut self, profile: usize, list: List, value: &str) -> Result<(), EditError> {
        let key = list.key();
        let array = self.array(profile, list)?;
        let Some(index) = array.iter().position(|item| item.as_str() == Some(value)) else {
            return Err(EditError::NotPresent {
                value: value.to_owned(),
                list: key.to_owned(),
            });
        };
        array.remove(index);
        Ok(())
    }

    fn array(&mut self, profile: usize, list: List) -> Result<&mut Array, EditError> {
        let tables = self
            .document
            .get_mut("profile")
            .and_then(Item::as_array_of_tables_mut)
            .ok_or(EditError::NoProfiles)?;
        let table = tables
            .get_mut(profile)
            .ok_or_else(|| EditError::NoProfile(profile.to_string()))?;

        let key = list.key();
        if table.get(key).is_none() {
            table[key] = Item::Value(Value::Array(Array::new()));
        }
        table[key]
            .as_array_mut()
            .ok_or_else(|| EditError::NotPresent {
                value: key.to_owned(),
                list: "an array".to_owned(),
            })
    }
}

/// The starter file `config init` writes.
///
/// Every line somebody would want to change is present and explained, because a
/// config whose options you have to look up is a config people leave at its defaults.
#[must_use]
pub fn starter(home: &str) -> String {
    format!(
        "# Tycho captures what you list here into a git store and pushes that store to\n\
         # folders that survive this machine. `tycho config check` validates this file.\n\
         version = 1\n\
         \n\
         [[profile]]\n\
         name = \"personal\"\n\
         \n\
         # Everything under these is captured. A git repository inside one is captured\n\
         # with its full history, plus what git alone could never bring back:\n\
         # uncommitted edits, untracked files, and anything gitignored.\n\
         watch = [\n\
         \x20 \"{home}/Documents\",\n\
         ]\n\
         \n\
         # Paths and globs to leave out. `tycho run --dry-run` reports every rule that\n\
         # matched nothing, which is how a typo surfaces before it costs you gigabytes.\n\
         ignore = [\n\
         ]\n\
         \n\
         # Exceptions to the line above, for something inside a path you ignored.\n\
         reinclude = [\n\
         ]\n\
         \n\
         # Where the backup goes: a synced cloud folder, or a drive. Tycho creates the\n\
         # repository inside it on first run and writes nowhere else. Mark a removable\n\
         # drive optional so being unplugged is a warning rather than a failure.\n\
         remotes = [\n\
         \x20 # {{ name = \"drive\", path = \"{home}/Library/CloudStorage/…/Backups\" }},\n\
         \x20 # {{ name = \"t7\", path = \"/Volumes/T7/tycho\", optional = true }},\n\
         ]\n\
         \n\
         # When it runs by itself, once `tycho service install` has been run.\n\
         schedule = {{ weekly = {{ day = \"sunday\", at = \"12:00\" }} }}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::{Editing, List, starter};

    const WITH_COMMENTS: &str = r#"# the whole file's heading
version = 1

[[profile]]
name = "demo"

# why these roots and not others
watch = [
  # the important one
  "/home/me/A",
  "/home/me/B", # trailing note
]
"#;

    fn open(text: &str) -> (tempfile::TempDir, Editing) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("tycho.toml");
        std::fs::write(&path, text).expect("write");
        let editing = Editing::open(&path).expect("parse");
        (dir, editing)
    }

    /// The reason TOML was chosen over JSON. A command that lost these would make the
    /// file worse every time it was used.
    #[test]
    fn adding_a_root_keeps_every_comment() {
        let (_dir, mut editing) = open(WITH_COMMENTS);
        assert!(editing.add(0, List::Watch, "/home/me/C").expect("add"));

        let text = editing.text();
        for comment in [
            "# the whole file's heading",
            "# why these roots and not others",
            "# the important one",
            "# trailing note",
        ] {
            assert!(text.contains(comment), "{comment} was lost:\n{text}");
        }
        assert!(text.contains("/home/me/C"), "{text}");
    }

    /// And the file it produces has to still parse, or the next command cannot read
    /// what this one wrote.
    #[test]
    fn the_edited_file_is_still_valid_toml_and_still_a_config() {
        let (_dir, mut editing) = open(WITH_COMMENTS);
        editing.add(0, List::Watch, "/home/me/C").expect("add");
        editing.add(0, List::Ignore, "*.tmp").expect("add");

        let parsed = crate::config::parse_with(
            &editing.text(),
            Some(std::path::Path::new("/home/me")),
            |_| None,
        )
        .expect("still valid TOML");
        let profile = parsed
            .config
            .profiles
            .first()
            .expect("the profile survived");
        assert_eq!(profile.watch.len(), 3);
        assert_eq!(profile.ignore_globs, ["*.tmp"]);
    }

    #[test]
    fn adding_something_already_there_changes_nothing() {
        let (_dir, mut editing) = open(WITH_COMMENTS);
        let before = editing.text();
        assert!(!editing.add(0, List::Watch, "/home/me/A").expect("add"));
        assert_eq!(editing.text(), before);
    }

    #[test]
    fn removing_a_root_leaves_the_rest_alone() {
        let (_dir, mut editing) = open(WITH_COMMENTS);
        editing
            .remove(0, List::Watch, "/home/me/B")
            .expect("remove");

        let text = editing.text();
        assert!(!text.contains("/home/me/B"), "{text}");
        assert!(text.contains("/home/me/A"), "{text}");
        assert!(text.contains("# why these roots and not others"), "{text}");
        assert!(text.contains("# the important one"), "{text}");
    }

    #[test]
    fn removing_something_absent_says_so_rather_than_succeeding_quietly() {
        let (_dir, mut editing) = open(WITH_COMMENTS);
        let error = editing
            .remove(0, List::Watch, "/home/me/never")
            .expect_err("not in the list");
        assert!(error.to_string().contains("/home/me/never"), "{error}");
    }

    /// A hand-written config may have no `reinclude` key at all, and adding one must
    /// not be an error.
    #[test]
    fn a_missing_list_is_created() {
        let (_dir, mut editing) = open(WITH_COMMENTS);
        assert!(
            editing
                .add(0, List::Reinclude, "/home/me/A/keep")
                .expect("add")
        );
        assert_eq!(
            editing.entries(0, List::Reinclude),
            ["/home/me/A/keep"],
            "{}",
            editing.text()
        );
    }

    #[test]
    fn one_profile_needs_no_name_and_two_do() {
        let (_dir, one) = open(WITH_COMMENTS);
        assert_eq!(one.which(None).expect("the only profile"), 0);

        let two = format!("{WITH_COMMENTS}\n[[profile]]\nname = \"work\"\nwatch = []\n");
        let (_dir, editing) = open(&two);
        assert_eq!(editing.profiles(), ["demo", "work"]);
        assert_eq!(editing.which(Some("work")).expect("named"), 1);
        assert!(editing.which(None).is_err(), "ambiguous must not guess");
    }

    /// `config init` must not hand somebody a file that `config check` rejects.
    #[test]
    fn the_starter_file_parses_and_only_warns_about_what_it_cannot_know() {
        let text = starter("/home/me");
        let parsed =
            crate::config::parse_with(&text, Some(std::path::Path::new("/home/me")), |_| None)
                .expect("valid TOML");

        // `NoRemotes` is expected and unavoidable: only the person running it knows
        // where their cloud folder is, so the starter comments the examples out.
        let unexpected: Vec<_> = parsed
            .diagnostics
            .iter()
            .filter(|item| item.severity == crate::config::Severity::Error)
            .filter(|item| item.kind != crate::config::DiagnosticKind::NoRemotes)
            .collect();
        assert!(unexpected.is_empty(), "{unexpected:#?}");
    }
}
