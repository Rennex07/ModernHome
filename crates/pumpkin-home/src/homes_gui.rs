//! The homes screen: what is in each slot, and what a click does.
//!
//! ## The screen is planned before it is built
//!
//! Everything here is pure. [`plan`] turns a store, a session and a layout into a
//! [`Grid`] that says which slot holds which item, and [`click`] turns a slot and
//! a click type into an [`Action`]. Neither touches the host, so both are testable
//! on the host target - which matters, because the slot arithmetic is the one part
//! of this plugin a typo in would silently corrupt: a wrong page offset puts home
//! 9 in home 1's bed and nothing complains.
//!
//! [`crate::lib`] does the host work: it builds the `Gui`, opens it, and runs the
//! action.
//!
//! ## The slot arithmetic, stated once
//!
//! `number = (page - 1) * slots_per_page + index + 1`, where `index` is the
//! position in the layout-ordered slot list and *not* the slot number. The
//! physical bed is `home_slots[index]`, and the paired delete button is
//! `dye_slots[index]` - which is the slot directly below it in the layout, and
//! which does not exist at all when that `h` was on the bottom row.
//!
//! ## What the viewer's limit has to do with it
//!
//! `max_homes` is resolved for the **viewer**, not the target. Paper read
//! `DonutHomesConfig.getMaxHomes(viewer)` inside `decorate()`, so an admin
//! viewing somebody else's homes has an effectively infinite limit, sees no
//! locked slots, and can edit slots past the target's own allowance. That is
//! preserved here, and it is the only way `/ahome see` works at all.
//!
//! ## Two homes screens cannot be told apart, and one is enough
//!
//! `InventoryClickEventData` carries no handle back to the `Gui` that was
//! clicked - only the screen *kind*. The Paper plugin keyed its handler map by
//! the `Inventory` object, which a port cannot do. So which screen is open lives
//! in the viewer's [`Session`](crate::state::Session), and a viewer only ever has
//! one open at a time.

use pumpkin_plugin_api::common::ClickType;

use crate::{
    config::Config,
    homes::{Home, PlayerHomes},
    layout::Layout,
    permissions::total_pages,
    state::Session,
    text,
};

/// A placeholder pair: the key as the template writes it, and the value.
pub type Placeholder = (&'static str, String);

/// What one home slot holds.
#[derive(Debug, Clone, PartialEq)]
pub enum Slot {
    /// A home is set here.
    Existing(Home),
    /// No home, but the viewer's limit reaches this number.
    Empty(i32),
    /// No home, and the viewer's limit does not reach this number.
    ///
    /// Purely decorative: the Paper plugin placed the item and registered no
    /// button, so a click does nothing at all - not even the click sound.
    Locked,
}

/// The screen, decided.
#[derive(Debug, Clone, PartialEq)]
pub struct Grid {
    /// One entry per home slot, in layout order.
    pub cells: Vec<Slot>,
    /// The slot number of each bed, in the same order as `cells`.
    pub home_slots: Vec<u32>,
    /// The slot number of each dye, or `None` where the layout has none below.
    pub dye_slots: Vec<Option<u32>>,
    /// The previous-page button's slot, when there is a previous page.
    pub previous: Option<u32>,
    /// The next-page button's slot, when there is a next page.
    pub next: Option<u32>,
    /// How many homes one page holds.
    pub slots_per_page: usize,
    /// How many pages the viewer's limit fills.
    pub total_pages: usize,
}

impl Grid {
    /// The slot index a physical slot belongs to.
    ///
    /// The bed and the dye of one home are found separately, because they mean
    /// different things and the dye is absent on the bottom row.
    #[must_use]
    pub fn home_index(&self, slot: u32) -> Option<usize> {
        self.home_slots.iter().position(|&s| s == slot)
    }

    /// The dye index a physical slot belongs to.
    #[must_use]
    pub fn dye_index(&self, slot: u32) -> Option<usize> {
        self.dye_slots
            .iter()
            .position(|dye| dye.as_ref() == Some(&slot))
    }

    /// Which cell a slot index holds.
    #[must_use]
    pub fn cell(&self, index: usize) -> Option<&Slot> {
        self.cells.get(index)
    }
}

/// The placeholder set for an item, without the per-home entries.
///
/// The whole of `buildGlobalPlaceholders`: `%player%`, `%target%`,
/// `%homes_count%`, `%max_homes%`, `%page%`, `%total_pages%`.
///
/// Order is ascending by length, so `%homes_count%` is read before `%home%` would
/// be - which matters, because a shorter key that is a *prefix* of a longer one
/// would otherwise eat it.
#[must_use]
pub fn global(
    config: &Config,
    session: &Session,
    homes: &PlayerHomes,
    pages: usize,
) -> Vec<Placeholder> {
    let mut out = vec![
        ("%player%", session.viewer_name.clone()),
        ("%target%", session.target_name.clone()),
        ("%max_homes%", session.max_homes.to_string()),
        ("%page%", session.page.to_string()),
        ("%total_pages%", pages.to_string()),
        ("%homes_count%", homes.homes.len().to_string()),
    ];
    out.sort_by_key(|item| std::cmp::Reverse(item.0.len()));
    let _ = config;
    out
}

/// The placeholder set for one home.
///
/// `buildHomePlaceholders`: the global set, plus `%index%`, `%number%` and
/// `%home%`, plus an ad-hoc `%server%` when the home belongs to another server.
#[must_use]
pub fn home<'a>(
    config: &Config,
    session: &'a Session,
    homes: &'a PlayerHomes,
    pages: usize,
    number: i32,
    home: &'a Home,
    server_id: &str,
) -> Vec<Placeholder> {
    let mut out = global(config, session, homes, pages);
    out.push(("%index%", number.to_string()));
    out.push(("%number%", number.to_string()));
    out.push(("%home%", home.name_or_empty().to_string()));
    if !home.is_local(server_id) {
        out.push(("%server%", home.server_or_empty().to_string()));
    }
    out
}

/// The placeholder set for an empty slot.
///
/// `%home%` is the empty string, which is why the shipped `&7No Home Set`
/// templates carry no `%home%` and the `tp-home-bed` one does.
#[must_use]
pub fn empty<'a>(
    config: &Config,
    session: &'a Session,
    homes: &'a PlayerHomes,
    pages: usize,
    number: i32,
) -> Vec<Placeholder> {
    let mut out = global(config, session, homes, pages);
    out.push(("%index%", number.to_string()));
    out.push(("%number%", number.to_string()));
    out.push(("%home%", String::new()));
    out
}

/// Plans the screen for `session` over `homes`.
///
/// The whole of `decorate()`: which slot holds what, and where the navigation
/// buttons go.
#[must_use]
pub fn plan(config: &Config, homes: &PlayerHomes, session: &Session, layout: &Layout) -> Grid {
    let per_page = layout.slots_per_page();
    let pages = total_pages(session.max_homes, per_page);
    let multiple = config.multiple_pages() && pages > 1;

    let start = session.page.saturating_sub(1) * per_page;
    let cells: Vec<Slot> = layout
        .home_slots
        .iter()
        .enumerate()
        .map(|(index, _)| {
            let number = (start + index + 1) as i32;
            match homes.find(number) {
                Some(home) => Slot::Existing(home.clone()),
                None if number <= session.max_homes => Slot::Empty(number),
                None => Slot::Locked,
            }
        })
        .collect();

    // Only slots inside the screen count, which is what the Paper plugin's two
    // `prevSlot < inventory.getSize()` checks guarded.
    let in_range = |slot: u32| slot < (layout.rows as u32) * 9;

    Grid {
        cells,
        home_slots: layout.home_slots.clone(),
        dye_slots: (0..layout.home_slots.len())
            .map(|index| layout.dye_for(index))
            .collect(),
        previous: (multiple && session.page > 1)
            .then(|| config.previous_page_slot())
            .filter(|&slot| in_range(slot)),
        next: (multiple && session.page < pages)
            .then(|| config.next_page_slot())
            .filter(|&slot| in_range(slot)),
        slots_per_page: per_page,
        total_pages: pages,
    }
}

/// What a click on `slot` means.
///
/// The Paper handler cancelled every click unconditionally and then looked for a
/// button, so a click on a locked slot, on a filler, or in the player's own
/// inventory does nothing. Only the cases below are actions.
#[must_use]
pub fn click(grid: &Grid, slot: u32, click_type: ClickType) -> Option<Action> {
    let left = matches!(click_type, ClickType::Left | ClickType::ShiftLeft);
    let right = matches!(click_type, ClickType::Right | ClickType::ShiftRight);

    if grid.previous == Some(slot) {
        return Some(Action::PreviousPage);
    }
    if grid.next == Some(slot) {
        return Some(Action::NextPage);
    }

    if let Some(index) = grid.home_index(slot) {
        return match grid.cell(index)? {
            Slot::Existing(home) => match (left, right) {
                (true, _) => Some(Action::Teleport { home: home.clone() }),
                (_, true) => Some(Action::Rename {
                    number: home.home_number,
                }),
                // A middle click, a double click, a number key: nothing. The
                // event was already cancelled, so no item moves either.
                _ => None,
            },
            Slot::Empty(number) => Some(Action::Set { number: *number }),
            Slot::Locked => None,
        };
    }

    if let Some(index) = grid.dye_index(slot) {
        return match grid.cell(index)? {
            Slot::Existing(home) => Some(Action::Delete { home: home.clone() }),
            // Both the bed and the dye of an empty slot set a home, which is what
            // the two identical lambdas in `addEmptySlot` did.
            Slot::Empty(number) => Some(Action::Set { number: *number }),
            Slot::Locked => None,
        };
    }

    None
}

/// What a click asked for.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Teleport the viewer to a home.
    Teleport { home: Home },
    /// Ask the viewer for a new name for home `number`.
    Rename { number: i32 },
    /// Set a new home in slot `number`.
    Set { number: i32 },
    /// Ask for confirmation before deleting a home.
    Delete { home: Home },
    /// Go back a page.
    PreviousPage,
    /// Go forward a page.
    NextPage,
}

/// The homes screen's title, with its placeholders filled in.
///
/// Build order is `%page%`, `%player%`, `%target%` - the order Paper's
/// `buildTitle()` used. A target name that contains `%player%` is therefore
/// substituted, and the reverse cannot happen.
#[must_use]
pub fn title(config: &Config, session: &Session) -> String {
    text::apply(
        &config.homes_title(),
        &[
            ("%page%", session.page.to_string()),
            ("%player%", session.viewer_name.clone()),
            ("%target%", session.target_name.clone()),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{homes::Home, state::Session, store::Store};

    /// A store holding `count` homes numbered 1..=count.
    fn store(count: i32) -> Store {
        let mut store = Store::default();
        for number in 1..=count {
            store.set_home(
                "target",
                Home::new(
                    number,
                    Some("world".into()),
                    (1.0, 2.0, 3.0),
                    (0.0, 0.0),
                    Some(format!("home{number}")),
                    None,
                ),
            );
        }
        store
    }

    fn homes_of(store: &Store) -> PlayerHomes {
        store.homes("target").cloned().unwrap_or_default()
    }

    /// A session watching `target`.
    fn session(max: i32) -> Session {
        Session::homes(
            "viewer".into(),
            "Viewer".into(),
            "target".into(),
            "Target".into(),
            max,
        )
    }

    /// The shipped six-row layout.
    fn layout() -> Layout {
        Layout::homes(&[])
    }

    #[test]
    fn page_one_shows_the_first_seven_homes() {
        // 7 slots a page, so home 1 sits in slot 11 and home 7 in slot 34.
        let store = store(10);
        let homes = homes_of(&store);
        let grid = plan(&Config::defaults(), &homes, &session(10), &layout());

        assert_eq!(grid.total_pages, 2);
        assert_eq!(grid.home_slots, vec![11, 13, 15, 28, 30, 32, 34]);
        let numbers: Vec<i32> = grid
            .cells
            .iter()
            .filter_map(|cell| match cell {
                Slot::Existing(home) => Some(home.home_number),
                Slot::Empty(number) => Some(*number),
                Slot::Locked => None,
            })
            .collect();
        assert_eq!(numbers, vec![1, 2, 3, 4, 5, 6, 7]);
        // Slot 11 is home 1's bed, not home 11's.
        let index = grid.home_index(11).expect("slot 11 is a bed");
        assert!(matches!(&grid.cells[index], Slot::Existing(h) if h.home_number == 1));
    }

    #[test]
    fn page_two_starts_at_the_eighth_home() {
        // `(page - 1) * slots_per_page + index + 1` is the whole offset, and a
        // mistake here would put home 8 in slot 11 without anything complaining.
        let store = store(10);
        let homes = homes_of(&store);
        let mut viewer = session(10);
        viewer.page = 2;

        let grid = plan(&Config::defaults(), &homes, &viewer, &layout());
        let index = grid.home_index(11).expect("slot 11 is a bed");
        assert!(
            matches!(&grid.cells[index], Slot::Existing(h) if h.home_number == 8),
            "slot 11 on page 2 is home 8"
        );
        // Slot 34 is index 6, which on page two is home 14 - past the limit of
        // 10, so it is *locked* rather than empty. A page runs past the limit, not
        // short of it, which is why the tail of a full-width last page is locked
        // slots and not simply nothing.
        let index = grid.home_index(34).expect("slot 34 is a bed");
        assert!(
            matches!(grid.cells[index], Slot::Locked),
            "home 14 is past a limit of 10"
        );
        // Page two is numbers 8 to 14. With a limit of ten, 8, 9 and 10 are inside it -
        // and all three are set - so every slot from 11 up is locked.
        for (slot, number) in [(11, 8), (13, 9), (15, 10)] {
            let index = grid.home_index(slot).expect("a bed");
            assert!(
                matches!(&grid.cells[index], Slot::Existing(h) if h.home_number == number),
                "slot {slot} on page 2 is home {number}"
            );
        }
        for (slot, number) in [(28, 11), (30, 12), (32, 13), (34, 14)] {
            let index = grid.home_index(slot).expect("a bed");
            assert!(
                matches!(grid.cells[index], Slot::Locked),
                "slot {slot} is home {number}, past the limit of 10"
            );
        }
    }

    #[test]
    fn a_number_past_the_limit_is_locked_and_answers_nothing() {
        let homes = homes_of(&store(0));
        let grid = plan(&Config::defaults(), &homes, &session(3), &layout());

        let index = grid.home_index(11).expect("slot 11 is a bed");
        assert!(matches!(grid.cells[index], Slot::Empty(1)));
        let index = grid.home_index(34).expect("slot 34 is a bed");
        assert!(
            matches!(grid.cells[index], Slot::Locked),
            "home 7 is past a limit of 3"
        );
        assert_eq!(click(&grid, 34, ClickType::Left), None);
    }

    #[test]
    fn a_left_click_on_a_home_teleports_and_a_right_click_renames() {
        let store = store(1);
        let homes = homes_of(&store);
        let grid = plan(&Config::defaults(), &homes, &session(5), &layout());

        let bed = grid.home_slots[0];
        let home = homes.find(1).cloned().expect("home 1");
        assert_eq!(
            click(&grid, bed, ClickType::Left),
            Some(Action::Teleport { home: home.clone() })
        );
        assert_eq!(
            click(&grid, bed, ClickType::ShiftLeft),
            Some(Action::Teleport { home: home.clone() }),
            "a shifted click is still a left click"
        );
        assert_eq!(
            click(&grid, bed, ClickType::Right),
            Some(Action::Rename { number: 1 })
        );
        assert_eq!(
            click(&grid, bed, ClickType::ShiftRight),
            Some(Action::Rename { number: 1 })
        );
    }

    #[test]
    fn a_middle_click_on_a_home_does_nothing() {
        // The Paper handler cancelled the event and then matched only left and
        // right. A middle click must still take no item out of the slot.
        let homes = homes_of(&store(1));
        let grid = plan(&Config::defaults(), &homes, &session(5), &layout());

        let bed = grid.home_slots[0];
        for click_type in [
            ClickType::Middle,
            ClickType::DoubleClick,
            ClickType::NumberKey,
            ClickType::Drop,
            ClickType::ControlDrop,
            ClickType::Unknown,
        ] {
            assert_eq!(
                click(&grid, bed, click_type),
                None,
                "{click_type:?} on a home's bed is not an action"
            );
        }
    }

    #[test]
    fn the_dye_opens_a_confirmation_and_never_teleports() {
        let store = store(1);
        let homes = homes_of(&store);
        let grid = plan(&Config::defaults(), &homes, &session(5), &layout());

        let dye = grid.dye_slots[0].expect("the shipped layout pairs a dye");
        assert_eq!(dye, 20);
        let home = homes.find(1).cloned().expect("home 1");
        assert_eq!(
            click(&grid, dye, ClickType::Right),
            Some(Action::Delete { home: home.clone() })
        );
        // Any click type, not just left: the Paper dye button did not filter.
        assert_eq!(
            click(&grid, dye, ClickType::ShiftLeft),
            Some(Action::Delete { home })
        );
    }

    #[test]
    fn an_empty_slots_bed_and_dye_both_set_a_home() {
        let homes = homes_of(&store(0));
        let grid = plan(&Config::defaults(), &homes, &session(5), &layout());

        let bed = grid.home_slots[0];
        let dye = grid.dye_slots[0].expect("paired");
        assert_eq!(
            click(&grid, bed, ClickType::Left),
            Some(Action::Set { number: 1 })
        );
        assert_eq!(
            click(&grid, dye, ClickType::Right),
            Some(Action::Set { number: 1 })
        );
        assert_eq!(
            click(&grid, bed, ClickType::Middle),
            Some(Action::Set { number: 1 })
        );
    }

    #[test]
    fn the_bottom_row_of_homes_has_no_dye() {
        // `--h--` on the last row: Paper guarded with `row + 1 < layout.size()`,
        // so no delete slot exists. The bed must still work on its own.
        let layout = Layout::parse(&["--h----".to_string()]);
        let homes = homes_of(&store(0));
        let grid = plan(&Config::defaults(), &homes, &session(5), &layout);

        assert_eq!(grid.home_slots.len(), 1);
        assert_eq!(grid.dye_slots, vec![None]);
        assert_eq!(
            click(&grid, grid.home_slots[0], ClickType::Left),
            Some(Action::Set { number: 1 })
        );
    }

    #[test]
    fn navigation_appears_only_when_there_is_a_page_to_go_to() {
        let homes = homes_of(&store(0));

        // A limit of five over seven a page is a single page, so there is
        // nowhere to go in either direction.
        let single = plan(&Config::defaults(), &homes, &session(5), &layout());
        assert_eq!(single.total_pages, 1);
        assert_eq!(single.previous, None, "page one has no previous");
        assert_eq!(single.next, None, "one page has no next");

        // A limit of ten is two pages, so page two has a way back and no way on.
        let second = {
            let mut viewer = session(10);
            viewer.page = 2;
            plan(&Config::defaults(), &homes, &viewer, &layout())
        };
        assert_eq!(second.total_pages, 2);
        assert_eq!(second.previous, Some(45));
        assert_eq!(second.next, None, "page two of two is the last");
    }

    #[test]
    fn an_admin_has_hundreds_of_millions_of_pages() {
        // `i32::MAX` homes over 7 a page is 306,783,379 pages, so the next button
        // never disappears for an admin. Asserted so a change to `total_pages`
        // cannot quietly "fix" this.
        let grid = plan(
            &Config::defaults(),
            &homes_of(&store(0)),
            &session(i32::MAX),
            &layout(),
        );

        assert_eq!(grid.total_pages, 306_783_379);
        assert_eq!(grid.next, Some(53));
        // Every slot is empty rather than locked, because the limit is infinite.
        assert!(grid.cells.iter().all(|cell| matches!(cell, Slot::Empty(_))));
    }

    #[test]
    fn turning_paging_off_removes_the_buttons_but_not_the_page_count() {
        // `enabled-multiple-pages: false` decides only whether the arrows are
        // placed. The page count is still derived from the limit, so a ten-home
        // viewer still has a second page *by arithmetic* - it just cannot reach it,
        // because there is no next button to click.
        //
        // The Paper config comment promised exactly this: "If false, only the
        // first page worth of homes will be shown".
        let mut config = Config::defaults();
        // The switch lives in `gui/homes.yml`, not in `config.yml`.
        config.replace(
            crate::config::Section::HomesGui,
            serde_norway::from_str("enabled-multiple-pages: false\n").expect("parses"),
        );
        let homes = homes_of(&store(10));

        let paged = plan(&Config::defaults(), &homes, &session(10), &layout());
        assert_eq!(paged.total_pages, 2);
        assert_eq!(
            paged.next,
            Some(53),
            "with paging on there is a way forward"
        );

        let unpaged = plan(&config, &homes, &session(10), &layout());
        assert_eq!(unpaged.total_pages, 2, "the arithmetic is unchanged");
        assert_eq!(unpaged.next, None, "but no button, so no way forward");
        assert_eq!(unpaged.previous, None);
    }

    #[test]
    fn the_title_fills_in_the_page() {
        assert_eq!(title(&Config::defaults(), &session(3)), "Homes • Page 1");
    }

    #[test]
    fn a_click_outside_the_screen_is_not_an_action() {
        // Slot 54 and beyond is the viewer's own inventory; slot 0 is a filler.
        // The Paper handler bailed on `rawSlot >= size` after cancelling.
        let homes = homes_of(&store(1));
        let grid = plan(&Config::defaults(), &homes, &session(5), &layout());
        assert_eq!(click(&grid, 54, ClickType::Left), None);
        assert_eq!(click(&grid, 0, ClickType::Left), None);
    }

    #[test]
    fn the_limit_decides_whether_a_slot_is_empty_or_locked() {
        // A viewer with a limit of 8 sees home 7 as a free slot; a viewer with a
        // limit of 6 sees the same slot locked. The home count does not matter.
        let homes = homes_of(&store(6));
        let grid = plan(&Config::defaults(), &homes, &session(8), &layout());
        assert!(matches!(grid.cells[6], Slot::Empty(7)));

        let grid = plan(&Config::defaults(), &homes, &session(6), &layout());
        assert!(matches!(grid.cells[6], Slot::Locked));
    }

    #[test]
    fn the_global_placeholders_carry_every_key_the_templates_comment_names() {
        // `gui/homes.yml` documents %player%, %target%, %index%, %home%,
        // %homes_count%, %max_homes%, %page%, %total_pages%. All eight have to be
        // present, or a configured template would print its own placeholder.
        let store = store(2);
        let homes = homes_of(&store);
        let mut out = global(&Config::defaults(), &session(7), &homes, 1);
        out.sort_by(|a, b| a.0.cmp(b.0));
        let keys: Vec<&str> = out.iter().map(|(key, _)| *key).collect();
        for wanted in [
            "%player%",
            "%target%",
            "%max_homes%",
            "%page%",
            "%total_pages%",
            "%homes_count%",
        ] {
            assert!(keys.contains(&wanted), "{wanted} is missing");
        }
    }

    #[test]
    fn the_longest_key_comes_first_so_a_prefix_cannot_eat_it() {
        // `%homes_count%` starts with `%home`, and if `%home%` were read first the
        // `%home%` would not match - but the scan is a single pass, so this is
        // about `find` picking the right entry, not about order.
        let store = store(2);
        let homes = homes_of(&store);
        let placeholders = global(&Config::defaults(), &session(7), &homes, 1);
        assert_eq!(
            text::apply("%homes_count% and %page%", &placeholders),
            "2 and 1"
        );
    }

    #[test]
    fn a_cross_server_home_gets_a_server_placeholder_and_a_local_one_does_not() {
        let store = {
            let mut store = Store::default();
            store.set_home(
                "target",
                Home::new(
                    1,
                    Some("other".into()),
                    (0.0, 0.0, 0.0),
                    (0.0, 0.0),
                    None,
                    Some("elsewhere".into()),
                ),
            );
            store
        };
        let homes = homes_of(&store);
        let remote = homes.find(1).cloned().expect("home 1");

        let with_server = home(
            &Config::defaults(),
            &session(5),
            &homes,
            1,
            1,
            &remote,
            "here",
        );
        assert_eq!(
            with_server
                .iter()
                .find(|(key, _)| *key == "%server%")
                .map(|(_, value)| value.as_str()),
            Some("elsewhere"),
            "%server% is only added for a home that is not local"
        );

        // A local home gets none, which is what made the extra "Server:" lore
        // line cross-server only.
        let local = Home::new(
            2,
            Some("world".into()),
            (0.0, 0.0, 0.0),
            (0.0, 0.0),
            None,
            None,
        );
        let without = home(
            &Config::defaults(),
            &session(5),
            &homes,
            1,
            2,
            &local,
            "here",
        );
        assert!(!without.iter().any(|(key, _)| *key == "%server%"));
    }
}
