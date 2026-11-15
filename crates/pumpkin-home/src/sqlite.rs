//! The homes, in a SQLite database.
//!
//! One row per home, keyed by the player's uuid and the slot the home occupies,
//! which is the shape the Paper plugin's `player_homes` table had.
//!
//! ## Only the players that changed are written
//!
//! A save rewrites the rows of the players whose homes changed, in one
//! transaction, and leaves every other row exactly as it was. A player who has
//! just lost their last home has their rows deleted, so the database does not
//! grow one stale row per player who ever set a home.
//!
//! ## The transaction is the atomicity
//!
//! A reader either sees the old rows or the new ones. There is no window in
//! which half of a player's homes are written, which is the same guarantee the
//! JSON backend gets from writing a temporary file and renaming it over the
//! old one.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::homes::{Home, PlayerHomes};

/// The schema the plugin writes.
///
/// The table is created if it is not there, so a fresh database and one that an
/// older build wrote are the same thing to this code. `home_number` is part of
/// the primary key, so one player cannot hold the same slot twice - which is
/// the rule the in-memory store keeps as well.
const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS player_homes (
        uuid        TEXT    NOT NULL,
        home_number INTEGER NOT NULL,
        world       TEXT,
        dimension   TEXT,
        x           REAL    NOT NULL,
        y           REAL    NOT NULL,
        z           REAL    NOT NULL,
        yaw         REAL    NOT NULL,
        pitch       REAL    NOT NULL,
        name        TEXT,
        server_id   TEXT,
        PRIMARY KEY (uuid, home_number)
    );
";

/// An open SQLite database holding the homes.
#[derive(Debug)]
pub struct Sqlite {
    /// The file the database lives in, for the error messages.
    path: PathBuf,
    /// The connection the homes are read from and written to.
    connection: Connection,
}

impl Sqlite {
    /// Opens the database at `path`, creating the schema if it is not there.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be opened or the schema cannot be written.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let connection = Connection::open(path)?;
        connection.execute_batch(SCHEMA)?;
        Ok(Self {
            path: path.to_path_buf(),
            connection,
        })
    }

    /// The file the database lives in.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads every home, grouped by the player they belong to.
    ///
    /// # Errors
    ///
    /// Fails when the table cannot be read.
    pub fn read_all(&self) -> rusqlite::Result<BTreeMap<String, PlayerHomes>> {
        let mut statement = self.connection.prepare(
            "SELECT uuid, home_number, world, dimension, x, y, z, yaw, pitch, name, server_id
             FROM player_homes
             ORDER BY uuid, home_number",
        )?;
        let mut rows = statement.query([])?;
        let mut players: BTreeMap<String, PlayerHomes> = BTreeMap::new();

        while let Some(row) = rows.next()? {
            let uuid: String = row.get(0)?;
            let home = Home {
                home_number: row.get(1)?,
                world: row.get(2)?,
                dimension: row.get(3)?,
                x: row.get(4)?,
                y: row.get(5)?,
                z: row.get(6)?,
                yaw: row.get(7)?,
                pitch: row.get(8)?,
                name: row.get(9)?,
                server_id: row.get(10)?,
            };
            players.entry(uuid).or_default().insert(home);
        }

        Ok(players)
    }

    /// Writes the homes of the players in `touched`, and only those.
    ///
    /// A player who is no longer in `players` has their rows deleted, which is
    /// how a player who has lost their last home stops having any.
    ///
    /// # Errors
    ///
    /// Fails when the transaction cannot be written or committed. Nothing is
    /// written in that case: the transaction is rolled back.
    pub fn write(
        &mut self,
        touched: &BTreeSet<String>,
        players: &BTreeMap<String, PlayerHomes>,
    ) -> rusqlite::Result<()> {
        let transaction = self.connection.transaction()?;

        for uuid in touched {
            transaction.execute("DELETE FROM player_homes WHERE uuid = ?1", [uuid])?;
            let Some(player) = players.get(uuid) else {
                continue;
            };
            for home in &player.homes {
                transaction.execute(
                    "INSERT INTO player_homes
                        (uuid, home_number, world, dimension, x, y, z, yaw, pitch, name, server_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                    rusqlite::params![
                        uuid,
                        home.home_number,
                        &home.world,
                        &home.dimension,
                        home.x,
                        home.y,
                        home.z,
                        home.yaw,
                        home.pitch,
                        &home.name,
                        &home.server_id,
                    ],
                )?;
            }
        }

        transaction.commit()
    }
}
