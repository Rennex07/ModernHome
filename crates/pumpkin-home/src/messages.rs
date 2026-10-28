//! Assembling a message and delivering it to a player.
//!
//! ## The prefix goes on before the placeholders, not after
//!
//! `MessagesConfig.get(key)` prepended `prefix` and *then* substituted the
//! placeholders, so a prefix could itself contain a `%placeholder%` and have it
//! filled. It never did, but the order is kept, because the one place it could
//! matter - a prefix containing `%target%`, say - now behaves the way the Paper
//! plugin's code says it should rather than the way some later refactor would.
//!
//! ## A missing key answers itself
//!
//! `getString(key, key)` is the whole of Paper's diagnostic for a typo'd key: the
//! message *is* the key name, with the prefix on the front, so
//! `home-limit-recahed` shows up in game as `AdvancedHome » home-limit-recahed`
//! instead of an empty line. That is reproduced, and a test pins it.
//!
//! ## Two gates, and one message each
//!
//! `allow-chat-messages` and `allow-actionbar-messages` gate the two channels
//! separately. The countdown's cancellation message is the interesting case: it
//! was sent to *both*, through `cancelWithMessage`, so turning chat off does not
//! silence it - it moves it entirely to the actionbar.

use pumpkin_plugin_api::player::Player;
use pumpkin_plugin_api::text::TextComponent;

use crate::{config::Config, text};

/// Renders `key` for delivery, prefix and placeholders included.
#[must_use]
pub fn render(config: &Config, key: &str, placeholders: &[(&'static str, String)]) -> String {
    text::apply(&config.message(key), placeholders)
}

/// Sends `key` to `player` in chat.
///
/// The caller has already decided that chat is allowed; this is the send, not the
/// gate. That is deliberate, because Paper's gates were `config.isChatEnabled()`
/// written out at each call site rather than one central switch, and keeping them
/// visible is what lets an admin see which messages a config file controls.
pub fn chat(config: &Config, player: &Player, key: &str, placeholders: &[(&'static str, String)]) {
    let rendered = render(config, key, placeholders);
    player.send_system_message(TextComponent::from_legacy_string_with_code(&rendered, text::CODE), false);
}

/// Sends `key` to `player` on the actionbar.
pub fn actionbar(config: &Config, player: &Player, key: &str, placeholders: &[(&'static str, String)]) {
    let rendered = render(config, key, placeholders);
    player.show_actionbar(TextComponent::from_legacy_string_with_code(&rendered, text::CODE));
}

/// The countdown's cancellation, which went to both channels.
///
/// `cancelWithMessage` sent one string through chat and the actionbar. It sent no
/// sound, and no countdown - the same string was simply both the message and the
/// reason.
pub fn cancel(config: &Config, player: &Player, key: &str, placeholders: &[(&'static str, String)]) {
    if config.allow_chat() {
        chat(config, player, key, placeholders);
    }
    if config.allow_actionbar() {
        actionbar(config, player, key, placeholders);
    }
}

/// The placeholders a message takes, built from the ones it actually uses.
///
/// Most messages take none, so the common case is an empty slice and no
/// allocations beyond the prefix.
#[must_use]
pub fn placeholders() -> Vec<(&'static str, String)> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_is_the_prefix_plus_the_key() {
        // The shipped prefix, verbatim from `messages.yml`. Note it is still
        // written in `&` codes: the conversion to `§` is the host's, and this
        // layer's job stops at substitution.
        let config = Config::defaults();
        assert_eq!(config.message("home-set"), "&8[&bHome&8] &7&aHome saved.");
    }

    #[test]
    fn a_missing_key_is_the_key_itself_with_the_prefix_on() {
        // Paper's only diagnostic for a typo. Reproduced, and pinned, so it stays.
        let config = Config::defaults();
        assert_eq!(config.message("no-such-key"), "&8[&bHome&8] &7no-such-key");
        // Which means a rendered missing key shows the key name, not a blank line.
        assert!(render(&config, "no-such-key", &[]).contains("no-such-key"));
    }

    #[test]
    fn placeholders_are_filled_after_the_prefix_is_on() {
        // `%index%` is the only placeholder any of the four commands substitutes.
        let config = Config::defaults();
        let rendered = render(&config, "home-not-exist", &[("%index%", "3".to_string())]);
        assert!(rendered.contains("Home 3"), "{rendered}");
        // And the prefix is in front of it, not inside the substituted value.
        assert!(rendered.starts_with("&8[&bHome&8]"));
    }

    #[test]
    fn a_message_with_no_placeholders_is_just_the_message() {
        let config = Config::defaults();
        assert_eq!(render(&config, "home-deleted", &[]), "&8[&bHome&8] &7&aHome deleted.");
    }

    #[test]
    fn the_empty_prefix_case_is_not_a_special_case() {
        // `prefix: ""` must mean no prefix, not a prefix that is the empty string
        // followed by a message. Paper checked `!prefix.isEmpty()`.
        let mut config = Config::defaults();
        config.replace(
            crate::config::Section::Messages,
            serde_norway::from_str("prefix: \"\"\nhome-set: \"&aHome set.\"\n").expect("parses"),
        );
        assert_eq!(config.message("home-set"), "&aHome set.");
    }

    #[test]
    fn a_prefix_with_a_placeholder_in_it_is_substituted() {
        // The reason the prefix is prepended *before* substitution rather than
        // after: a prefix may name a placeholder and have it filled.
        let mut config = Config::defaults();
        config.replace(
            crate::config::Section::Messages,
            serde_norway::from_str(
                "prefix: \"[%target%] \"\nhome-deleted: \"&cHome deleted.\"\n",
            )
            .expect("parses"),
        );
        assert_eq!(
            render(&config, "home-deleted", &[("%target%", "Target".to_string())]),
            "[Target] &cHome deleted."
        );
    }
}
