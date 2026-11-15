//! The homes, in a MySQL or MariaDB database.
//!
//! One row per home, keyed by the player's uuid and the slot the home
//! occupies - the same shape the Paper plugin's `player_homes` table had, and
//! the same shape the SQLite backend uses.
//!
//! Only changed players are written, in one InnoDB transaction.

use std::collections::{BTreeMap, BTreeSet};

use crate::homes::{Home, PlayerHomes};
use crate::mysql::{Client, Error, quote};

/// The columns the homes are stored in, in the order the rows are read.
const COLUMNS: [&str; 11] = [
    "uuid",
    "home_number",
    "world",
    "dimension",
    "x",
    "y",
    "z",
    "yaw",
    "pitch",
    "name",
    "server_id",
];

/// An open MySQL or MariaDB database holding the homes.
#[derive(Debug)]
pub struct MySql {
    /// The connection the homes are read from and written to.
    client: Client,
    /// The table the homes live in.
    table: String,
}

impl MySql {
    /// Opens the database and creates the table if it is not there.
    ///
    /// # Errors
    ///
    /// Fails when the server cannot be reached, when it refuses the
    /// credentials, or when the table cannot be created.
    pub fn open(
        host: &str,
        port: u16,
        user: &str,
        password: &str,
        database: &str,
        table: &str,
    ) -> Result<Self, Error> {
        let table = identifier(table)?;
        let mut client = Client::connect(host, port, user, password, database)?;
        client.execute(&create_table(&table))?;
        Ok(Self { client, table })
    }

    /// Reads every home, grouped by the player they belong to.
    ///
    /// # Errors
    ///
    /// Fails when the query cannot be run or the rows cannot be read.
    pub fn read_all(&mut self) -> Result<BTreeMap<String, PlayerHomes>, Error> {
        let sql = format!(
            "SELECT {} FROM `{}` ORDER BY uuid, home_number",
            COLUMNS.join(", "),
            self.table
        );
        let rows = self.client.query(&sql)?;
        let mut players: BTreeMap<String, PlayerHomes> = BTreeMap::new();

        for row in rows {
            if row.len() != COLUMNS.len() {
                return Err(Error::Protocol("invalid home row column count".into()));
            }
            let uuid = row[0]
                .clone()
                .ok_or_else(|| Error::Protocol("NULL home UUID".into()))?;
            let home = Home {
                home_number: number(&row[1])?,
                world: row[2].clone(),
                dimension: row[3].clone(),
                x: number(&row[4])?,
                y: number(&row[5])?,
                z: number(&row[6])?,
                yaw: number(&row[7])?,
                pitch: number(&row[8])?,
                name: row[9].clone(),
                server_id: row[10].clone(),
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
    /// Fails when a query cannot be run. Failed transactions are rolled back.
    /// If the connection breaks during COMMIT, its outcome is unknown.
    pub fn write(
        &mut self,
        touched: &BTreeSet<String>,
        players: &BTreeMap<String, PlayerHomes>,
    ) -> Result<(), Error> {
        self.client.execute("START TRANSACTION")?;
        let result = self.write_rows(touched, players);
        if let Err(error) = result {
            let _ = self.client.execute("ROLLBACK");
            return Err(error);
        }
        self.client.execute("COMMIT")
    }

    fn write_rows(
        &mut self,
        touched: &BTreeSet<String>,
        players: &BTreeMap<String, PlayerHomes>,
    ) -> Result<(), Error> {
        for uuid in touched {
            let delete = format!("DELETE FROM `{}` WHERE uuid = {}", self.table, quote(uuid));
            self.client.execute(&delete)?;

            let Some(player) = players.get(uuid) else {
                continue;
            };
            for home in &player.homes {
                let insert = format!(
                    "INSERT INTO `{}` ({}) VALUES ({})",
                    self.table,
                    COLUMNS.join(", "),
                    [
                        quote(uuid),
                        home.home_number.to_string(),
                        nullable(&home.world),
                        nullable(&home.dimension),
                        home.x.to_string(),
                        home.y.to_string(),
                        home.z.to_string(),
                        home.yaw.to_string(),
                        home.pitch.to_string(),
                        nullable(&home.name),
                        nullable(&home.server_id),
                    ]
                    .join(", ")
                );
                self.client.execute(&insert)?;
            }
        }
        Ok(())
    }
}

/// The statement that creates the table.
fn create_table(table: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS `{table}` (
            `uuid` VARCHAR(36) NOT NULL,
            `home_number` INT NOT NULL,
            `world` TEXT,
            `dimension` TEXT,
            `x` DOUBLE NOT NULL DEFAULT 0,
            `y` DOUBLE NOT NULL DEFAULT 0,
            `z` DOUBLE NOT NULL DEFAULT 0,
            `yaw` FLOAT NOT NULL DEFAULT 0,
            `pitch` FLOAT NOT NULL DEFAULT 0,
            `name` TEXT,
            `server_id` TEXT,
            PRIMARY KEY (`uuid`, `home_number`)
        ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4"
    )
}

fn nullable(value: &Option<String>) -> String {
    value.as_deref().map_or_else(|| "NULL".into(), quote)
}

fn number<T: std::str::FromStr>(value: &Option<String>) -> Result<T, Error> {
    value
        .as_deref()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| Error::Protocol("invalid numeric home field".into()))
}

/// Checks a table name and gives it back.
///
/// A table name cannot be a value in a query, so it cannot be escaped the way
/// a value is: it is checked instead. Only letters, digits and underscores
/// are allowed, which is every name an admin would write.
fn identifier(table: &str) -> Result<String, Error> {
    let trimmed = table.trim();
    if trimmed.is_empty() || trimmed.len() > 64 {
        return Err(Error::Protocol("the table name is empty".to_string()));
    }
    if !trimmed
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return Err(Error::Protocol(format!(
            "the table name `{trimmed}` may only hold letters, digits and underscores"
        )));
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::homes::Home;
    use crate::mysql::Row;

    /// Run against a disposable database using MODERNHOME_TEST_MYSQL_* settings.
    #[test]
    #[ignore = "requires a disposable MySQL/MariaDB database"]
    fn real_database_round_trip_and_rollback() {
        let setting = |key: &str| std::env::var(format!("MODERNHOME_TEST_MYSQL_{key}")).expect(key);
        let host = setting("HOST");
        let port = setting("PORT").parse().unwrap();
        let user = setting("USER");
        let password = setting("PASSWORD");
        let database = setting("DATABASE");
        let table = format!("modernhome_test_{}", std::process::id());
        let mut client = Client::connect(&host, port, &user, &password, &database).unwrap();
        let mut store = MySql::open(&host, port, &user, &password, &database, &table).unwrap();
        let mut home = Home::new(
            1,
            Some("world".into()),
            (1.25, 64.0, -4.75),
            (90.0, 1.0),
            Some("O'Brien\\雪🦀\n\0".into()),
            None,
        );
        home.dimension = None;
        let touched = BTreeSet::from(["uuid-test".to_string()]);
        let players = BTreeMap::from([(
            "uuid-test".to_string(),
            PlayerHomes {
                homes: vec![home.clone()],
            },
        )]);
        store.write(&touched, &players).unwrap();
        drop(store);
        let mut store = MySql::open(&host, port, &user, &password, &database, &table).unwrap();
        assert_eq!(
            store.read_all().unwrap()["uuid-test"].homes,
            vec![home.clone()]
        );
        client
            .execute("SET SESSION sql_mode = 'NO_BACKSLASH_ESCAPES'")
            .unwrap();
        let text = "quote' slash\\\0雪";
        assert_eq!(
            client.query(&format!("SELECT {}", quote(text))).unwrap()[0][0].as_deref(),
            Some(text)
        );
        let trigger = format!("{table}_reject");
        client.execute(&format!("CREATE TRIGGER `{trigger}` BEFORE INSERT ON `{table}` FOR EACH ROW SIGNAL SQLSTATE '45000' SET MESSAGE_TEXT = 'test rollback'")).unwrap();
        assert!(store.write(&touched, &players).is_err());
        assert_eq!(store.read_all().unwrap()["uuid-test"].homes, vec![home]);
        client
            .execute(&format!("DROP TRIGGER `{trigger}`"))
            .unwrap();
        store.write(&touched, &BTreeMap::new()).unwrap();
        assert!(store.read_all().unwrap().is_empty());
        client.execute(&format!("DROP TABLE `{table}`")).unwrap();
    }

    /// The rows the mock server answers a `SELECT` with.
    fn rows() -> Vec<Row> {
        vec![vec![
            Some("uuid-a".to_string()),
            Some("1".to_string()),
            Some("world".to_string()),
            Some("minecraft:overworld".to_string()),
            Some("1.5".to_string()),
            Some("64".to_string()),
            Some("-3.25".to_string()),
            Some("90".to_string()),
            Some("0".to_string()),
            Some("base".to_string()),
            Some("server-1".to_string()),
        ]]
    }

    #[test]
    fn the_rows_are_read_into_the_homes() {
        let (host, port, _statements) = crate::mysql::testing::mock_server(rows());
        let mut database = MySql::open(&host, port, "user", "secret", "database", "homes")
            .expect("the database opens");

        let players = database.read_all().expect("the rows are read");
        assert_eq!(players.len(), 1);
        let home = players
            .get("uuid-a")
            .and_then(|homes| homes.find(1))
            .expect("the home is there");
        assert_eq!(home.world.as_deref(), Some("world"));
        assert_eq!(home.dimension.as_deref(), Some("minecraft:overworld"));
        assert_eq!(home.x, 1.5);
        assert_eq!(home.name.as_deref(), Some("base"));
    }

    #[test]
    fn only_the_changed_player_is_written() {
        let (host, port, statements) = crate::mysql::testing::mock_server(rows());
        let mut database = MySql::open(&host, port, "user", "secret", "database", "homes")
            .expect("the database opens");

        let mut players: BTreeMap<String, PlayerHomes> = BTreeMap::new();
        let mut changed = BTreeSet::new();
        changed.insert("uuid-b".to_string());
        players.insert(
            "uuid-b".to_string(),
            PlayerHomes {
                homes: vec![Home::new(
                    2,
                    Some("world".to_string()),
                    (1.0, 2.0, 3.0),
                    (0.0, 0.0),
                    None,
                    None,
                )],
            },
        );

        database
            .write(&changed, &players)
            .expect("the rows are written");

        let seen: Vec<String> = std::iter::from_fn(|| {
            statements
                .recv_timeout(std::time::Duration::from_millis(200))
                .ok()
        })
        .collect();
        assert!(
            seen.iter().any(|sql| sql.contains(&format!(
                "DELETE FROM `homes` WHERE uuid = {}",
                quote("uuid-b")
            ))),
            "the player's rows are deleted first: {seen:?}"
        );
        assert!(
            seen.iter()
                .any(|sql| sql.contains("INSERT INTO `homes`") && sql.contains(&quote("uuid-b"))),
            "the home is written: {seen:?}"
        );
        assert!(
            !seen.iter().any(|sql| sql.contains(&quote("uuid-a"))),
            "another player's row is not touched: {seen:?}"
        );
    }

    #[test]
    fn a_table_name_that_is_not_a_name_is_refused() {
        let error = identifier("homes; DROP TABLE players").expect_err("the name is refused");
        assert!(matches!(error, Error::Protocol(_)));
        assert_eq!(
            identifier("modernhome_homes").expect("a name is kept"),
            "modernhome_homes"
        );
    }
}
