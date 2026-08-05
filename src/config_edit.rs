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
use std::time::Duration;
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value};

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
        let table = self.profile_table_mut(profile)?;
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

    fn profile_table_mut(&mut self, profile: usize) -> Result<&mut Table, EditError> {
        let tables = self
            .document
            .get_mut("profile")
            .and_then(Item::as_array_of_tables_mut)
            .ok_or(EditError::NoProfiles)?;
        tables
            .get_mut(profile)
            .ok_or_else(|| EditError::NoProfile(profile.to_string()))
    }

    /// Appends a new `[[profile]]` table.
    ///
    /// # Errors
    ///
    /// If the document's `profile` key exists and is not an array of tables - a
    /// malformed file `open` would already have rejected.
    pub fn add_profile(&mut self, new: &NewProfile) -> Result<(), EditError> {
        if self.document.get("profile").is_none() {
            self.document["profile"] = Item::ArrayOfTables(ArrayOfTables::new());
        }
        let tables = self.document["profile"]
            .as_array_of_tables_mut()
            .ok_or(EditError::NoProfiles)?;
        tables.push(build_profile_table(new));
        Ok(())
    }

    /// Removes the profile at `index`.
    ///
    /// # Errors
    ///
    /// If there is no profile at that index.
    pub fn remove_profile(&mut self, index: usize) -> Result<(), EditError> {
        let tables = self
            .document
            .get_mut("profile")
            .and_then(Item::as_array_of_tables_mut)
            .ok_or(EditError::NoProfiles)?;
        if index >= tables.len() {
            return Err(EditError::NoProfile(index.to_string()));
        }
        tables.remove(index);
        Ok(())
    }

    /// Every remote configured on a profile, in file order.
    #[must_use]
    pub fn remotes(&self, profile: usize) -> Vec<RemoteEntry> {
        self.document
            .get("profile")
            .and_then(Item::as_array_of_tables)
            .and_then(|tables| tables.get(profile))
            .and_then(|table| table.get("remotes"))
            .and_then(Item::as_array)
            .map(|array| array.iter().filter_map(remote_entry).collect())
            .unwrap_or_default()
    }

    /// Appends a remote to a profile's `remotes` array, creating it if this is the
    /// first one.
    ///
    /// # Errors
    ///
    /// If the profile does not exist.
    pub fn add_remote(&mut self, profile: usize, remote: &NewRemote) -> Result<(), EditError> {
        let array = self.remotes_array(profile)?;
        array.push(remote_inline(remote));
        if let Some(added) = array.iter_mut().last() {
            added.decor_mut().set_prefix("\n  ");
        }
        array.set_trailing_comma(true);
        Ok(())
    }

    /// Removes a remote by name.
    ///
    /// # Errors
    ///
    /// If the profile does not exist, or has no remote by that name.
    pub fn remove_remote(&mut self, profile: usize, name: &str) -> Result<(), EditError> {
        let array = self.remotes_array(profile)?;
        let index = array
            .iter()
            .position(|value| value_str(value, "name") == Some(name));
        let Some(index) = index else {
            return Err(EditError::NotPresent {
                value: name.to_owned(),
                list: "remotes".to_owned(),
            });
        };
        array.remove(index);
        Ok(())
    }

    fn remotes_array(&mut self, profile: usize) -> Result<&mut Array, EditError> {
        let table = self.profile_table_mut(profile)?;
        if table.get("remotes").is_none() {
            table["remotes"] = Item::Value(Value::Array(Array::new()));
        }
        table["remotes"]
            .as_array_mut()
            .ok_or_else(|| EditError::NotPresent {
                value: "remotes".to_owned(),
                list: "an array".to_owned(),
            })
    }

    /// Whether a profile has **said** it wants no remotes.
    ///
    /// Not the same question as having none. A profile with an empty `remotes` and no
    /// `local_only` is unfinished setup and fails `config check`; one with the flag is
    /// a deliberate choice that passes. Reporting both as "local only" told somebody
    /// their broken config was a decision they had made.
    #[must_use]
    pub fn is_local_only(&self, profile: usize) -> bool {
        self.document
            .get("profile")
            .and_then(Item::as_array_of_tables)
            .and_then(|tables| tables.get(profile))
            .and_then(|table| table.get("local_only"))
            .and_then(Item::as_bool)
            .unwrap_or(false)
    }

    /// A profile's `store_path` override, exactly as written.
    ///
    /// Unexpanded, because that is how the file holds it and how it stays portable;
    /// the caller runs it through `AbsPath::parse`, which is the one place `~` and
    /// `$HOME` are resolved.
    #[must_use]
    pub fn store_path(&self, profile: usize) -> Option<String> {
        self.document
            .get("profile")
            .and_then(Item::as_array_of_tables)
            .and_then(|tables| tables.get(profile))
            .and_then(|table| table.get("store_path"))
            .and_then(Item::as_str)
            .map(str::to_owned)
    }

    /// Sets or clears `local_only` on a profile.
    ///
    /// # Errors
    ///
    /// If the profile does not exist.
    pub fn set_local_only(&mut self, profile: usize, value: bool) -> Result<(), EditError> {
        let table = self.profile_table_mut(profile)?;
        if value {
            table["local_only"] = Item::Value(Value::from(value));
        } else {
            table.remove("local_only");
        }
        Ok(())
    }

    /// Replaces a profile's schedule.
    ///
    /// # Errors
    ///
    /// If the profile does not exist.
    pub fn set_schedule(
        &mut self,
        profile: usize,
        schedule: crate::config::Schedule,
    ) -> Result<(), EditError> {
        let table = self.profile_table_mut(profile)?;
        table["schedule"] = Item::Value(Value::InlineTable(schedule_inline(schedule)));
        Ok(())
    }

    /// Clears a profile's schedule, so it only runs when invoked by hand.
    ///
    /// # Errors
    ///
    /// If the profile does not exist.
    pub fn clear_schedule(&mut self, profile: usize) -> Result<(), EditError> {
        let table = self.profile_table_mut(profile)?;
        table.remove("schedule");
        Ok(())
    }

    /// Whether a profile carries a `schedule` key at all, without validating it -
    /// that is `config check`'s job.
    #[must_use]
    pub fn has_schedule(&self, profile: usize) -> bool {
        self.document
            .get("profile")
            .and_then(Item::as_array_of_tables)
            .and_then(|tables| tables.get(profile))
            .is_some_and(|table| table.get("schedule").is_some())
    }
}

/// A remote as read back from the file, unvalidated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteEntry {
    pub name: String,
    pub path: String,
    pub optional: bool,
    pub trust_ownership: bool,
    pub behind_tolerance: Option<u32>,
}

/// A remote to append. Every field here becomes a key in the same inline table
/// `config.md` documents, and a default-valued one is omitted rather than written
/// out, so an added remote reads the way a hand-written one would.
#[derive(Clone, Debug)]
pub struct NewRemote {
    pub name: String,
    pub path: String,
    pub optional: bool,
    pub trust_ownership: bool,
    pub behind_tolerance: Option<u32>,
}

/// A profile to append.
#[derive(Clone, Debug)]
pub struct NewProfile {
    pub name: String,
    pub watch: Vec<String>,
    pub remotes: Vec<NewRemote>,
    pub schedule: Option<crate::config::Schedule>,
    pub local_only: bool,
}

/// The TOML block `profile add --dry-run` would append, without touching a file.
#[must_use]
pub fn preview_profile(new: &NewProfile) -> String {
    let mut tables = ArrayOfTables::new();
    tables.push(build_profile_table(new));
    let mut document = DocumentMut::new();
    document["profile"] = Item::ArrayOfTables(tables);
    document.to_string()
}

fn build_profile_table(new: &NewProfile) -> Table {
    let mut table = Table::new();
    table.insert("name", Item::Value(Value::from(new.name.clone())));
    if !new.watch.is_empty() {
        let mut array = Array::new();
        for path in &new.watch {
            array.push(path.clone());
        }
        table.insert("watch", Item::Value(Value::Array(multiline(array))));
    }
    if !new.remotes.is_empty() {
        let mut array = Array::new();
        for remote in &new.remotes {
            array.push(remote_inline(remote));
        }
        table.insert("remotes", Item::Value(Value::Array(multiline(array))));
    }
    if let Some(schedule) = new.schedule {
        table.insert(
            "schedule",
            Item::Value(Value::InlineTable(schedule_inline(schedule))),
        );
    }
    if new.local_only {
        table.insert("local_only", Item::Value(Value::from(true)));
    }
    table
}

/// One entry per line, the same layout `starter` writes by hand - a wrapped single
/// line is unreadable past a handful of entries.
fn multiline(mut array: Array) -> Array {
    for item in array.iter_mut() {
        item.decor_mut().set_prefix("\n  ");
    }
    array.set_trailing_comma(true);
    array.set_trailing("\n");
    array
}

fn remote_inline(remote: &NewRemote) -> InlineTable {
    let mut table = InlineTable::new();
    table.insert("name", Value::from(remote.name.clone()));
    table.insert("path", Value::from(remote.path.clone()));
    if remote.optional {
        table.insert("optional", Value::from(true));
    }
    if remote.trust_ownership {
        table.insert("trust_ownership", Value::from(true));
    }
    if let Some(tolerance) = remote.behind_tolerance {
        table.insert("behind_tolerance", Value::from(i64::from(tolerance)));
    }
    table
}

fn remote_entry(value: &Value) -> Option<RemoteEntry> {
    let table = value.as_inline_table()?;
    Some(RemoteEntry {
        name: table.get("name")?.as_str()?.to_owned(),
        path: table.get("path")?.as_str()?.to_owned(),
        optional: table
            .get("optional")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        trust_ownership: table
            .get("trust_ownership")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        behind_tolerance: table
            .get("behind_tolerance")
            .and_then(Value::as_integer)
            .and_then(|n| u32::try_from(n).ok()),
    })
}

fn value_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.as_inline_table()?.get(key)?.as_str()
}

/// `{ daily = { at = "..." } }`, `{ weekly = { day = "...", at = "..." } }` or
/// `{ every = "..." }` - the same three shapes `config::raw::RawSchedule` reads.
fn schedule_inline(schedule: crate::config::Schedule) -> InlineTable {
    let mut table = InlineTable::new();
    match schedule {
        crate::config::Schedule::Daily { at } => {
            let mut inner = InlineTable::new();
            inner.insert("at", Value::from(at.to_string()));
            table.insert("daily", Value::InlineTable(inner));
        }
        crate::config::Schedule::Weekly { day, at } => {
            let mut inner = InlineTable::new();
            inner.insert("day", Value::from(format!("{day:?}").to_lowercase()));
            inner.insert("at", Value::from(at.to_string()));
            table.insert("weekly", Value::InlineTable(inner));
        }
        crate::config::Schedule::Every(every) => {
            table.insert("every", Value::from(every_spec(every)));
        }
    }
    table
}

/// The compact `<N>h`/`<N>m` form the `every:` SPEC grammar accepts - the only shape
/// `schedule set` ever produces here, so round-tripping it back through the same two
/// units is exact.
fn every_spec(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds > 0 && seconds.is_multiple_of(3600) {
        let hours = seconds / 3600;
        format!("{hours}h")
    } else {
        let minutes = (seconds / 60).max(1);
        format!("{minutes}m")
    }
}

/// The starter file `config init` writes.
///
/// Every line somebody would want to change is present and explained, because a
/// config whose options you have to look up is a config people leave at its defaults.
#[must_use]
pub fn starter() -> String {
    "# Tycho captures what you list here into a git store and pushes that store to\n\
         # folders that survive this machine. `tycho config check` validates this file.\n\
         version = 1\n\
         # This checkout's path, so `tycho __bootstrap` does not have to guess it.\n\
         # source = \"~/Developer/tycho\"\n\
         \n\
         # Nothing is watched until a profile exists. `tycho profile add` writes one, and\n\
         # `tycho config check` prints the exact command - everything below is what it\n\
         # produces, so hand-editing instead is equally supported.\n\
         #\n\
         # [[profile]]\n\
         # name = \"personal\"\n\
         #\n\
         # Everything under these is captured. A git repository inside one is captured\n\
         # with its full history, plus what git alone could never bring back:\n\
         # uncommitted edits, untracked files, and anything gitignored.\n\
         # watch = [\n\
         #   \"~/Documents\",\n\
         # ]\n\
         #\n\
         # Paths and globs to leave out. `tycho run --dry-run` reports every rule that\n\
         # matched nothing, which is how a typo surfaces before it costs you gigabytes.\n\
         # ignore = []\n\
         #\n\
         # Exceptions to the line above, for something inside a path you ignored.\n\
         # reinclude = []\n\
         #\n\
         # Where the backup goes: a synced cloud folder, or a drive. Tycho creates the\n\
         # repository inside it on first run and writes nowhere else. Mark a removable\n\
         # drive optional so being unplugged is a warning rather than a failure, and add\n\
         # trust_ownership on one that records none - exFAT and FAT32 - which git\n\
         # otherwise refuses to operate on.\n\
         #\n\
         # `name` is a label for this destination, not a location - `path` finds the\n\
         # drive. It is what `status` and `doctor` call the remote, so make it read like\n\
         # the thing printed on the drive. Renaming it later starts that remote's\n\
         # history over: the state file keys last-seen and behind-count by the name, so\n\
         # a rename shows as `unseen` and re-verifies from scratch.\n\
         # remotes = [\n\
         #   { name = \"drive\", path = \"~/Library/CloudStorage/GoogleDrive-you/Backups\" },\n\
         #   { name = \"t7\", path = \"/Volumes/T7/tycho\", optional = true, trust_ownership = true },\n\
         # ]\n\
         #\n\
         # When it runs by itself, once `tycho service install` has been run.\n\
         # schedule = { weekly = { day = \"sunday\", at = \"12:00\" } }\n"
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::{Editing, List, starter};
    use std::time::Duration;

    /// A drive letter is what makes a path absolute on Windows, and forward slashes
    /// keep it inside a TOML basic string without escaping.
    #[cfg(unix)]
    const HOME: &str = "/home/me";
    #[cfg(windows)]
    const HOME: &str = "C:/home/me";

    /// A remote path the host can actually hold. `AbsPath` drops what it cannot
    /// parse, so a POSIX-only literal here leaves a profile with zero remotes on
    /// Windows and the assertion blames the edit rather than the fixture.
    #[cfg(unix)]
    const REMOTE: &str = "/Volumes/Drive/Backups";
    #[cfg(windows)]
    const REMOTE: &str = "D:/Backups";

    fn home(rest: &str) -> String {
        format!("{HOME}{rest}")
    }

    fn with_comments() -> String {
        format!(
            r#"# the whole file's heading
version = 1

[[profile]]
name = "demo"

# why these roots and not others
watch = [
  # the important one
  "{HOME}/A",
  "{HOME}/B", # trailing note
]
"#
        )
    }

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
        let (_dir, mut editing) = open(&with_comments());
        assert!(editing.add(0, List::Watch, &home("/C")).expect("add"));

        let text = editing.text();
        for comment in [
            "# the whole file's heading",
            "# why these roots and not others",
            "# the important one",
            "# trailing note",
        ] {
            assert!(text.contains(comment), "{comment} was lost:\n{text}");
        }
        assert!(text.contains(&home("/C")), "{text}");
    }

    /// And the file it produces has to still parse, or the next command cannot read
    /// what this one wrote.
    #[test]
    fn the_edited_file_is_still_valid_toml_and_still_a_config() {
        let (_dir, mut editing) = open(&with_comments());
        editing.add(0, List::Watch, &home("/C")).expect("add");
        editing.add(0, List::Ignore, "*.tmp").expect("add");

        let parsed =
            crate::config::parse_with(&editing.text(), Some(std::path::Path::new(HOME)), |_| None)
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
        let (_dir, mut editing) = open(&with_comments());
        let before = editing.text();
        assert!(!editing.add(0, List::Watch, &home("/A")).expect("add"));
        assert_eq!(editing.text(), before);
    }

    #[test]
    fn removing_a_root_leaves_the_rest_alone() {
        let (_dir, mut editing) = open(&with_comments());
        editing.remove(0, List::Watch, &home("/B")).expect("remove");

        let text = editing.text();
        assert!(!text.contains(&home("/B")), "{text}");
        assert!(text.contains(&home("/A")), "{text}");
        assert!(text.contains("# why these roots and not others"), "{text}");
        assert!(text.contains("# the important one"), "{text}");
    }

    #[test]
    fn removing_something_absent_says_so_rather_than_succeeding_quietly() {
        let (_dir, mut editing) = open(&with_comments());
        let error = editing
            .remove(0, List::Watch, &home("/never"))
            .expect_err("not in the list");
        assert!(error.to_string().contains(&home("/never")), "{error}");
    }

    /// A hand-written config may have no `reinclude` key at all, and adding one must
    /// not be an error.
    #[test]
    fn a_missing_list_is_created() {
        let (_dir, mut editing) = open(&with_comments());
        assert!(
            editing
                .add(0, List::Reinclude, &home("/A/keep"))
                .expect("add")
        );
        assert_eq!(
            editing.entries(0, List::Reinclude),
            [home("/A/keep")],
            "{}",
            editing.text()
        );
    }

    #[test]
    fn one_profile_needs_no_name_and_two_do() {
        let (_dir, one) = open(&with_comments());
        assert_eq!(one.which(None).expect("the only profile"), 0);

        let two = format!(
            "{}\n[[profile]]\nname = \"work\"\nwatch = []\n",
            with_comments()
        );
        let (_dir, editing) = open(&two);
        assert_eq!(editing.profiles(), ["demo", "work"]);
        assert_eq!(editing.which(Some("work")).expect("named"), 1);
        assert!(editing.which(None).is_err(), "ambiguous must not guess");
    }

    /// The starter names no particular machine.
    ///
    /// `rules.rs` refuses to expand `~` on the way in, because "resolving them on the
    /// way in would bake this machine's home directory into it" - and the starter used
    /// to interpolate the real home into three commented examples, doing exactly that.
    /// It also travels: the config is captured into the backup tree, so those lines
    /// went onto every drive the store was pushed to.
    #[test]
    fn the_starter_bakes_in_no_home_directory() {
        let text = starter();
        assert!(!text.contains("/Users/"), "{text}");
        assert!(!text.contains("/home/"), "{text}");
        assert!(!text.contains("C:\\"), "{text}");
        assert!(
            text.contains("~/Documents"),
            "the examples still show a path"
        );
    }

    /// The starter defines nothing and says exactly that.
    ///
    /// It used to ship an active profile with its remotes commented out, so a new
    /// install answered `config check` with `NoRemotes` - a profile that looked
    /// configured and could not run. Everything is commented now, and the single
    /// finding is the one whose hint is the command that sets a profile up.
    #[test]
    fn the_starter_file_parses_and_reports_only_that_it_has_no_profiles() {
        let text = starter();
        let parsed = crate::config::parse_with(&text, Some(std::path::Path::new(HOME)), |_| None)
            .expect("valid TOML");

        let kinds: Vec<_> = parsed
            .diagnostics
            .iter()
            .map(|item| item.kind.clone())
            .collect();
        assert_eq!(
            kinds,
            vec![crate::config::DiagnosticKind::NoProfiles],
            "{:#?}",
            parsed.diagnostics
        );
    }

    /// The command the starter's own hint tells a new user to run has to work against
    /// the file the starter just wrote, from zero profiles.
    #[test]
    fn a_profile_can_be_added_to_the_starter_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("tycho.toml");
        std::fs::write(&path, starter()).expect("write");

        let mut editing = Editing::open(&path).expect("open");
        assert!(editing.profiles().is_empty(), "the starter defines none");
        editing
            .add_profile(&super::NewProfile {
                name: "me".to_owned(),
                watch: vec![home("/Documents")],
                remotes: vec![new_remote("drive", REMOTE)],
                schedule: None,
                local_only: false,
            })
            .expect("add to a file with no profile array at all");
        assert_eq!(editing.profiles(), ["me"]);
    }

    fn new_remote(name: &str, path: &str) -> super::NewRemote {
        super::NewRemote {
            name: name.to_owned(),
            path: path.to_owned(),
            optional: false,
            trust_ownership: false,
            behind_tolerance: None,
        }
    }

    #[test]
    fn a_new_profile_appends_a_table_that_still_parses() {
        let (_dir, mut editing) = open(&with_comments());
        editing
            .add_profile(&super::NewProfile {
                name: "work".to_owned(),
                watch: vec![home("/Work")],
                remotes: vec![new_remote("drive", REMOTE)],
                schedule: Some(crate::config::Schedule::Daily {
                    at: crate::config::TimeOfDay {
                        hour: 12,
                        minute: 0,
                    },
                }),
                local_only: false,
            })
            .expect("add profile");

        assert_eq!(editing.profiles(), ["demo", "work"]);
        let parsed =
            crate::config::parse_with(&editing.text(), Some(std::path::Path::new(HOME)), |_| None)
                .expect("still valid TOML");
        let work = parsed
            .config
            .profiles
            .iter()
            .find(|profile| profile.name.as_str() == "work")
            .expect("the new profile survived");
        assert_eq!(work.watch.len(), 1);
        assert_eq!(work.remotes.len(), 1);
        assert_eq!(work.remotes[0].name.as_str(), "drive");
    }

    #[test]
    fn removing_a_profile_leaves_the_others_untouched() {
        let two = format!(
            "{}\n[[profile]]\nname = \"work\"\nwatch = []\n",
            with_comments()
        );
        let (_dir, mut editing) = open(&two);
        editing.remove_profile(0).expect("remove");
        assert_eq!(editing.profiles(), ["work"]);
    }

    #[test]
    fn a_remote_round_trips_through_add_and_list() {
        let (_dir, mut editing) = open(&with_comments());
        editing
            .add_remote(
                0,
                &super::NewRemote {
                    name: "drive".to_owned(),
                    path: REMOTE.to_owned(),
                    optional: true,
                    trust_ownership: true,
                    behind_tolerance: Some(2),
                },
            )
            .expect("add remote");

        let remotes = editing.remotes(0);
        assert_eq!(remotes.len(), 1);
        assert_eq!(remotes[0].name, "drive");
        assert_eq!(remotes[0].path, REMOTE);
        assert!(remotes[0].optional);
        assert!(remotes[0].trust_ownership);
        assert_eq!(remotes[0].behind_tolerance, Some(2));

        editing.remove_remote(0, "drive").expect("remove remote");
        assert!(editing.remotes(0).is_empty());
    }

    #[test]
    fn removing_an_absent_remote_says_so() {
        let (_dir, mut editing) = open(&with_comments());
        let error = editing
            .remove_remote(0, "ghost")
            .expect_err("not configured");
        assert!(error.to_string().contains("ghost"), "{error}");
    }

    #[test]
    fn a_schedule_can_be_set_and_cleared() {
        let (_dir, mut editing) = open(&with_comments());
        assert!(!editing.has_schedule(0));

        editing
            .set_schedule(
                0,
                crate::config::Schedule::Every(Duration::from_secs(21_600)),
            )
            .expect("set schedule");
        assert!(editing.has_schedule(0));
        assert!(
            editing.text().contains("every = \"6h\""),
            "{}",
            editing.text()
        );

        editing.clear_schedule(0).expect("clear schedule");
        assert!(!editing.has_schedule(0));
    }

    #[test]
    fn a_weekly_schedule_writes_the_lowercase_day() {
        let (_dir, mut editing) = open(&with_comments());
        editing
            .set_schedule(
                0,
                crate::config::Schedule::Weekly {
                    day: crate::config::Weekday::Sunday,
                    at: crate::config::TimeOfDay {
                        hour: 12,
                        minute: 0,
                    },
                },
            )
            .expect("set schedule");
        assert!(
            editing.text().contains(r#"day = "sunday""#),
            "{}",
            editing.text()
        );
    }

    #[test]
    fn local_only_is_written_and_removed_rather_than_ever_written_false() {
        let (_dir, mut editing) = open(&with_comments());
        editing.set_local_only(0, true).expect("set");
        assert!(
            editing.text().contains("local_only = true"),
            "{}",
            editing.text()
        );

        editing.set_local_only(0, false).expect("clear");
        assert!(!editing.text().contains("local_only"), "{}", editing.text());
    }

    #[test]
    fn a_dry_run_preview_matches_what_add_profile_would_write() {
        let new = super::NewProfile {
            name: "work".to_owned(),
            watch: vec![home("/Work")],
            remotes: vec![],
            schedule: None,
            local_only: true,
        };
        let preview = super::preview_profile(&new);
        assert!(preview.contains("[[profile]]"), "{preview}");
        assert!(preview.contains("name = \"work\""), "{preview}");
        assert!(preview.contains("local_only = true"), "{preview}");
    }
}
