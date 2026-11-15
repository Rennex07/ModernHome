//! The plugin's configuration, parsed one file at a time.
//!
//! ## Why `serde_norway::Value` and not typed structs
//!
//! Six of the seven files could have been typed structs. `gui/homes.yml` could
//! not: its `items:` map holds `previous-page-slot: 45` - a number - beside
//! `previous-page:` - a section with a name, a lore and a material. A struct
//! cannot hold both, and a map with two value types needs an enum that would be
//! nothing but plumbing.
//!
//! So every file is parsed into a [`Value`] tree and read through small accessors
//! that carry the Java default inline. That is also what keeps the Paper
//! behaviour honest: `FileConfiguration.getInt("teleport-time", 5)` answers `5`
//! for a *missing key*, and `getString(key, key)` answers the key itself, so a
//! typo'd message key is visible in game rather than silently blank. Both are
//! reproduced here.
//!
//! ## A broken file is never silently replaced
//!
//! [load](crate::Config::load) returns what it could read and what it could not.
//! A file that fails to parse falls back to the shipped default and is reported,
//! rather than being rewritten - an admin who has edited a file must not lose the
//! edit to a syntax error they can see.
//!
//! ## What is not here
//!
//! The HomesGui `fillers:` and ConfirmGui `items:` sections are read as trees
//! and interpreted by [`crate::homes_gui`] and [`crate::confirm_gui`], because
//! they are only meaningful while a screen is being built.

use serde_norway::Value;

use crate::layout::{ConfirmLayout, Layout};

/// The shipped default files, compiled in.
///
/// A Pumpkin plugin has no JAR resources, so the Java plugin's
/// `saveResource("gui/homes.yml")` becomes a file this module writes once, from
/// here, on first load. It is never rewritten afterwards, which is how the
/// comments an admin leaves in it survive.
pub mod defaults {
    pub const CONFIG: &str = include_str!("../config/config.yml");
    pub const MESSAGES: &str = include_str!("../config/messages.yml");
    pub const SOUNDS: &str = include_str!("../config/sounds.yml");
    pub const SERVER: &str = include_str!("../config/server.yml");
    pub const DATABASE: &str = include_str!("../config/database.yml");
    pub const HOMES_GUI: &str = include_str!("../config/gui/homes.yml");
    pub const CONFIRM_GUI: &str = include_str!("../config/gui/confirm.yml");
}

/// The default layout, used when `layout:` is empty or missing.
///
/// `HomesGuiConfig.parseLayout`'s own fallback list. The fifth row is all `#`,
/// which is what makes the previous/next buttons land on slots 45 and 53.
pub const DEFAULT_HOMES_LAYOUT: [&str; 6] = [
    "---------",
    "--h-h-h--",
    "---------",
    "-h-h-h-h-",
    "---------",
    "#########",
];

/// The default confirm disposition, used when `gui-disposition:` is empty.
///
/// `ConfirmGuiConfig.parseLayout`'s own fallback list.
pub const DEFAULT_CONFIRM_LAYOUT: [&str; 3] = ["---------", "--d-h-a--", "----g----"];

/// The default teleport countdown, in seconds.
const DEFAULT_TELEPORT_TIME: i32 = 5;
/// The default combat window, in seconds.
const DEFAULT_COMBAT_DELAY: i32 = 5;
/// The default home limit for a player who holds no `ModernHome:limit.N`.
const DEFAULT_LIMIT_SCAN_MAX: i32 = 256;
/// Fallback when `items.previous-page-slot` is missing.
const DEFAULT_PREVIOUS_SLOT: u32 = 45;
/// Fallback when `items.next-page-slot` is missing.
const DEFAULT_NEXT_SLOT: u32 = 53;

/// One parsed configuration.
///
/// Built by [`load`] from a data folder, and cloned out from behind a lock
/// whenever a command, a GUI or a task needs it.
#[derive(Debug, Clone)]
pub struct Config {
    /// `config.yml`.
    main: Value,
    /// `messages.yml`.
    messages: Value,
    /// `sounds.yml`.
    sounds: Value,
    /// `gui/homes.yml`.
    homes_gui: Value,
    /// `gui/confirm.yml`.
    confirm_gui: Value,
    /// `server.yml`.
    server: Value,
    /// `database.yml`.
    database: Value,
    /// The homes screen layout, parsed once from `homes_gui`.
    homes_layout: Layout,
    /// The confirm screen layout, parsed once from `confirm_gui`.
    confirm_layout: ConfirmLayout,
}

/// Where the homes are kept.
///
/// Chosen by `database.yml`'s `storage`. The JSON backend is the default,
/// because it is the one that needs nothing but the data folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Storage {
    /// One JSON file, replaced atomically after every change.
    Json,
    /// A SQLite database, one row per home.
    Sqlite,
    /// A MySQL or MariaDB database, one row per home.
    ///
    /// The two are the same backend: they speak the same protocol, and the
    /// only difference an admin sees is which name they write in `storage`.
    MySql,
    /// An explicitly configured backend is not supported.
    Invalid,
}

/// What [`load`] could and could not read.
///
/// A `problems` entry names the file and the parser's message. It is reported at
/// load and on every `/home reload`, because a stale default is worth telling an
/// admin about more than once.
#[derive(Debug)]
pub struct Loaded {
    /// The configuration. Always present - a broken file is replaced by the
    /// shipped default, so there is something to run with.
    pub value: Config,
    /// Every file that could not be read or parsed, and why.
    pub problems: Vec<String>,
}

impl Loaded {
    /// Whether every file read cleanly.
    #[must_use]
    pub fn clean(&self) -> bool {
        self.problems.is_empty()
    }
}

impl Config {
    /// An empty configuration made only of the shipped defaults.
    ///
    /// Only useful for tests, and for the divide-by-zero-free "before load" state
    /// a command can observe. No code path runs on one.
    #[must_use]
    pub fn defaults() -> Self {
        let parse = |text: &str| parse_or_empty(text).unwrap_or(Value::Null);
        let homes_gui = parse(defaults::HOMES_GUI);
        let confirm_gui = parse(defaults::CONFIRM_GUI);
        Self {
            main: parse(defaults::CONFIG),
            messages: parse(defaults::MESSAGES),
            sounds: parse(defaults::SOUNDS),
            homes_layout: Layout::homes(&string_list(&homes_gui, "layout")),
            confirm_layout: ConfirmLayout::parse(&string_list(&confirm_gui, "gui-disposition")),
            homes_gui,
            confirm_gui,
            server: parse(defaults::SERVER),
            database: parse(defaults::DATABASE),
        }
    }

    /// `teleport-time`: the countdown, in seconds.
    #[must_use]
    pub fn teleport_time(&self) -> i32 {
        self.main
            .get("teleport-time")
            .and_then(Value::as_i64)
            .map_or(DEFAULT_TELEPORT_TIME, |value| value as i32)
    }

    /// `combat-delay`: how long after taking damage a teleport is refused.
    #[must_use]
    pub fn combat_delay(&self) -> i32 {
        self.main
            .get("combat-delay")
            .and_then(Value::as_i64)
            .map_or(DEFAULT_COMBAT_DELAY, |value| value as i32)
    }

    /// `allow-chat-messages`.
    #[must_use]
    pub fn allow_chat(&self) -> bool {
        self.main
            .get("allow-chat-messages")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    /// `allow-actionbar-messages`.
    #[must_use]
    pub fn allow_actionbar(&self) -> bool {
        self.main
            .get("allow-actionbar-messages")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    /// `blacklisted-worlds`: worlds a home may not be set in.
    #[must_use]
    pub fn blacklisted_worlds(&self) -> Vec<String> {
        string_list(&self.main, "blacklisted-worlds")
    }

    /// Whether `world` may not have a home set in it.
    ///
    /// Java used `List.contains`, which is an exact, case-sensitive match. The
    /// shipped config comment warns about that, and the port keeps it.
    #[must_use]
    pub fn is_blacklisted(&self, world: &str) -> bool {
        self.blacklisted_worlds().iter().any(|name| name == world)
    }

    /// `home-limit-scan-max`: the highest limit permission that is looked for.
    #[must_use]
    pub fn limit_scan_max(&self) -> i32 {
        self.main
            .get("home-limit-scan-max")
            .and_then(Value::as_i64)
            .map_or(DEFAULT_LIMIT_SCAN_MAX, |value| value as i32)
    }

    /// `server.yml`'s `server-id`, defaulting to `"server-1"`.
    #[must_use]
    pub fn server_id(&self) -> String {
        string_at(&self.server, "server-id").unwrap_or_else(|| "server-1".to_string())
    }

    /// `database.yml`'s `store.file`, defaulting to `"homes.json"`.
    #[must_use]
    pub fn store_file(&self) -> String {
        self.database
            .get("store")
            .and_then(|store| store.get("file"))
            .and_then(Value::as_str)
            .unwrap_or("homes.json")
            .to_string()
    }

    /// `database.yml`'s `storage`, defaulting to the JSON backend.
    ///
    /// Unknown names are refused to avoid writing homes into the wrong backend.
    #[must_use]
    pub fn storage(&self) -> Storage {
        match self
            .database
            .get("storage")
            .and_then(Value::as_str)
            .map(str::trim)
        {
            Some("sqlite") => Storage::Sqlite,
            Some("mysql") | Some("mariadb") => Storage::MySql,
            Some("json") => Storage::Json,
            None if self.database.get("storage").is_none() => Storage::Json,
            _ => Storage::Invalid,
        }
    }

    /// A setting from `database.yml`'s `mysql` section.
    ///
    /// The whole section is read the same way for MySQL and for MariaDB, so
    /// one set of keys serves both.
    fn mysql_value(&self, key: &str) -> Option<&Value> {
        self.database
            .get("mysql")
            .and_then(|section| section.get(key))
    }

    /// `database.yml`'s `mysql.host`, defaulting to `"127.0.0.1"`.
    #[must_use]
    pub fn mysql_host(&self) -> String {
        self.mysql_value("host")
            .and_then(Value::as_str)
            .unwrap_or("127.0.0.1")
            .to_string()
    }

    /// `database.yml`'s `mysql.port`, defaulting to `3306`.
    pub fn mysql_port(&self) -> Result<u16, String> {
        match self.mysql_value("port") {
            None => Ok(3306),
            Some(value) => value
                .as_i64()
                .and_then(|p| u16::try_from(p).ok())
                .filter(|p| *p != 0)
                .ok_or_else(|| "mysql.port must be between 1 and 65535".into()),
        }
    }

    /// `database.yml`'s `mysql.user`, defaulting to `"root"`.
    #[must_use]
    pub fn mysql_user(&self) -> String {
        self.mysql_value("user")
            .and_then(Value::as_str)
            .unwrap_or("root")
            .to_string()
    }

    /// `database.yml`'s `mysql.password`, defaulting to nothing.
    #[must_use]
    pub fn mysql_password(&self) -> String {
        self.mysql_value("password")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }

    /// `database.yml`'s `mysql.database`, defaulting to `"modernhome"`.
    #[must_use]
    pub fn mysql_database(&self) -> String {
        self.mysql_value("database")
            .and_then(Value::as_str)
            .unwrap_or("modernhome")
            .to_string()
    }

    /// `database.yml`'s `mysql.table`, defaulting to `"modernhome_homes"`.
    #[must_use]
    pub fn mysql_table(&self) -> String {
        self.mysql_value("table")
            .and_then(Value::as_str)
            .unwrap_or("modernhome_homes")
            .to_string()
    }

    /// `database.yml`'s `sqlite.file`, defaulting to `"homes.db"`.
    #[must_use]
    pub fn sqlite_file(&self) -> String {
        self.database
            .get("sqlite")
            .and_then(|sqlite| sqlite.get("file"))
            .and_then(Value::as_str)
            .unwrap_or("homes.db")
            .to_string()
    }

    /// The message `key` renders as, with the prefix already prepended.
    ///
    /// A key that is missing answers itself, exactly as
    /// `MessagesConfig.raw`'s `getString(key, key)` did, so a typo shows up in
    /// game instead of vanishing.
    #[must_use]
    pub fn message(&self, key: &str) -> String {
        let raw = self
            .messages
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                // Newly added messages work with existing configuration files.
                parse_or_empty(defaults::MESSAGES)?
                    .get(key)?
                    .as_str()
                    .map(str::to_string)
            })
            .unwrap_or_else(|| key.to_string());
        let prefix = self
            .messages
            .get("prefix")
            .and_then(Value::as_str)
            .unwrap_or("");
        if prefix.is_empty() {
            raw
        } else {
            format!("{prefix}{raw}")
        }
    }

    /// The sound `key` names, and the volume and pitch it is played at.
    ///
    /// `None` when the section or its `sound:` key is absent, which is what
    /// `SoundsConfig.playSound`'s two early returns did.
    #[must_use]
    pub fn sound(&self, key: &str) -> Option<(String, f32, f32)> {
        let section = self.sounds.get(key)?;
        let name = section.get("sound")?.as_str()?.to_string();
        let volume = number_at(section, "volume").unwrap_or(1.0) as f32;
        let pitch = number_at(section, "pitch").unwrap_or(1.0) as f32;
        Some((name, volume, pitch))
    }

    /// `gui/homes.yml`'s `title`, with `%page%`/`%player%`/`%target%` still in it.
    #[must_use]
    pub fn homes_title(&self) -> String {
        string_at(&self.homes_gui, "title").unwrap_or_else(|| "Homes Page: %page%".to_string())
    }

    /// `enabled-multiple-pages`.
    #[must_use]
    pub fn multiple_pages(&self) -> bool {
        self.homes_gui
            .get("enabled-multiple-pages")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    /// `home_rename`, the chat prompt for a right-clicked home.
    #[must_use]
    pub fn rename_prompt(&self) -> String {
        string_at(&self.homes_gui, "home_rename")
            .unwrap_or_else(|| "&7Type new the home name in chat".to_string())
    }

    /// `home_rename_actionbar`, the same prompt as an actionbar.
    #[must_use]
    pub fn rename_prompt_actionbar(&self) -> String {
        string_at(&self.homes_gui, "home_rename_actionbar")
            .unwrap_or_else(|| "&7Type new the home name in chat".to_string())
    }

    /// `items.previous-page-slot`.
    #[must_use]
    pub fn previous_page_slot(&self) -> u32 {
        self.homes_gui
            .get("items")
            .and_then(|items| items.get("previous-page-slot"))
            .and_then(Value::as_u64)
            .map_or(DEFAULT_PREVIOUS_SLOT, |value| value as u32)
    }

    /// `items.next-page-slot`.
    #[must_use]
    pub fn next_page_slot(&self) -> u32 {
        self.homes_gui
            .get("items")
            .and_then(|items| items.get("next-page-slot"))
            .and_then(Value::as_u64)
            .map_or(DEFAULT_NEXT_SLOT, |value| value as u32)
    }

    /// `gui/confirm.yml`'s `title`. It takes no placeholders.
    #[must_use]
    pub fn confirm_title(&self) -> String {
        string_at(&self.confirm_gui, "title").unwrap_or_else(|| "Confirm Delete".to_string())
    }

    /// The parsed `gui/homes.yml` tree, for the screen builders.
    #[must_use]
    pub fn homes_gui(&self) -> &Value {
        &self.homes_gui
    }

    /// Replaces one section, for a test that needs a value the shipped file does
    /// not have.
    ///
    /// The fields are private so nothing but the accessors can read them, and the
    /// layouts are *derived* from two of them at construction time - so a test that
    /// replaced `layout` would not see a re-derived layout. That is why the tests
    /// that need a different `enabled-multiple-pages` replace `main` (which is
    /// read per call) rather than the layouts.
    #[cfg(test)]
    pub(crate) fn replace(&mut self, section: Section, value: Value) {
        match section {
            Section::Main => self.main = value,
            Section::Messages => self.messages = value,
            Section::Sounds => self.sounds = value,
            Section::HomesGui => self.homes_gui = value,
            Section::ConfirmGui => self.confirm_gui = value,
            Section::Server => self.server = value,
            Section::Database => self.database = value,
        }
    }

    /// The parsed `gui/confirm.yml` tree, for the screen builders.
    #[must_use]
    pub fn confirm_gui(&self) -> &Value {
        &self.confirm_gui
    }

    /// The homes screen layout.
    #[must_use]
    pub fn homes_layout(&self) -> &Layout {
        &self.homes_layout
    }

    /// The confirm screen layout.
    #[must_use]
    pub fn confirm_layout(&self) -> &ConfirmLayout {
        &self.confirm_layout
    }
}

/// Which of the seven files a [`Config::replace`] swaps out.
///
/// Only the two sections a test actually needs to change are constructed; the rest
/// are here so the enum names every file, which is the point of naming them rather
/// than passing an index. `#[allow(dead_code)]` rather than four more tests that
/// would each assert a field assignment and none of the behaviour.
#[cfg(test)]
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub(crate) enum Section {
    Main,
    Messages,
    Sounds,
    HomesGui,
    ConfirmGui,
    Server,
    Database,
}

/// Reads every file in `folder`, writing the shipped default first when absent.
///
/// The folder is the guest-visible `data` path the host preopened, so this is
/// plain `std::fs`. `guis` is the sub-directory the two screen files live in.
pub fn load(folder: &str) -> Loaded {
    let mut problems = Vec::new();
    let mut read = |name: &str, default: &'static str| -> Value {
        let path = std::path::Path::new(folder).join(name);
        let text = match read_config(&path, default) {
            Ok(text) => text,
            Err(error) => {
                problems.push(format!("{name}: could not be read or created ({error})"));
                default.to_string()
            }
        };
        match parse_or_empty(&text) {
            Some(value) => value,
            None => {
                // Deliberately not re-written: the admin's edits stay on disk so
                // they can be fixed, and the plugin runs on the shipped default
                // rather than on whatever a half-parsed file happened to yield.
                problems.push(format!("{name}: could not be parsed"));
                parse_or_empty(default).unwrap_or(Value::Null)
            }
        }
    };

    let homes_gui = read("gui/homes.yml", defaults::HOMES_GUI);
    let confirm_gui = read("gui/confirm.yml", defaults::CONFIRM_GUI);
    let main = read("config.yml", defaults::CONFIG);
    let messages = read("messages.yml", defaults::MESSAGES);
    let sounds = read("sounds.yml", defaults::SOUNDS);
    let server = read("server.yml", defaults::SERVER);
    let database = read("database.yml", defaults::DATABASE);

    Loaded {
        value: Config {
            homes_layout: Layout::homes(&string_list(&homes_gui, "layout")),
            confirm_layout: ConfirmLayout::parse(&string_list(&confirm_gui, "gui-disposition")),
            main,
            messages,
            sounds,
            homes_gui,
            confirm_gui,
            server,
            database,
        },
        problems,
    }
}

fn read_config(path: &std::path::Path, default: &str) -> std::io::Result<String> {
    use std::io::Write;
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(mut file) => {
                    file.write_all(default.as_bytes())?;
                    Ok(default.to_string())
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    std::fs::read_to_string(path)
                }
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

/// Parses YAML, mapping empty input to an empty mapping rather than to `Null`.
fn parse_or_empty(text: &str) -> Option<Value> {
    if text.trim().is_empty() {
        return Some(Value::Mapping(serde_norway::Mapping::new()));
    }
    serde_norway::from_str(text).ok()
}

/// `root[key]` as a string.
fn string_at(root: &Value, key: &str) -> Option<String> {
    root.get(key).and_then(Value::as_str).map(str::to_string)
}

/// `root[key]` as a list of strings, dropping anything that is not a string.
fn string_list(root: &Value, key: &str) -> Vec<String> {
    root.get(key)
        .and_then(Value::as_sequence)
        .map(|sequence| {
            sequence
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// A scalar at `root[key]`, read as an `f64`.
///
/// YAML `volume: 1` is an integer, and Java read it with `getDouble`, so the
/// integer-to-double widening is done here rather than being left to fail.
fn number_at(root: &Value, key: &str) -> Option<f64> {
    root.get(key).and_then(Value::as_f64)
}

/// The key order a config value is read in, so a test can assert defaults.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreadable_configuration_is_preserved_and_reported() {
        let folder = std::env::temp_dir().join(format!(
            "modernhome-config-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("config.yml");
        let invalid = [0xff, 0xfe, 0x41];
        std::fs::write(&path, invalid).unwrap();
        let loaded = load(folder.to_str().unwrap());
        assert!(!loaded.clean());
        assert!(
            loaded
                .problems
                .iter()
                .any(|problem| problem.starts_with("config.yml:"))
        );
        assert_eq!(std::fs::read(&path).unwrap(), invalid);
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn new_failure_messages_work_with_older_configuration() {
        let mut config = Config::defaults();
        config.replace(
            Section::Messages,
            parse_or_empty("prefix: ''\nhome-set: 'Custom text'\n").unwrap(),
        );
        assert_eq!(config.message("home-set"), "Custom text");
        assert!(
            config
                .message("home-rename-fail")
                .contains("Failed to save")
        );
        assert!(
            config
                .message("teleport-world-ambiguous")
                .contains("dimension")
        );
        assert_eq!(config.message("unknown-key"), "unknown-key");
    }

    #[test]
    fn the_shipped_files_are_valid_yaml() {
        // Every embedded default has to parse, because it is what an admin gets
        // when their own copy is broken, and a broken default would be a plugin
        // that cannot start.
        for text in [
            defaults::CONFIG,
            defaults::MESSAGES,
            defaults::SOUNDS,
            defaults::SERVER,
            defaults::DATABASE,
            defaults::HOMES_GUI,
            defaults::CONFIRM_GUI,
        ] {
            assert!(
                parse_or_empty(text).is_some(),
                "a shipped default does not parse"
            );
        }
    }

    #[test]
    fn the_shipped_defaults_are_the_values_the_java_code_defaulted_to() {
        let config = Config::defaults();

        assert_eq!(config.teleport_time(), 5);
        assert_eq!(config.combat_delay(), 5);
        assert!(config.allow_chat());
        assert!(config.allow_actionbar());
        assert_eq!(config.blacklisted_worlds(), vec!["spawn".to_string()]);
        assert!(config.is_blacklisted("spawn"));
        assert!(!config.is_blacklisted("world"));
        assert_eq!(config.server_id(), "server-1");
        assert_eq!(config.store_file(), "homes.json");
        assert_eq!(config.previous_page_slot(), 45);
        assert_eq!(config.next_page_slot(), 53);
        assert_eq!(config.homes_title(), "Homes • Page %page%");
        assert_eq!(config.confirm_title(), "Delete home?");
        assert!(config.multiple_pages());
    }

    #[test]
    fn a_missing_key_defaults_rather_than_failing() {
        // Java: `getInt("teleport-time", 5)`. An admin who deletes the key must
        // get 5, not a crash.
        let config = Config::defaults();
        assert_eq!(config.teleport_time(), 5);
        assert_eq!(config.limit_scan_max(), 256);
    }

    #[test]
    fn an_empty_config_defaults_to_the_paper_values() {
        // `Config` built from an empty tree: the same fallbacks have to hold, or a
        // truncated file would run with a zero countdown.
        let empty: Config = Config {
            main: Value::Mapping(Default::default()),
            messages: Value::Mapping(Default::default()),
            sounds: Value::Mapping(Default::default()),
            homes_gui: Value::Mapping(Default::default()),
            confirm_gui: Value::Mapping(Default::default()),
            server: Value::Mapping(Default::default()),
            database: Value::Mapping(Default::default()),
            homes_layout: Layout::parse(&[]),
            confirm_layout: ConfirmLayout::parse(&[]),
        };
        assert_eq!(empty.teleport_time(), 5);
        assert_eq!(empty.server_id(), "server-1");
        assert_eq!(empty.store_file(), "homes.json");
        assert!(empty.allow_chat());
        assert!(empty.blacklisted_worlds().is_empty());
    }

    #[test]
    fn a_missing_message_key_answers_itself() {
        // The one diagnostic the Paper plugin left in place on purpose: a typo'd
        // key is visible in game as the key's own name.
        let config = Config::defaults();
        assert_eq!(config.message("home-set"), prefix() + "&aHome saved.");
        assert_eq!(
            config.message("no-such-key"),
            format!("{}no-such-key", prefix())
        );
    }

    #[test]
    fn the_prefix_is_prepended_exactly_once() {
        // `getString(key, key)` read the body, the prefix went on the front, and
        // the key name was *not* used as a fallback because the key exists.
        let config = Config::defaults();
        let prefix = prefix();
        assert_eq!(config.message("home-set"), format!("{prefix}&aHome saved."));
        assert!(
            config.message("home-set").starts_with(&prefix),
            "the prefix leads, once"
        );
    }

    #[test]
    fn a_sound_section_needs_its_name() {
        let config = Config::defaults();
        let (name, volume, pitch) = config.sound("error").expect("error ships");
        assert_eq!(name, "ENTITY_VILLAGER_NO");
        assert_eq!(volume, 1.0);
        assert_eq!(pitch, 1.0);
        // A key that is not in the file at all is a no-op, which is what the two
        // early returns in SoundsConfig.playSound did.
        assert_eq!(config.sound("no-such-sound"), None);
    }

    #[test]
    fn a_sound_volume_rounded_in_yaml_is_read_as_a_number() {
        // `volume: 1` is an integer in YAML and Java read it with getDouble.
        let yaml = "ui-open:\n  sound: X\n  volume: 1\n  pitch: 2\n";
        let config = with(yaml, "sounds");
        let (_, volume, pitch) = config.sound("ui-open").expect("parses");
        assert_eq!(volume, 1.0);
        assert_eq!(pitch, 2.0);
    }

    #[test]
    fn a_layout_from_the_file_is_the_shipped_one() {
        let config = Config::defaults();
        assert_eq!(
            config.homes_layout().home_slots,
            vec![11, 13, 15, 28, 30, 32, 34]
        );
        assert_eq!(config.confirm_layout().rows, 3);
    }

    #[test]
    fn a_blacklist_entry_must_match_exactly() {
        let yaml = "blacklisted-worlds:\n  - \"spawn\"\n";
        let config = with(yaml, "main");
        assert!(config.is_blacklisted("spawn"));
        // Not case-insensitive, and not a prefix: both were exact in Java.
        assert!(!config.is_blacklisted("Spawn"));
        assert!(!config.is_blacklisted("spawn_world"));
    }

    /// Builds a config whose `main`/`sounds` section is `yaml`, everything else
    /// the shipped default.
    fn with(yaml: &str, slot: &str) -> Config {
        let mut config = Config::defaults();
        let parsed = parse_or_empty(yaml).expect("test yaml parses");
        match slot {
            "main" => config.main = parsed,
            "sounds" => config.sounds = parsed,
            "database" => config.database = parsed,
            _ => unreachable!("unknown slot"),
        }
        config
    }

    /// The shipped prefix, so a test does not hard-code it.
    fn prefix() -> String {
        let messages = parse_or_empty(defaults::MESSAGES).expect("ships");
        messages
            .get("prefix")
            .and_then(Value::as_str)
            .expect("prefix ships")
            .to_string()
    }

    #[test]
    fn the_json_store_is_the_default() {
        let config = Config::defaults();
        assert_eq!(config.storage(), Storage::Json);
        assert_eq!(config.store_file(), "homes.json");
        assert_eq!(config.sqlite_file(), "homes.db");
    }

    #[test]
    fn the_sqlite_store_is_chosen_by_name() {
        let config = with("storage: sqlite", "database");
        assert_eq!(config.storage(), Storage::Sqlite);
    }

    #[test]
    fn an_unknown_store_name_is_refused() {
        let config = with("storage: sqlited", "database");
        assert_eq!(config.storage(), Storage::Invalid);
    }

    #[test]
    fn mysql_and_mariadb_choose_the_same_backend() {
        for name in ["mysql", "mariadb"] {
            assert_eq!(
                with(&format!("storage: {name}"), "database").storage(),
                Storage::MySql
            );
        }
        for port in ["0", "-1", "65536", "wrong"] {
            assert!(
                with(&format!("mysql:\n  port: {port}"), "database")
                    .mysql_port()
                    .is_err()
            );
        }
        assert_eq!(Config::defaults().mysql_port().unwrap(), 3306);
    }

    #[test]
    fn the_sqlite_file_can_be_named() {
        let config = with("sqlite:\n  file: homes.sqlite3", "database");
        assert_eq!(config.sqlite_file(), "homes.sqlite3");
    }
}
