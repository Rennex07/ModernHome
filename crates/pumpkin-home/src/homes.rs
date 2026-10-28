//! A home, and the record shape it round-trips through the store.
//!
//! ## What is stored, and what is resolved on read
//!
//! The Java `Home` constructor had two overloads and a rule that is worth
//! carrying over exactly: a custom name of `null`, `""` or `"   "` became `null`,
//! and anything else was `trim()`ed. So an empty name is stored as `null` and
//! renders as an empty `%home%`, which is why the shipped template
//! `&9Home %index% %home%` ends in a space for an unnamed home.
//!
//! A `Location` was *resolved* lazily - `getLocation()` rebuilt one from
//! `worldName` plus the raw coordinates only when the world was actually loaded -
//! so a home in a world that is not loaded still has its coordinates and reports
//! its world as "not loaded". [`Home::resolve`] is the same idea, without the
//! handle, because a port cannot hold one across the guest boundary.

use serde::{Deserialize, Serialize};

/// One home, as it is stored and as the GUI needs it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Home {
    /// The slot this home occupies. Unique per player.
    pub home_number: i32,
    /// The world the home was set in, by name, or `null` if it never had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub world: Option<String>,
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
    #[serde(default)]
    pub z: f64,
    #[serde(default)]
    pub yaw: f32,
    #[serde(default)]
    pub pitch: f32,
    /// The custom name, `null` when the home has none. See the module header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The server the home was set on, `null` when it predates cross-server
    /// records - which is also how a home that has always been local is spelled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
}

impl Home {
    /// Builds a home from a stored row.
    ///
    /// `world` and `name` are normalized here rather than at the call site, so
    /// an empty string written by an older build behaves like a missing field.
    #[must_use]
    pub fn new(
        home_number: i32,
        world: Option<String>,
        x: f64,
        y: f64,
        z: f64,
        yaw: f32,
        pitch: f32,
        name: Option<String>,
        server_id: Option<String>,
    ) -> Self {
        Self {
            home_number,
            world: world.filter(|w| !w.is_empty()),
            x,
            y,
            z,
            yaw,
            pitch,
            name: name.filter(|n| !n.trim().is_empty()).map(|n| n.trim().to_string()),
            server_id,
        }
    }

    /// The custom name, or an empty string when there is none.
    ///
    /// The `%home%` placeholder, and the rule the Java `getCustomName()` implied
    /// by returning `null`: it is the caller that decides between `Some` and the
    /// placeholder's empty value.
    #[must_use]
    pub fn name_or_empty(&self) -> &str {
        self.name.as_deref().unwrap_or("")
    }

    /// Whether this home belongs to `current_server`.
    ///
    /// `server_id == null` means it was written before cross-server records
    /// existed, and the Java `isLocal` treated that as local. Same here.
    #[must_use]
    pub fn is_local(&self, current_server: &str) -> bool {
        self.server_id.as_deref() == Some(current_server) || self.server_id.is_none()
    }

    /// Whether the home's world is loaded.
    ///
    /// The Java version answered this with `Bukkit.getWorld(name) != null`. A
    /// port has the same answer from the server's world list, so the check moves
    /// to the caller that has the handle.
    #[must_use]
    pub fn has_world(&self) -> bool {
        self.world.is_some()
    }

    /// `%.0f, %.0f, %.0f` of the stored coordinates.
    ///
    /// Formatted with `format!("{:.,...}")` rather than a cast, so a coordinate
    /// of `70.5` shows as `71` in the delete-confirmation lore exactly as
    /// `String.format("%.0f", ...)` did.
    #[must_use]
    pub fn coordinates(&self) -> String {
        format!("{:.0}, {:.0}, {:.0}", self.x, self.y, self.z)
    }

    /// The server this home belongs to, for `%server%` and the cross-server lore.
    #[must_use]
    pub fn server_or_empty(&self) -> &str {
        self.server_id.as_deref().unwrap_or("")
    }
}

/// Everything one player has, the shape the store file holds.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlayerHomes {
    pub homes: Vec<Home>,
}

impl PlayerHomes {
    /// The home in slot `number`, if there is one.
    #[must_use]
    pub fn find(&self, number: i32) -> Option<&Home> {
        self.homes.iter().find(|home| home.home_number == number)
    }

    /// The slot numbers in use, ascending.
    ///
    /// The SQL the Paper plugin read with was `ORDER BY home_number`, and both
    /// the duplicate scan and the "lowest free slot" walk depend on that order.
    /// [`Self::insert`] keeps it, and reads assume it.
    #[must_use]
    pub fn numbers(&self) -> Vec<i32> {
        self.homes.iter().map(|home| home.home_number).collect()
    }

    /// The lowest free slot number between 1 and `max`, or `-1` when there is none.
    ///
    /// `/sethome` used this rule - walk upwards from 1 and take the first gap -
    /// and the Paper plugin computed the same thing over a `HashSet`, which is
    /// fine only because the walk starts at 1.
    #[must_use]
    pub fn next_slot(&self, max: i32) -> i32 {
        for number in 1..=max {
            if self.find(number).is_none() {
                return number;
            }
        }
        -1
    }

    /// Adds or replaces the home in `self`'s slot, keeping the list ordered.
    ///
    /// An insert and an update are the same operation - the Paper SQL was an
    /// `INSERT ... ON CONFLICT(uuid, home_number) DO UPDATE` - so there is one
    /// method here rather than two.
    pub fn insert(&mut self, home: Home) {
        match self
            .homes
            .iter_mut()
            .find(|existing| existing.home_number == home.home_number)
        {
            Some(existing) => *existing = home,
            None => {
                self.homes.push(home);
                self.homes.sort_by_key(|home| home.home_number);
            }
        }
    }

    /// Removes the home in slot `number`, reporting whether it was there.
    pub fn delete(&mut self, number: i32) -> bool {
        let before = self.homes.len();
        self.homes.retain(|home| home.home_number != number);
        self.homes.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(number: i32) -> Home {
        Home::new(number, Some("world".to_string()), 1.0, 2.0, 3.0, 90.0, 0.0, None, None)
    }

    #[test]
    fn an_empty_or_blank_name_becomes_no_name() {
        // The Java constructor: null, "" and "   " all collapse to null, and
        // `%home%` then renders empty. Copying that is what keeps the shipped
        // item template's trailing space behaving the same.
        for blank in [None, Some(String::new()), Some("   ".to_string())] {
            let made = Home::new(1, None, 0.0, 0.0, 0.0, 0.0, 0.0, blank, None);
            assert_eq!(made.name, None);
            assert_eq!(made.name_or_empty(), "");
        }
    }

    #[test]
    fn a_name_is_trimmed_and_kept() {
        let made = Home::new(1, None, 0.0, 0.0, 0.0, 0.0, 0.0, Some("  base  ".into()), None);
        assert_eq!(made.name.as_deref(), Some("base"));
    }

    #[test]
    fn a_home_with_no_server_id_counts_as_local() {
        // Written before cross-server records, so it must not look like a home
        // that belongs to some other server.
        let local = Home::new(1, None, 0.0, 0.0, 0.0, 0.0, 0.0, None, None);
        assert!(local.is_local("server-1"));
    }

    #[test]
    fn only_the_same_server_id_counts_as_local() {
        let remote = Home::new(1, None, 0.0, 0.0, 0.0, 0.0, 0.0, None, Some("server-2".into()));
        assert!(!remote.is_local("server-1"));
        assert!(remote.is_local("server-2"));
    }

    #[test]
    fn coordinates_round_like_string_format_for_everything_but_a_half() {
        let made = Home::new(1, None, 12.5, 70.0, -13.25, 0.0, 0.0, None, None);
        // `String.format("%.0f", ...)` rounds half *away from zero*, so 12.5
        // shows as 13 there and as 12 here, because Rust's `{:.0}` rounds half
        // *to even*. A Minecraft coordinate's fractional part can be any float,
        // so a home can land on a half; it is cosmetic and is asserted rather
        // than papered over.
        assert_eq!(made.coordinates(), "12, 70, -13");

        // Away from a half the two agree, which is every ordinary coordinate.
        let ordinary = Home::new(2, None, 12.4, 70.6, -13.7, 0.0, 0.0, None, None);
        assert_eq!(ordinary.coordinates(), "12, 71, -14");
    }

    #[test]
    fn a_home_without_a_world_reports_it() {
        // The GUI needs to say "world not loaded" for a cross-server home rather
        // than try to teleport into nothing.
        assert!(!Home::new(1, None, 0.0, 0.0, 0.0, 0.0, 0.0, None, Some("srv".into())).has_world());
        assert!(home(1).has_world());
    }

    #[test]
    fn the_lowest_free_slot_is_found_not_the_first_gap_in_a_set() {
        let mut homes = PlayerHomes::default();
        homes.insert(home(1));
        homes.insert(home(2));
        homes.insert(home(4));
        assert_eq!(homes.next_slot(10), 3);
        assert_eq!(homes.next_slot(1), -1, "slot 1 is taken and the cap is 1");
        assert_eq!(homes.next_slot(4), 3);
    }

    #[test]
    fn an_insert_replaces_rather_than_duplicating() {
        // The Paper SQL was `ON CONFLICT(uuid, home_number) DO UPDATE`, so
        // writing home 1 twice leaves one row. Two rows would double a slot in
        // the GUI and make `next_slot` skip a number that is unused.
        let mut homes = PlayerHomes::default();
        homes.insert(home(1));
        homes.insert(Home::new(
            1,
            Some("other".into()),
            9.0,
            9.0,
            9.0,
            0.0,
            0.0,
            Some("renamed".into()),
            None,
        ));
        assert_eq!(homes.homes.len(), 1);
        assert_eq!(homes.homes[0].world.as_deref(), Some("other"));
        assert_eq!(homes.homes[0].name.as_deref(), Some("renamed"));
    }

    #[test]
    fn the_stay_ordered_by_home_number() {
        // `ORDER BY home_number` is what the GUI's slot arithmetic and both
        // command's duplicate scan are written against.
        let mut homes = PlayerHomes::default();
        homes.insert(home(7));
        homes.insert(home(2));
        homes.insert(home(4));
        assert_eq!(homes.numbers(), vec![2, 4, 7]);
    }

    #[test]
    fn deleting_reports_whether_the_home_was_there() {
        let mut homes = PlayerHomes::default();
        homes.insert(home(1));
        assert!(homes.delete(1));
        assert!(!homes.delete(1));
        assert!(homes.homes.is_empty());
    }

    #[test]
    fn finding_walks_the_ordered_list() {
        let mut homes = PlayerHomes::default();
        homes.insert(home(3));
        assert!(homes.find(3).is_some());
        assert!(homes.find(2).is_none());
    }
}
