//! The two screen layouts, parsed the way the Paper plugin parsed them.
//!
//! Both files describe a chest as one string per row of nine characters. The
//! Java `parseLayout` had two nearly identical loops that disagreed about what a
//! character *means*, and the difference is the point:
//!
//! - `gui/homes.yml` only looks for `h`, and pairs each one with the slot
//!   directly below it - **but only if there is a row below**. An `h` on the last
//!   row gets a home slot and no delete slot, because the loop guarded with
//!   `row + 1 < layout.size()`.
//! - `gui/confirm.yml` looks for *anything that is not `-`* and buckets every
//!   occurrence of the same character together, so an item whose `symbol:` is
//!   `h` is placed in every `h` slot. That is how the shipped confirm screen
//!   puts its paper item in slots 4 *and* 8.
//!
//! `#` is not special to either loop: the navigation row in `homes.yml` is
//! ordinary, and the previous/next buttons are placed by absolute slot from
//! `items.previous-page-slot` and `items.next-page-slot`, not by walking `#`.

use pumpkin_plugin_api_v0_2::Screen;

/// The row count a chest screen of that many rows is.
///
/// The Java plugin computed `guiRows * 9` and handed it to
/// `Bukkit.createInventory`. A port has to name the screen instead, and there is
/// exactly one screen variant per row count, so the two are the same number.
///
/// `None` when the layout asks for more rows than a chest can have - the Java
/// version would have happily built a 27-row inventory that no client can open,
/// and this one has to refuse instead.
#[must_use]
pub fn screen_for_rows(rows: usize) -> Option<Screen> {
    match rows {
        1 => Some(Screen::Generic9x1),
        2 => Some(Screen::Generic9x2),
        3 => Some(Screen::Generic9x3),
        4 => Some(Screen::Generic9x4),
        5 => Some(Screen::Generic9x5),
        6 => Some(Screen::Generic9x6),
        _ => None,
    }
}

/// The layout of a homes screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The slots a home button sits in, in template order.
    pub home_slots: Vec<u32>,
    /// The slot that carries the matching delete button, one per home slot.
    /// Shorter than `home_slots` when the last row of homes has no row below it.
    pub dye_slots: Vec<u32>,
    /// The number of rows, which is what decides the screen variant.
    pub rows: usize,
}

impl Layout {
    /// Parses `lines`, falling back to the shipped layout when it is empty.
    ///
    /// The empty-list default is `HomesGuiConfig.parseLayout`'s own, and the
    /// slots it produces are the well-known `[11, 13, 15, 28, 30, 32, 34]` that
    /// every screenshot of the plugin is a screenshot of.
    #[must_use]
    pub fn homes(lines: &[String]) -> Self {
        let owned;
        let lines: &[String] = if lines.is_empty() {
            owned = crate::config::DEFAULT_HOMES_LAYOUT
                .iter()
                .map(|line| (*line).to_string())
                .collect::<Vec<_>>();
            &owned
        } else {
            lines
        };

        Self::parse(lines)
    }

    /// The parse loop itself, shared by every layout.
    ///
    /// `dye` decides whether an `h` claims the slot below it. Both variants guard
    /// `col` with `min(len, 9)`, so a row longer than nine characters is ignored
    /// past the ninth rather than wrapping into the next row.
    #[must_use]
    pub fn parse(lines: &[String]) -> Self {
        let mut home_slots = Vec::new();
        let mut dye_slots = Vec::new();

        for (row, line) in lines.iter().enumerate() {
            let chars = line.chars().take(9).collect::<Vec<_>>();
            for (col, c) in chars.iter().enumerate() {
                if *c == 'h' {
                    let slot = row * 9 + col;
                    home_slots.push(slot as u32);
                    if row + 1 < lines.len() {
                        dye_slots.push(((row + 1) * 9 + col) as u32);
                    }
                }
            }
        }

        Self {
            home_slots,
            dye_slots,
            rows: lines.len(),
        }
    }

    /// The delete slot for the home at `index`, or `None` when it has none.
    ///
    /// `index` is the position in `home_slots`, not the home number. The last
    /// home of a screen whose bottom row is all homes has no delete button, and
    /// `addExistingHome` in the Java plugin had to cope with that too.
    #[must_use]
    pub fn dye_for(&self, index: usize) -> Option<u32> {
        self.dye_slots.get(index).copied()
    }

    /// How many homes one page holds.
    #[must_use]
    pub fn slots_per_page(&self) -> usize {
        self.home_slots.len()
    }

    /// The screen variant, or `None` when there is no row the guest can open.
    #[must_use]
    pub fn screen(&self) -> Option<Screen> {
        screen_for_rows(self.rows)
    }
}

/// The layout of the delete-confirmation screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmLayout {
    /// Every slot that carries `symbol`, in template order.
    pub slots: Vec<(char, u32)>,
    /// The number of rows.
    pub rows: usize,
}

impl ConfirmLayout {
    /// Parses `lines`, falling back to the shipped disposition when it is empty.
    #[must_use]
    pub fn parse(lines: &[String]) -> Self {
        let owned;
        let lines: &[String] = if lines.is_empty() {
            owned = crate::config::DEFAULT_CONFIRM_LAYOUT
                .iter()
                .map(|line| (*line).to_string())
                .collect::<Vec<_>>();
            &owned
        } else {
            lines
        };

        let mut slots = Vec::new();
        for (row, line) in lines.iter().enumerate() {
            for (col, c) in line.chars().take(9).enumerate() {
                if c != '-' {
                    slots.push((c, (row * 9 + col) as u32));
                }
            }
        }

        Self {
            slots,
            rows: lines.len(),
        }
    }

    /// The screen variant, or `None` for a row count a chest cannot have.
    #[must_use]
    pub fn screen(&self) -> Option<Screen> {
        screen_for_rows(self.rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped `gui/homes.yml` layout.
    fn homes() -> Vec<String> {
        [
            "---------",
            "--h-h-h--",
            "---------",
            "-h-h-h-h-",
            "---------",
            "#########",
        ]
        .iter()
        .map(|line| (*line).to_string())
        .collect()
    }

    #[test]
    fn the_shipped_layout_gives_the_slots_every_screenshot_shows() {
        let layout = Layout::homes(&homes());
        assert_eq!(layout.home_slots, vec![11, 13, 15, 28, 30, 32, 34]);
        assert_eq!(layout.dye_slots, vec![20, 22, 24, 37, 39, 41, 43]);
        assert_eq!(layout.slots_per_page(), 7);
        assert_eq!(layout.rows, 6);
    }

    #[test]
    fn every_home_row_takes_the_slot_directly_below_it() {
        // `--h-h--` in row 1 puts homes at 11 and 13 and dyes at 20 and 22, which
        // is the `row*9+col` / `(row+1)*9+col` rule, not a fixed row offset.
        let layout = Layout::parse(&["--h-h--".to_string()]);
        assert_eq!(layout.home_slots, vec![2, 4]);
        assert_eq!(
            layout.dye_slots,
            Vec::<u32>::new(),
            "no row below, so no dyes"
        );
        assert_eq!(layout.dye_for(0), None);
    }

    #[test]
    fn a_row_of_nine_is_not_allowed_to_wrap() {
        // `min(len, 9)` in the Java loop: column 9 of an over-long row belongs to
        // the next row, and skipping past nine characters is what stops it
        // silently becoming slot 9 of the same row.
        //
        // Both rows are entirely `h`, so all eighteen slots are homes and the
        // second row's claim the dyes. An unwrapped loop would have produced
        // slot 9 twice - once as row 0's ninth column and once as row 1's first.
        let layout = Layout::parse(&["hhhhhhhhh".to_string(), "hhhhhhhhh".to_string()]);
        assert_eq!(layout.home_slots, (0..18).collect::<Vec<u32>>());
        assert_eq!(layout.dye_slots, (9..18).collect::<Vec<u32>>());
        let unique: std::collections::BTreeSet<u32> = layout.home_slots.iter().copied().collect();
        assert_eq!(unique.len(), 18, "no slot appears twice");
    }

    #[test]
    fn an_empty_layout_falls_back_to_the_shipped_one() {
        // Not to an empty screen: `parseLayout`'s own default list is what an
        // admin gets when they blank the file out.
        let layout = Layout::homes(&[]);
        assert_eq!(layout, Layout::parse(&homes()));
    }

    #[test]
    fn a_screen_is_named_for_every_row_count_a_chest_can_have() {
        assert_eq!(screen_for_rows(1), Some(Screen::Generic9x1));
        assert_eq!(screen_for_rows(6), Some(Screen::Generic9x6));
        assert_eq!(screen_for_rows(7), None);
        assert_eq!(screen_for_rows(0), None);
    }

    #[test]
    fn the_confirm_layout_buckets_every_slot_with_the_same_symbol() {
        // The shipped disposition is `---------`, `--d-h-a--`, `----g----`, which
        // puts `d` at 2, `a` at 6, `g` at 11 and - because an `h` item is placed
        // in *every* slot its symbol appears in - the paper at 4 and 8.
        let confirm = ConfirmLayout::parse(&[
            "---------".to_string(),
            "--d-h-a--".to_string(),
            "----g----".to_string(),
        ]);
        let at = |symbol: char| -> Vec<u32> {
            confirm
                .slots
                .iter()
                .filter(|(c, _)| *c == symbol)
                .map(|(_, slot)| *slot)
                .collect()
        };
        assert_eq!(at('d'), vec![11]);
        assert_eq!(at('a'), vec![15]);
        assert_eq!(at('g'), vec![22]);
        assert_eq!(at('h'), vec![13]);
        // `---` is a gap, not a symbol, so it contributes nothing.
        assert!(at('-').is_empty());
        assert_eq!(confirm.rows, 3);
    }

    #[test]
    fn an_empty_confirm_layout_falls_back_to_the_shipped_one() {
        let fallback = ConfirmLayout::parse(&[]);
        assert_eq!(fallback.rows, 3);
        assert_eq!(fallback.slots.iter().filter(|(c, _)| *c == 'h').count(), 1);
    }
}
