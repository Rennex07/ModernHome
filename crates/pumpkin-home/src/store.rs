//! The homes, on disk.
//!
//! ## Why one JSON file
//!
//! The Paper original kept homes in SQLite or MySQL. A Pumpkin plugin is a
//! WebAssembly component inside a WASI sandbox that is granted exactly one
//! directory - its own data folder - and neither a SQLite driver nor a MySQL
//! client can be linked into a component. There is no in-process database
//! either: the plugin API has no storage interface beyond `std::fs`.
//!
//! So the homes live in one JSON file, written back whole after every change.
//! The shape is the row shape the `player_homes` table had, one key per player
//! uuid, so a store written by this plugin is readable by eye.
//!
//! ## There is no separate cache
//!
//! `HomeManager` kept a 60-second TTL cache, because the SQL table was shared
//! with every other server on the proxy and could change underneath it. Nothing
//! else writes this file, so the in-memory map *is* the cache and there is
//! nothing for it to be stale against. That is also why a port can answer
//! `/sethome`'s duplicate scan synchronously, where the Paper plugin had to open
//! a connection on the main thread to do it.
//!
//! ## A file that fails to parse is refused, not repaired
//!
//! [`Store::load`] keeps the previous - possibly empty - state when the file
//! cannot be read. Saving over a file the plugin did not understand would
//! destroy the very data it failed to parse, which is the one loss an admin
//! cannot undo.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tracing::error;

use crate::homes::{Home, PlayerHomes};

/// The file's whole content: one entry per player uuid.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct File {
    /// Every player's homes, keyed by uuid string.
    #[serde(default)]
    pub players: BTreeMap<String, PlayerHomes>,
}

/// The homes, and the file they live in.
#[derive(Debug, Default)]
pub struct Store {
    players: BTreeMap<String, PlayerHomes>,
    /// Whether anything has changed since the last [`Store::save`].
    dirty: bool,
    /// Whether the last [`Store::load`] failed, which blocks writes.
    problem: bool,
    path: PathBuf,
}

impl Store {
    /// Binds a store to `path` without reading it.
    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self {
            players: BTreeMap::new(),
            dirty: false,
            problem: false,
            path: path.into(),
        }
    }

    /// Reads the file, keeping the current state if it cannot be read.
    ///
    /// An absent file is not an error: the plugin has simply never run here.
    pub fn load(&mut self) {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.problem = false;
                return;
            }
            Err(error) => {
                error!(
                    "could not read {}: {error}; the homes in memory are kept",
                    self.path.display()
                );
                self.problem = true;
                return;
            }
        };

        match serde_json::from_str::<File>(&text) {
            Ok(file) => {
                self.players = file.players;
                self.problem = false;
            }
            Err(error) => {
                // The old content is abandoned but the file is left exactly as it
                // is, so nothing is lost and an admin can still look at it.
                error!(
                    "{} is not valid ({error}); it was left as written",
                    self.path.display()
                );
                self.problem = true;
            }
        }
    }

    /// Writes the file when anything has changed.
    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        if self.problem {
            error!(
                "{} is unreadable, so it is not being overwritten",
                self.path.display()
            );
            return;
        }
        let content = File {
            players: self.players.clone(),
        };
        match serde_json::to_string_pretty(&content) {
            Ok(text) => {
                if let Err(error) = std::fs::write(&self.path, text) {
                    error!("could not write {}: {error}", self.path.display());
                }
            }
            Err(error) => error!("could not serialize the homes: {error}"),
        }
    }

    /// The player's homes, borrowing them in place.
    ///
    /// Returns an empty `PlayerHomes` for a player who has none, which is what
    /// the Java `homeCache.get(uuid)` returned after a first load - and, more to
    /// the point, what `homeCache.get` returned for a uuid it did not hold, which
    /// is a `null` that the caller never had to check.
    pub fn with_player<T>(&self, uuid: &str, body: impl FnOnce(&PlayerHomes) -> T) -> T {
        match self.players.get(uuid) {
            Some(found) => body(found),
            None => body(empty_homes()),
        }
    }

    /// The same, with the homes mutable.
    ///
    /// The `dirty` flag goes up on *every* call rather than only when the body
    /// changes something. A save after a read-only call therefore rewrites the
    /// file with identical content, which is harmless and is the price of not
    /// asking the body whether it wrote.
    pub fn with_player_mut<T>(
        &mut self,
        uuid: &str,
        body: impl FnOnce(&mut PlayerHomes) -> T,
    ) -> T {
        self.dirty = true;
        body(self.players.entry(uuid.to_string()).or_default())
    }

    /// Reads a player's homes, or `None` when they have none.
    #[must_use]
    pub fn homes(&self, uuid: &str) -> Option<&PlayerHomes> {
        self.players.get(uuid)
    }

    /// How many players have homes, for the startup line.
    #[must_use]
    pub fn player_count(&self) -> usize {
        self.players.len()
    }

    /// Whether the file could not be read, so writes are blocked.
    #[must_use]
    pub fn has_problem(&self) -> bool {
        self.problem
    }

    /// Adds or replaces one home and reports nothing - there is nothing to
    /// report, because an insert and an update are the same operation.
    pub fn set_home(&mut self, uuid: &str, home: Home) {
        self.with_player_mut(uuid, |player| player.insert(home));
    }

    /// Deletes one home and reports whether it was there.
    ///
    /// `HomeManager.deleteHome` returned `false` on a zero-row UPDATE so the
    /// confirmation screen could tell "nothing happened" apart from "done".
    pub fn delete_home(&mut self, uuid: &str, number: i32) -> bool {
        let deleted = self.with_player_mut(uuid, |player| player.delete(number));
        if deleted {
            // Drop a player who has just lost their last home, so the file does
            // not grow one stale empty object per player who ever set one.
            if self
                .players
                .get(uuid)
                .is_some_and(|player| player.homes.is_empty())
            {
                self.players.remove(uuid);
            }
        }
        deleted
    }

    /// Renames one home, reporting whether it existed.
    ///
    /// `HomeManager.renameHome` used an `UPDATE ... SET name = ?`, so a home that
    /// is not there yields `false` and no message.
    pub fn rename_home(&mut self, uuid: &str, number: i32, name: &str) -> bool {
        self.with_player_mut(uuid, |player| {
            match player.homes.iter_mut().find(|home| home.home_number == number) {
                Some(home) => {
                    home.name = (!name.trim().is_empty()).then(|| name.trim().to_string());
                    true
                }
                None => false,
            }
        })
    }

    /// The slot numbers a player has taken, ascending.
    #[must_use]
    pub fn home_numbers(&self, uuid: &str) -> Vec<i32> {
        self.homes(uuid).map(PlayerHomes::numbers).unwrap_or_default()
    }

    /// Inserts a batch of rows, ignoring any that already exist.
    ///
    /// `INSERT OR IGNORE` / `INSERT IGNORE`, which is what made the Essentials
    /// migration re-runnable. An already-set slot is left alone, because the
    /// Paper plugin never overwrote one on import.
    pub fn insert_batch(&mut self, uuid: &str, rows: impl IntoIterator<Item = Home>) {
        self.with_player_mut(uuid, |player| {
            for row in rows {
                if player.find(row.home_number).is_none() {
                    player.insert(row);
                }
            }
        });
    }
}

/// The empty set of homes, for a player who has none.
///
/// A `OnceLock` rather than a bare `static EMPTY: PlayerHomes`, because
/// `PlayerHomes` is not `const`-constructible and a shared mutable static would be
/// wrong even if it were.
fn empty_homes() -> &'static PlayerHomes {
    static EMPTY: OnceLock<PlayerHomes> = OnceLock::new();
    EMPTY.get_or_init(PlayerHomes::default)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch store in a directory that is cleaned up afterwards.
    fn store(dir: &std::path::Path) -> Store {
        Store::at(dir.join("homes.json"))
    }

    fn temp() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "pumpkin-home-store-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("temp dir");
        path
    }

    fn home(number: i32) -> Home {
        Home::new(
            number,
            Some("world".to_string()),
            1.0,
            2.0,
            3.0,
            0.0,
            0.0,
            None,
            None,
        )
    }

    #[test]
    fn an_absent_file_is_not_an_error() {
        let dir = temp();
        let mut store = store(&dir);
        store.load();
        assert!(!store.has_problem());
        assert_eq!(store.player_count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_home_survives_a_round_trip() {
        let dir = temp();
        let mut first = store(&dir);
        first.set_home("uuid-a", home(1));
        first.save();

        let mut second = store(&dir);
        second.load();
        assert_eq!(second.home_numbers("uuid-a"), vec![1]);
        let reads = second.with_player("uuid-a", |player| player.find(1).cloned());
        assert_eq!(reads, Some(home(1)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_is_written_until_something_changes() {
        let dir = temp();
        let mut store = store(&dir);
        store.set_home("uuid-a", home(1));
        store.save();
        let after_first = std::fs::read_to_string(store.path.clone()).expect("written");

        // A no-op read must not touch the file, or every save would rewrite it
        // and an mtime-based backup would fire on nothing.
        store.with_player("uuid-a", |_| ());
        store.save();
        assert_eq!(std::fs::read_to_string(store.path.clone()).expect("still there"), after_first);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_file_is_left_alone_and_blocks_writes() {
        let dir = temp();
        let path = dir.join("homes.json");
        std::fs::write(&path, "{ not json").expect("written");
        let mut store = store(&dir);
        store.load();

        assert!(store.has_problem(), "a refused parse is reported");
        // An insert is refused rather than saved over the top of it.
        store.set_home("uuid-a", home(1));
        store.save();
        assert_eq!(std::fs::read_to_string(&path).expect("untouched"), "{ not json");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deleting_the_last_home_drops_the_player() {
        // Otherwise the file grows one empty object for every player who ever
        // set a home, forever.
        let dir = temp();
        let mut store = store(&dir);
        store.set_home("uuid-a", home(1));
        assert!(store.delete_home("uuid-a", 1));
        assert_eq!(store.player_count(), 0);
        assert!(!store.delete_home("uuid-a", 1), "nothing to delete twice");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rename_reports_whether_the_home_was_there() {
        let dir = temp();
        let mut store = store(&dir);
        store.set_home("uuid-a", home(2));
        assert!(store.rename_home("uuid-a", 2, "  base  "));
        assert_eq!(
            store.homes("uuid-a").and_then(|p| p.find(2)).and_then(|h| h.name.clone()),
            Some("base".to_string())
        );
        assert!(!store.rename_home("uuid-a", 9, "nope"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_blank_rename_clears_the_name_rather_than_storing_blanks() {
        // Java's Home constructor collapsed "", "   " and null to null, so the
        // GUI's `%home%` rendered empty. Storing "  " would show a space.
        let dir = temp();
        let mut store = store(&dir);
        store.set_home("uuid-a", home(1));
        assert!(store.rename_home("uuid-a", 1, "   "));
        assert_eq!(
            store.homes("uuid-a").and_then(|p| p.find(1)).expect("home").name,
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_insert_ignores_a_slot_that_is_taken() {
        // `INSERT OR IGNORE`: the migration is re-runnable and must not clobber a
        // home the player set after it ran.
        let dir = temp();
        let mut store = store(&dir);
        store.set_home("uuid-a", home(3));
        store.insert_batch("uuid-a", [home(1), home(3)]);
        assert_eq!(store.home_numbers("uuid-a"), vec![1, 3]);
        assert_eq!(
            store.homes("uuid-a").and_then(|p| p.find(3)).map(|h| h.x),
            Some(1.0),
            "the original row's coordinates are unchanged"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unknown_player_reads_as_empty_not_a_panic() {
        let dir = temp();
        let store = store(&dir);
        assert_eq!(store.home_numbers("nobody"), Vec::<i32>::new());
        let count = store.with_player("nobody", |player| player.homes.len());
        assert_eq!(count, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_player_read_more_than_once_gets_the_same_empty_set() {
        // `with_player` must not create an entry as a side effect, or a tab
        // completion would grow the file by a name.
        let dir = temp();
        let store = store(&dir);
        store.with_player("nobody", |_| ());
        assert_eq!(store.player_count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
