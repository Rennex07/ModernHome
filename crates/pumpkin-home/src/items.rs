//! Building a GUI item from a configured template.
//!
//! ## Materials are named the Bukkit way
//!
//! `gui/homes.yml` and `gui/confirm.yml` spell materials as the Paper
//! `Material` enum did - `LIME_BED`, `RED_STAINED_GLASS_PANE`, `GRAY_DYE` - and
//! that spelling is kept: it is what every existing config file on every server
//! running this plugin already says, and an admin who has edited one must not
//! have to learn a second vocabulary.
//!
//! So the name is mapped to its registry key rather than the other way round:
//! lower-cased, and given the `minecraft:` namespace. `Material.matchMaterial`'s
//! fallback was `STONE` when the name did not match, and the fallback here is
//! `Item::Stone`, for the same reason - a typo in a config file should be a
//! visible stone, not a crash and not an invisible slot.
//!
//! ## Two rules for the display name, and they differ on purpose
//!
//! `HomesGuiConfig.buildItem` set the name whenever the `name:` key existed -
//! including when it was the empty string - so an item configured with `name: ""`
//! shows nothing. `FillerItem.toItemStack` and `ConfirmItem.toItemStack`, which
//! are the same code path for the confirm screen, only set it when it was
//! non-empty. The difference is one `isEmpty()` check in the Java, and both
//! behaviours are reproduced rather than unified, because an admin who wrote
//! `name: ""` on a home item and expected a blank name is relying on the first.

use pumpkin_plugin_api_v0_2::ItemStack;
use pumpkin_plugin_api_v0_2::item::ItemStackExt;
use pumpkin_plugin_api_v0_2::text::TextComponent;

use crate::text;

/// The fallback material, the Java `Material.STONE`.
///
/// The v0.2 host answers an unknown registry key with air rather than with an
/// error, so the fallback is applied by the plugin: an item that names
/// something the server does not have becomes a stone, which is what Paper's
/// `Material.matchMaterial` returning `null` became.
const FALLBACK_KEY: &str = "minecraft:stone";

/// The registry key a configured material name maps to.
///
/// The whole of the Bukkit-name rule, and it is pure: lower-case the name and, if
/// it has no namespace of its own, give it `minecraft:`. Note that the `_` are
/// *kept*, not turned into dots - `lime_bed` is the real name, and only the
/// *sounds* file uses the dotted form. See [`crate::sounds`].
///
/// A name that already carries a namespace is returned as written, so an admin who
/// writes `minecraft:lime_bed` - or a resource-pack item of their own - gets it
/// rather than `minecraft:minecraft:...`.
///
/// An empty name maps to the empty string, which the callers turn into a stone: it
/// is the `null` `Material.matchMaterial("")` returned, and `STONE` is what Paper
/// substituted for a name that did not resolve.
#[must_use]
pub fn material_key(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.contains(':') {
        trimmed.to_lowercase()
    } else {
        format!("minecraft:{}", trimmed.to_lowercase())
    }
}

/// Maps a configured material name to the registry key of its item.
///
/// The name is the one the configuration gives; the key is what the item is
/// created from. An empty name maps to [`FALLBACK_KEY`], which is the `null`
/// `Material.matchMaterial("")` returned, and `STONE` is what Paper substituted
/// for a name that did not resolve.
///
/// This one reaches the host - `ItemStack` is a wit import, and off `wasm32` it
/// is an `unreachable!()` stub - so it is not exercised by the tests here.
/// [`material_key`], which is the whole of the decision, is.
pub async fn material(name: &str) -> ItemStack {
    let key = material_key(name);
    let key = if key.is_empty() {
        FALLBACK_KEY.to_string()
    } else {
        key
    };
    let stack = ItemStack::of(key.clone(), 1);
    if stack.get_registry_key().await == "minecraft:air" && key != FALLBACK_KEY {
        // The host does not have that item, and answered with air. Paper
        // answered with a stone, so the stone is what goes on the screen.
        return ItemStack::of(FALLBACK_KEY, 1);
    }
    stack
}

/// A single item of count one, from a Bukkit material name.
///
/// The one-item case, which is every item this plugin puts on a screen: the Java
/// `new ItemStack(material)` had no other constructor and so never set an amount.
pub async fn of(name: &str) -> ItemStack {
    material(name).await
}

/// Turns italic off on an item's text.
///
/// This is what makes the screens look like AdvancedHome's did rather than the
/// vanilla default. A client renders an item's display name and its lore in
/// *italic* unless the component says otherwise, and Bukkit's `ItemMeta` always
/// said otherwise: `setDisplayName` and `setLore` both applied an `italic(false)`
/// decoration before the string was even parsed. A legacy `&`-coded string carries
/// no such instruction, so without this every name and every line of lore on both
/// screens comes out slanted where the original was upright.
///
/// Chat is untouched - a chat message is not item text, and is not italic by
/// default either way - so this is applied to items only.
pub fn plain(component: TextComponent) -> TextComponent {
    component.italic(false)
}

/// Turns a rendered string into a component, non-italic.
///
/// Almost always this is the host's legacy parser over the whole string, which is
/// what every template in the shipped config is written for. The one case that cannot
/// go through it is a string holding an ampersand that is not a colour code: the
/// parser consumes the character after every `&`, valid or not, so `Fish & Chips`
/// loses its `&` and a typo like `&HI` loses both characters.
///
/// So the string is split into code runs and literal runs. Code runs go to the
/// parser and are nested in as children; literal runs go on as plain text and are
/// never shown to the parser. A string with nothing but codes - every template, and
/// every name without a stray `&` - takes the single-call path and behaves exactly
/// as before.
pub fn component(rendered: &str) -> TextComponent {
    let runs = text::segments(rendered);

    let all_codes = runs.iter().all(|(is_code, _)| *is_code);
    if all_codes {
        return plain(TextComponent::from_legacy_string_with_code(
            rendered,
            text::CODE,
        ));
    }

    let mut root = TextComponent::text("");
    for (is_code, piece) in runs {
        if is_code {
            let child = TextComponent::from_legacy_string_with_code(&piece, text::CODE);
            root = root.add_child(child);
        } else {
            root = root.add_text(&piece);
        }
    }
    plain(root)
}

/// Builds one GUI item.
///
/// `set_name_when_empty` chooses between the two Java rules described in the
/// module header: `true` for the homes screen's own items, `false` for the
/// confirm screen's and for fillers.
pub async fn build_item(
    item: &serde_norway::Value,
    placeholders: &[(&str, String)],
    always_name: bool,
) -> ItemStack {
    let name = item.get("material").and_then(serde_norway::Value::as_str);
    let stack = match name {
        Some(name) => material(name).await,
        None => ItemStack::of(FALLBACK_KEY, 1),
    };

    let name = item.get("name").and_then(serde_norway::Value::as_str);
    if let Some(name) = name {
        if always_name || !name.is_empty() {
            let rendered = text::apply(name, placeholders);
            // Paper's `ColorUtils.color(applyPlaceholders(...))` at
            // `HomesGuiConfig.java:168`: placeholders first, colour second, so a home
            // named `&#FF0000` colours its own bed.
            stack
                .set_custom_name(Some(component(&text::decorate(&rendered))))
                .await;
        }
    }

    let lore = item
        .get("lore")
        .and_then(serde_norway::Value::as_sequence)
        .map(|lines| {
            lines
                .iter()
                .filter_map(serde_norway::Value::as_str)
                .map(|line| text::apply(line, placeholders))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if !lore.is_empty() {
        // One host call carries the whole list, so it is built and moved rather
        // than appended a line at a time. `set_lore` takes the list by value -
        // `list<text-component>` in the WIT, which wit-bindgen owns.
        stack
            .set_lore(
                lore.into_iter()
                    .map(|line| {
                        // Same order as the name above, and for the same reason: Paper
                        // colourised the lore after substituting into it too.
                        component(&text::decorate(&line))
                    })
                    .collect(),
            )
            .await;
    }

    stack
}

/// A filler, which is a slot number and an item rather than a behaviour.
#[derive(Debug, Clone, PartialEq)]
pub struct Filler {
    /// The slot it lands in.
    pub slot: u32,
    /// The configured material name, as written.
    pub material: String,
    /// The configured display name, as written.
    pub name: String,
    /// The configured lore, as written.
    pub lore: Vec<String>,
}

/// Reads the `fillers:` list from a GUI file.
///
/// Java read this with `getMapList`, which type-demoted each entry: a missing
/// `name` became `""`, a missing `material` became `"STONE"`, and a `slot` that
/// was absent or negative skipped the entry entirely. The same three rules apply.
#[must_use]
pub fn fillers(root: &serde_norway::Value) -> Vec<Filler> {
    root.get("fillers")
        .and_then(serde_norway::Value::as_sequence)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    let slot = entry.get("slot").and_then(serde_norway::Value::as_u64)?;
                    Some(Filler {
                        slot: slot as u32,
                        material: entry
                            .get("material")
                            .and_then(serde_norway::Value::as_str)
                            .unwrap_or("STONE")
                            .to_string(),
                        name: entry
                            .get("name")
                            .and_then(serde_norway::Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        lore: entry
                            .get("lore")
                            .and_then(serde_norway::Value::as_sequence)
                            .map(|lines| {
                                lines
                                    .iter()
                                    .filter_map(serde_norway::Value::as_str)
                                    .map(str::to_string)
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_norway::Value;

    /// Parses a snippet of GUI YAML.
    fn value(yaml: &str) -> serde_norway::Value {
        serde_norway::from_str(yaml).expect("test yaml parses")
    }

    #[test]
    fn the_bukkit_material_names_the_shipped_configs_use_map_to_their_real_keys() {
        // Every material in the two shipped files, spelled as Paper spelled it.
        // This is the whole risk in the material rule, so it is checked against the
        // keys the game actually uses rather than against the enum.
        for (name, expected) in [
            ("LIME_BED", "minecraft:lime_bed"),
            ("LIME_DYE", "minecraft:lime_dye"),
            ("LIGHT_GRAY_BED", "minecraft:light_gray_bed"),
            ("GRAY_DYE", "minecraft:gray_dye"),
            ("RED_BED", "minecraft:red_bed"),
            ("RED_DYE", "minecraft:red_dye"),
            ("ARROW", "minecraft:arrow"),
            ("RED_STAINED_GLASS_PANE", "minecraft:red_stained_glass_pane"),
            (
                "LIME_STAINED_GLASS_PANE",
                "minecraft:lime_stained_glass_pane",
            ),
            ("BARRIER", "minecraft:barrier"),
            ("PAPER", "minecraft:paper"),
            ("STONE", "minecraft:stone"),
        ] {
            assert_eq!(material_key(name), expected, "for {name}");
        }
    }

    #[test]
    fn the_shipped_gui_files_only_use_names_this_rule_resolves() {
        // Read straight out of the shipped files, so a future edit that adds a
        // material this rule cannot spell is caught here rather than showing up as a
        // stone on a server.
        let config = crate::config::Config::defaults();
        for root in [config.homes_gui(), config.confirm_gui()] {
            for entry in all_item_materials(root) {
                let key = material_key(&entry);
                assert!(!key.is_empty(), "`{entry}` maps to nothing");
                assert_eq!(
                    key.to_lowercase(),
                    key,
                    "`{entry}` maps to a key that is not lower-case"
                );
            }
        }
    }

    /// Every `material:` in a GUI file, including the ones inside `items:` and
    /// `fillers:`.
    fn all_item_materials(root: &serde_norway::Value) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(Value::String(name)) = root.get("material") {
            out.push(name.clone());
        }
        if let Some(items) = root.get("items").and_then(Value::as_mapping) {
            for (_, section) in items {
                if let Some(Value::String(name)) = section.get("material") {
                    out.push(name.clone());
                }
            }
        }
        out.extend(fillers(root).into_iter().map(|filler| filler.material));
        out
    }

    #[test]
    fn a_name_that_is_already_a_registry_key_is_left_alone() {
        // An admin who writes the modern spelling must not get
        // `minecraft:minecraft:...`.
        assert_eq!(material_key("minecraft:lime_bed"), "minecraft:lime_bed");
        // And one of their own resource-pack items is left entirely alone.
        assert_eq!(material_key("mypack:laser_gun"), "mypack:laser_gun");
    }

    #[test]
    fn a_blank_name_maps_to_nothing_and_becomes_a_stone() {
        // `Material.matchMaterial("")` was null, and `STONE` is what Paper
        // substituted for anything that did not resolve.
        assert_eq!(material_key(""), "");
        assert_eq!(material_key("   "), "");
        assert_eq!(FALLBACK_KEY, "minecraft:stone");
    }

    #[test]
    fn a_name_with_upper_case_is_lowered() {
        // Bukkit's enum names are SCREAMING_SNAKE, so lowering is the rule rather
        // than an allowance.
        assert_eq!(material_key("LIME_BED"), material_key("lime_bed"));
    }

    #[test]
    fn a_filler_needs_a_slot_and_defaults_the_rest() {
        let fillers = fillers(&value(
            "fillers:\n  - slot: 4\n    name: '&bHi'\n  - slot: 8\n  - name: 'no slot'\n  - slot: 0\n",
        ));
        assert_eq!(fillers.len(), 3, "only the entry with no slot is skipped");
        assert_eq!(fillers[0].slot, 4);
        assert_eq!(fillers[0].name, "&bHi");
        // `FillerItem.toItemStack`'s defaults: name "" and material STONE.
        assert_eq!(fillers[1].slot, 8);
        assert_eq!(fillers[2].slot, 0, "slot 0 is a slot");
        assert_eq!(fillers[1].name, "");
        assert_eq!(fillers[1].material, "STONE");
    }

    #[test]
    fn a_missing_fillers_list_is_empty_rather_than_an_error() {
        assert!(fillers(&value("title: x\n")).is_empty());
    }

    // Deliberately absent: a test that builds an `ItemStack`.
    //
    // `ItemStack::of` and every setter on it are wit imports, and off `wasm32`
    // they are `unreachable!()` stubs, so such a test aborts the whole harness
    // rather than failing. What those tests would have checked - that the shipped
    // templates render to the right names and lore - is verified on the server
    // instead: open `/home` and read the items. [`material_key`] above is the part
    // of this module that can be wrong without a server noticing, and it is tested
    // against the real registry keys.
}
