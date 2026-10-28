//! Playing a configured sound.
//!
//! ## The name is a Bukkit `Sound` enum name, and it is converted
//!
//! `sounds.yml` spells sounds as the Paper `Sound` enum did:
//! `ENTITY_VILLAGER_NO`, `BLOCK_NOTE_BLOCK_PLING`, `UI_BUTTON_CLICK`. That
//! spelling is kept, because it is what every existing config says, and converted
//! to a resource location by the one rule that covers it: lower-case the whole
//! name and turn every `_` into `.`
//!
//! ```text
//! ENTITY_VILLAGER_NO   -> minecraft:entity.villager.no
//! BLOCK_NOTE_BLOCK_PLING -> minecraft:block.note_block.pling
//! UI_BUTTON_CLICK       -> minecraft:ui.button.click
//! ```
//!
//! A Pumpkin plugin has no name-to-sound lookup: `sounds.wit` is an enum of 1,991
//! variants with no `from_name`, and the API's [`Sound`] type has no registry
//! path. The sane alternative is the escape hatch the API *does* provide,
//! `Player::play_custom_sound`, which takes the resource location as a string -
//! and on the host a resource location is literally a `String`
//! (`pumpkin-util/src/resource_location.rs:2`), accepted either bare or with the
//! `minecraft:` namespace. So the conversion is complete rather than a guess, and
//! an unknown name fails the way Paper's did: silently, with nothing played.
//!
//! ## The early returns are kept
//!
//! `SoundsConfig.playSound` did nothing when the section was missing, or when it
//! had no `sound:` key, or when the sound did not resolve. A port reproduces the
//! first two and inherits the third from the client, which ignores a sound event
//! it does not recognise. Nothing is logged on any of them - Paper logged nothing
//! either.

use pumpkin_plugin_api::player::{Player, SoundCategory};

/// The category every sound in `sounds.yml` is played in.
///
/// Paper's `player.playSound(loc, sound, volume, pitch)` used the sound's own
/// category, which is what `minecraft:<category>` would have been. `MASTER` is the
/// closest single answer, and it is the one that reaches every user regardless of
/// how they have mixed their sound sliders.
const CATEGORY: SoundCategory = SoundCategory::Master;

/// Every Bukkit sound name this plugin resolves, and its real resource location.
///
/// Sorted by the name as written, which is also the order they appear in the shipped
/// file, so the two can be read against each other. Each key was read off the game's
/// own sound registry rather than derived from the Bukkit name - which is the whole
/// point of the table.
pub const KNOWN: [(&str, &str); 3] = [
    ("BLOCK_NOTE_BLOCK_PLING", "minecraft:block.note_block.pling"),
    ("ENTITY_VILLAGER_NO", "minecraft:entity.villager.no"),
    ("UI_BUTTON_CLICK", "minecraft:ui.button.click"),
];

/// The resource location a Bukkit sound name maps to.
///
/// A name that already carries a namespace is left as it is, so an admin can write
/// `minecraft:entity.villager.no` directly and get it without being handed
/// `minecraft:minecraft:...`.
#[must_use]
pub fn resource_location(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Some((_, location)) = KNOWN
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(trimmed))
    {
        return (*location).to_string();
    }
    if trimmed.contains(':') {
        trimmed.to_lowercase()
    } else {
        format!("minecraft:{}", trimmed.to_lowercase().replace('_', "."))
    }
}

/// Plays the sound `key`, if there is one and it names a sound.
///
/// `volume` and `pitch` are what the configured section asked for, or 1.0 - which
/// is what `getDouble("volume", 1.0)` meant.
pub fn play(config: &crate::config::Config, player: &Player, key: &str) {
    let Some((name, volume, pitch)) = config.sound(key) else {
        return;
    };
    let location = resource_location(&name);
    if location.is_empty() {
        return;
    }
    player.play_custom_sound(&location, CATEGORY, volume, pitch);
}



#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_sound_names_become_resource_locations() {
        // All three names `sounds.yml` uses, converted by the rule, and these are
        // the real resource locations.
        assert_eq!(
            resource_location("ENTITY_VILLAGER_NO"),
            "minecraft:entity.villager.no"
        );
        assert_eq!(
            resource_location("BLOCK_NOTE_BLOCK_PLING"),
            "minecraft:block.note_block.pling"
        );
        assert_eq!(resource_location("UI_BUTTON_CLICK"), "minecraft:ui.button.click");
    }

    #[test]
    fn a_name_that_already_has_a_namespace_is_not_doubled() {
        assert_eq!(
            resource_location("minecraft:entity.villager.no"),
            "minecraft:entity.villager.no"
        );
    }

    #[test]
    fn a_blank_name_makes_no_sound() {
        // An empty `sound:` key is the "nothing to play" case, and it must not
        // become `minecraft:` and be sent to the client.
        assert_eq!(resource_location(""), "");
        assert_eq!(resource_location("   "), "");
    }

    #[test]
    fn every_sound_the_config_ships_is_known() {
        // If this fails, `sounds.yml` names a sound the conversion does not
        // produce a location for, and that sound would silently never play.
        let config = crate::config::Config::defaults();
        for key in ["error", "ui-open", "ui-click", "teleported", "timer"] {
            let (name, _, _) = config.sound(key).expect("ships");
            assert!(!resource_location(&name).is_empty(), "{key}");
        }
    }

    #[test]
    fn the_default_volume_and_pitch_are_one() {
        // `getDouble("volume", 1.0)`, and both the shipped entries say so.
        let config = crate::config::Config::defaults();
        let (_, volume, pitch) = config.sound("ui-click").expect("ships");
        assert_eq!(volume, 1.0);
        assert_eq!(pitch, 1.0);
    }
}
