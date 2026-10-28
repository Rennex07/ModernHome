//! The delete-confirmation screen: the home being deleted, and three buttons.
//!
//! ## Two kinds of slot, built in a fixed order
//!
//! `DeleteConfirmGui.decorate()` did three things, and the order still matters:
//!
//! 1. Every item under `items:` is placed in **every** slot its `symbol:` appears
//!    in, and a button is registered for the three that carry a recognised
//!    `nbt:`. This is why the shipped screen puts its paper item in slots 4 *and*
//!    8 - one item, one symbol, several slots.
//! 2. The `h` slots then get a home summary that is **not** configured at all: a
//!    sheet of paper, whose name and lore are built in code. Overwriting anything
//!    step 1 put there.
//! 3. Fillers last, over the top of both, purely cosmetic.
//!
//! So an admin can configure the three buttons but not the summary, and a
//! `gui-disposition` that puts a button's symbol where the summary should be is
//! overwritten by the summary. That is the Paper behaviour, and it is why the
//! shipped disposition keeps `h` on its own row segment.
//!
//! ## `nbt:` is how a button is recognised, not its key
//!
//! The Java `decorate` switched on `item.getNbt()` against the three literals, so
//! an admin who renames a button's `nbt:` silently removes it - the item stays,
//! the behaviour goes. The same three literals are matched here, so the failure
//! mode is identical rather than quietly different.

use crate::homes::Home;

/// The only `nbt:` values that make a button, as literals.
///
/// `ConfirmGuiConfig` had no constant for them; `deleteHome` compared them
/// inline. Spelled out here so a test can assert the set.
pub const CANCEL: &str = "TO_CANCEL";
pub const ACCEPT: &str = "TO_ACCEPT";
pub const BACK: &str = "TO_BACK";

/// Which of the three buttons a slot carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    /// Cancel the deletion and go back to the homes screen at page one.
    Cancel,
    /// Confirm the deletion.
    Accept,
    /// Go back to the homes screen without deleting.
    Back,
}

impl Button {
    /// The `nbt:` value that makes this button, or [`None`] for anything else.
    #[must_use]
    pub fn from_nbt(nbt: &str) -> Option<Self> {
        match nbt {
            CANCEL => Some(Self::Cancel),
            ACCEPT => Some(Self::Accept),
            BACK => Some(Self::Back),
            _ => None,
        }
    }
}

/// What a slot holds.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    /// A configured item, named by its key under `items:`.
    Configured(String),
    /// The summary of the home being deleted, which is not configured.
    HomeDisplay,
}

/// One decided slot.
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    /// The slot it lands in.
    pub slot: u32,
    /// What it shows.
    pub item: Item,
    /// The button it carries, if any.
    pub button: Option<Button>,
}

/// The confirmation screen, decided.
#[derive(Debug, Clone, PartialEq)]
pub struct Screen {
    /// Every slot with an item in it, in placement order.
    pub cells: Vec<Cell>,
    /// The screen's row count.
    pub rows: usize,
}

impl Screen {
    /// The button a slot carries, if any.
    #[must_use]
    pub fn button_at(&self, slot: u32) -> Option<Button> {
        self.cells
            .iter()
            .find(|cell| cell.slot == slot)
            .and_then(|cell| cell.button)
    }

    /// Whether a slot has any item at all.
    ///
    /// A slot with an item but no button is how the summary and a filler look, and
    /// a click on either does nothing.
    #[must_use]
    pub fn holds(&self, slot: u32) -> bool {
        self.cells.iter().any(|cell| cell.slot == slot)
    }
}

/// The placeholder set the confirm screen's configured items are built with.
///
/// `DeleteConfirmGui.buildPlaceholders`: `%player%`, `%target%`, `%index%`,
/// `%number%`, `%home%`. Note what is absent - `%homes_count%` and `%max_homes%`
/// are on the homes screen but not here, and a template that uses one on the
/// confirm screen leaves it literal.
#[must_use]
pub fn placeholders(viewer: &str, target: &str, home: &Home) -> Vec<(&'static str, String)> {
    let number = home.home_number.to_string();
    vec![
        ("%player%", viewer.to_string()),
        ("%target%", target.to_string()),
        ("%index%", number.clone()),
        ("%number%", number),
        ("%home%", home.name_or_empty().to_string()),
    ]
}

/// Plans the confirmation screen for `home`.
///
/// The whole of `decorate()`, minus the fillers - which are built from
/// [`crate::items::fillers`] and applied over the top of this.
#[must_use]
pub fn plan(config: &serde_norway::Value, layout: &crate::layout::ConfirmLayout) -> Screen {
    let mut cells: Vec<Cell> = Vec::new();
    let size = u32::try_from(layout.rows).unwrap_or(u32::MAX) * 9;

    // Step one: every configured item, in file order. An item is placed in *every*
    // slot its symbol occupies, and the later of two items on the same symbol
    // wins, which is what placing them in order produced in Java too.
    if let Some(items) = config.get("items").and_then(serde_norway::Value::as_mapping) {
        for (key, item) in items {
            let Some(item) = item.as_mapping() else { continue };
            let symbol = item
                .get("symbol")
                .and_then(serde_norway::Value::as_str)
                .and_then(|s| s.chars().next())
                .unwrap_or('-');
            let nbt = item
                .get("nbt")
                .and_then(serde_norway::Value::as_str)
                .unwrap_or("");
            let button = Button::from_nbt(nbt);
            let key = key.as_str().unwrap_or_default().to_string();

            for (slot_symbol, slot) in &layout.slots {
                if *slot_symbol != symbol {
                    continue;
                }
                // `slot < 0 || slot >= inventory.getSize()` in Java, and the
                // same bounds check guards a disposition longer than the screen.
                if *slot >= size {
                    continue;
                }
                cells.push(Cell {
                    slot: *slot,
                    item: Item::Configured(key.clone()),
                    button,
                });
            }
        }
    }

    // Step two: the summary, which replaces whatever step one put in an `h` slot.
    for (symbol, slot) in &layout.slots {
        if *symbol != 'h' || *slot >= size {
            continue;
        }
        cells.retain(|cell| cell.slot != *slot);
        cells.push(Cell {
            slot: *slot,
            item: Item::HomeDisplay,
            button: None,
        });
    }

    Screen {
        cells,
        rows: layout.rows,
    }
}

/// The name of the summary item.
///
/// `"&9Home " + number + " " + name`, where `name` is the custom name or the empty
/// string. So an unnamed home's name ends in a space, exactly as the Java string
/// concatenation produced.
#[must_use]
pub fn summary_name(home: &Home) -> String {
    format!("&9Home {} {}", home.home_number, home.name_or_empty())
}

/// The two lore lines of the summary item.
///
/// `&7World: &f<world>` with `"unknown"` when the home has no world name, and
/// `&7Coords: &f` followed by the coordinates rounded to whole numbers.
#[must_use]
pub fn summary_lore(home: &Home) -> [String; 2] {
    [
        format!(
            "&7World: &f{}",
            home.world.as_deref().unwrap_or("unknown")
        ),
        format!("&7Coords: &f{}", home.coordinates()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::Config;
    use crate::homes::Home;
    use crate::layout::ConfirmLayout;

    /// The shipped `gui-disposition`.
    fn layout() -> ConfirmLayout {
        ConfirmLayout::parse(&[
            "---------".to_string(),
            "--d-h-a--".to_string(),
            "----g----".to_string(),
        ])
    }

    fn home(number: i32, name: Option<&str>) -> Home {
        Home::new(
            number,
            Some("world".into()),
            12.0,
            70.0,
            -13.0,
            0.0,
            0.0,
            name.map(str::to_string),
            None,
        )
    }

    fn config() -> Config {
        Config::defaults()
    }

    #[test]
    fn the_shipped_screen_has_a_cancel_a_confirm_and_a_back() {
        let screen = plan(config().confirm_gui(), &layout());

        assert_eq!(screen.button_at(11), Some(Button::Cancel));
        assert_eq!(screen.button_at(15), Some(Button::Accept));
        assert_eq!(screen.button_at(22), Some(Button::Back));
        assert_eq!(screen.rows, 3);
    }

    #[test]
    fn the_summary_is_in_every_slot_its_symbol_occupies() {
        // Row 1 of the shipped disposition is `--d-h-a--`, so its `h` is column 4,
// which is slot 13. An admin adding a second `h` to that row gets a second paper.
        let screen = plan(config().confirm_gui(), &layout());
        let display: Vec<u32> = screen
            .cells
            .iter()
            .filter(|cell| cell.item == Item::HomeDisplay)
            .map(|cell| cell.slot)
            .collect();
        assert_eq!(display, vec![13]);
    }

    #[test]
    fn a_configured_item_is_in_every_slot_its_symbol_occupies() {
        let screen = plan(config().confirm_gui(), &layout());
        let cancels: Vec<u32> = screen
            .cells
            .iter()
            .filter(|cell| cell.item == Item::Configured("cancel-update".into()))
            .map(|cell| cell.slot)
            .collect();
        assert_eq!(cancels, vec![11]);
    }

    #[test]
    fn an_item_with_an_unknown_nbt_is_shown_but_has_no_button() {
        // Renaming a `nbt:` keeps the item and silently removes the behaviour.
        let mut edited = config().confirm_gui().clone();
        if let Some(items) = edited
            .get_mut("items")
            .and_then(serde_norway::Value::as_mapping_mut)
            && let Some(cancel) = items.get_mut("cancel-update")
            && let Some(cancel) = cancel.as_mapping_mut()
        {
            cancel["nbt"] = serde_norway::Value::String("NOT_A_BUTTON".into());
        }
        let screen = plan(&edited, &layout());

        assert_eq!(screen.button_at(11), None, "the behaviour is gone");
        assert!(
            screen
                .cells
                .iter()
                .any(|cell| cell.slot == 11 && cell.item == Item::Configured("cancel-update".into())),
            "the item is still there"
        );
    }

    #[test]
    fn the_summary_overwrites_whatever_its_symbol_slot_already_held() {
        // Step two ran *after* step one in Java, so the summary always wins in its
        // own slots - including a slot a configured item wanted. No shipped item
        // has symbol `h`, so this can only happen if an admin points one at `h`,
        // which is exactly what this does.
        //
        // The result is that the item disappears and its button goes with it: the
        // item is placed, the summary replaces it, and `cells.retain` drops the
        // cell - and with it the button.
        let mut edited = config().confirm_gui().clone();
        if let Some(items) = edited
            .get_mut("items")
            .and_then(serde_norway::Value::as_mapping_mut)
            && let Some(confirm) = items.get_mut("confirm-update")
            && let Some(confirm) = confirm.as_mapping_mut()
        {
            confirm["symbol"] = serde_norway::Value::String("h".into());
        }

        // The shipped disposition's `h` is slot 13.
        let screen = plan(&edited, &layout());
        assert_eq!(screen.button_at(13), None, "the confirm button is gone");
        assert!(
            screen
                .cells
                .iter()
                .any(|cell| cell.slot == 13 && cell.item == Item::HomeDisplay),
            "the summary is there instead"
        );
        assert!(
            !screen
                .cells
                .iter()
                .any(|cell| cell.item == Item::Configured("confirm-update".into())),
            "and the confirm item is not anywhere at all"
        );
        // The other two buttons are untouched: only the shared slot was affected.
        assert_eq!(screen.button_at(11), Some(Button::Cancel));
        assert_eq!(screen.button_at(22), Some(Button::Back));
    }

    #[test]
    fn no_cell_is_ever_placed_outside_the_screen() {
        // `slot >= inventory.getSize()` was checked for every item in Java. With
        // `col` capped at nine and `row` at the row count, `parse` cannot produce
        // an out-of-range slot, so this is the guard staying in place rather than
        // a case a config can reach.
        let layout = ConfirmLayout::parse(&["---------".to_string(), "#########".to_string()]);
        let screen = plan(config().confirm_gui(), &layout);

        assert_eq!(screen.rows, 2);
        assert!(
            screen.cells.iter().all(|cell| cell.slot < 18),
            "every slot is inside two rows"
        );
    }

    #[test]
    fn the_placeholders_carry_five_keys_and_not_seven() {
        let home = home(3, Some("base"));
        let placeholders = placeholders("Viewer", "Target", &home);
        let keys: Vec<&str> = placeholders.iter().map(|(key, _)| *key).collect();
        assert_eq!(keys.len(), 5);
        for wanted in ["%player%", "%target%", "%index%", "%number%", "%home%"] {
            assert!(keys.contains(&wanted), "{wanted} is missing");
        }
        for unwanted in ["%homes_count%", "%max_homes%"] {
            assert!(!keys.contains(&unwanted), "{unwanted} does not belong here");
        }
        assert_eq!(
            placeholders
                .iter()
                .find(|(key, _)| *key == "%home%")
                .map(|(_, value)| value.as_str()),
            Some("base")
        );
    }

    #[test]
    fn a_home_with_no_name_has_an_empty_home_placeholder() {
        let home = home(1, None);
        let placeholders = placeholders("Viewer", "Target", &home);
        assert_eq!(
            placeholders
                .iter()
                .find(|(key, _)| *key == "%home%")
                .map(|(_, value)| value.as_str()),
            Some("")
        );
    }

    #[test]
    fn the_summary_name_and_lore_are_built_in_code_not_config() {
        let home = home(4, Some("base"));
        assert_eq!(summary_name(&home), "&9Home 4 base");
        assert_eq!(
            summary_lore(&home),
            [
                "&7World: &fworld".to_string(),
                "&7Coords: &f12, 70, -13".to_string()
            ]
        );
    }

    #[test]
    fn an_unnamed_home_has_a_trailing_space_in_its_summary_name() {
        // `"&9Home " + number + " " + name` with an empty name, so the space is
        // at the end. It is what the Paper string concatenation produced.
        let home = home(4, None);
        assert_eq!(summary_name(&home), "&9Home 4 ");
    }

    #[test]
    fn a_home_with_no_world_says_unknown() {
        let home = Home::new(1, None, 0.0, 0.0, 0.0, 0.0, 0.0, None, None);
        assert_eq!(summary_lore(&home)[0], "&7World: &funknown");
        assert_eq!(summary_lore(&home)[1], "&7Coords: &f0, 0, 0");
    }

    #[test]
    fn a_nbt_that_is_empty_makes_no_button() {
        // A confirm item with no `nbt:` at all is decoration, and the shipped
        // file gives every item one.
        assert_eq!(Button::from_nbt(""), None);
        assert_eq!(Button::from_nbt("TO_CANCEL"), Some(Button::Cancel));
        assert_eq!(Button::from_nbt("TO_ACCEPT"), Some(Button::Accept));
        assert_eq!(Button::from_nbt("TO_BACK"), Some(Button::Back));
        assert_eq!(Button::from_nbt("something else"), None);
    }
}
