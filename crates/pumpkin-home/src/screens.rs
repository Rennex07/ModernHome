//! Building and opening the two screens.
//!
//! ## Why this is separate from the planners
//!
//! [`crate::homes_gui`] and [`crate::confirm_gui`] decide what a screen *contains*
//! and touch nothing, so they are testable on the host target. This module is the
//! half that cannot be: it builds `ItemStack`s, hands them to a `Gui`, and gives
//! the `Gui` to the player. Off `wasm32` those wit-bindgen imports are
//! `unreachable!()` stubs, so everything in here traps rather than doing anything
//! useful - which is exactly why no decision lives in here.
//!
//! ## A screen is rebuilt every time one is opened
//!
//! `player.open_gui(gui)` consumes the `Gui`, and there is no `get_gui()` back off
//! the player, so there is nothing to edit in place even if there were. A screen is
//! assembled from the session each time. Paper did the same: `openNewPage()`
//! unregistered the old inventory, built a brand-new one and opened it, keeping
//! only the page number and the loaded homes.
//!
//! ## One session, and which screen it is
//!
//! `InventoryClickEventData` carries no handle back to the `Gui` that was clicked,
//! only the screen *kind*, so the plugin cannot ask "which of my menus was this?".
//! The answer is [`Session::open`], set by whichever screen was opened last and
//! dropped when that screen closes. A viewer only ever has one open, so the
//! question has one answer.

use pumpkin_plugin_api_v0_2::ItemStack;
use pumpkin_plugin_api_v0_2::Screen;
use pumpkin_plugin_api_v0_2::common::ClickType;
use pumpkin_plugin_api_v0_2::gui::Gui;
use pumpkin_plugin_api_v0_2::java_packets::{CCloseContainer, ClientboundPacket};
use pumpkin_plugin_api_v0_2::player::Player;
use pumpkin_plugin_api_v0_2::text::TextComponent;

use crate::{
    config::Config,
    confirm_gui,
    homes::{Home, PlayerHomes},
    homes_gui::{self, Action, Grid, Slot},
    items, messages, permissions, sounds,
    state::{self, Session},
    store::Store,
    text,
};

/// The countdown's first tick, in ticks.
///
/// One tick, not zero: the host arms a repeating task's first run at
/// `current_tick + delay` and a zero delay is a task that can fire before the
/// state it needs exists.
pub const COUNTDOWN_DELAY: u64 = 1;

/// The countdown's period, in ticks. Twenty is one second at 20 TPS.
///
/// The same pair Paper's `runAtEntityTimer(player, tick, 1L, 20L)` used, and the
/// host honours both numbers, so the cadence is the same rather than a
/// reimplementation of it.
pub const COUNTDOWN_PERIOD: u64 = 20;

/// The viewer's target's homes, read out of the store.
///
/// One closure, so the store lock is released before a single item is built - the
/// store's own header makes the same promise about not holding a lock across
/// anything else.
fn homes_of(session: &Session) -> PlayerHomes {
    state::with_store(|store: &mut Store| store.with_player(&session.target, Clone::clone))
}

/// The installed configuration, or the shipped defaults.
///
/// Every builder takes a `&Config` so it is explicit, and this is the one place
/// that reaches for the installed copy. It needs an answer for the case where the
/// plugin has not finished loading, which a scheduled task can reach: the host does
/// not unwind a failed load, so a task armed by `on_load` can outlive the error.
pub fn config() -> std::sync::Arc<Config> {
    state::config().unwrap_or_else(|| std::sync::Arc::new(Config::defaults()))
}

/// The homes screen, decided, for a session that is already open.
pub fn grid(config: &Config, session: &Session) -> Grid {
    homes_gui::plan(config, &homes_of(session), session, config.homes_layout())
}

/// What a click on the homes screen means.
pub fn homes_click(grid: &Grid, slot: u32, click: ClickType) -> Option<Action> {
    homes_gui::click(grid, slot, click)
}

/// Opens the homes screen for `viewer`, showing `target`'s homes.
///
/// `target_name` is used for `%target%` and for the message a delete prints, which
/// is what makes `/ahome osee <player>` work for somebody who has never joined and
/// so has no handle.
pub async fn show_homes(viewer: &Player, target: String, target_name: String) {
    let config = config();
    let session = Session::homes(
        state::uuid_of(viewer),
        viewer.get_name(),
        target,
        target_name,
        permissions::max_homes(viewer, config.limit_scan_max()).await,
    );
    render_homes(&config, viewer, &session).await;
    sounds::play(&config, viewer, "ui-open");
}

/// Opens `gui` for `viewer`, marking the viewer for the duration.
///
/// The host fires `InventoryCloseEvent` for the screen being replaced *inside* this
/// call, so the viewer has to be marked before it and unmarked after - otherwise the
/// close handler drops the session this very call is about to install, and the
/// screen opens once and then ignores every click. See [`crate::state::begin_reopen`].
async fn open_for(viewer: &Player, gui: Gui) {
    let viewer_uuid = state::uuid_of(viewer);
    state::begin_reopen(&viewer_uuid);
    viewer.open_gui(gui).await;
    state::end_reopen(&viewer_uuid);
}

/// Shuts whatever window `viewer` has open, by hand.
///
/// Paper closed the screen itself when a rename started (`viewer.closeInventory()`
/// at `HomeGuiHandler.java:401`), so the player was looking at the world when the
/// prompt asked for a name. There is no `close-inventory` in the v0.1 WIT, but the
/// same packet can be written by hand: `java-player.send-packet` at `player.wit:1351`
/// takes the whole `clientbound-packet` enum and `Close Container` is one of its
/// variants, and a Java player is reachable from any player through `as-java`.
///
/// The catch is the window id. `increment_screen_handler_sync_id`
/// (`entity/player.rs:5632-5636`) makes it `current % 100 + 1`, so an open window
/// is always somewhere in `1..=100`, and `0` is the player's own inventory - and
/// nothing in the WIT reports which one is open. So this sends the close for every
/// id rather than guessing one: a close for a window the client does not have open
/// is discarded, and the one that matches is the one that closes the screen.
///
/// 101 packets is wasteful, and this runs once per right-click on a home, so it is
/// not on any path where it would be felt. A `sync-id` accessor on the player
/// resource would let this send exactly one.
///
/// A Bedrock player has no `as-java`, so this does nothing for them and they close
/// the screen themselves.
pub async fn close_screen(viewer: &Player) {
    let Some(java) = viewer.as_java() else {
        return;
    };
    for sync_id in 0..=100 {
        java.send_packet(ClientboundPacket::CCloseContainer(CCloseContainer {
            sync_id,
        }))
        .await;
    }
}

/// Rebuilds the homes screen from a session, shows it, and records it as the
/// viewer's open screen.
///
/// Everything that comes back to the homes screen goes through here: a page
/// change, a rename, a delete, and the confirmation screen's cancel and back
/// buttons. Each of those built a *new* `HomeGuiHandler` in Paper, and a new one
/// started at page one - so a caller that did not move the page must not.
///
/// The recorded session is `as_homes`, not the one passed in. Returning from the
/// confirmation screen arrives here carrying `open: Confirm`, and installing that
/// verbatim is a homes screen that renders and then ignores every click, because
/// clicks are routed by `open`.
pub async fn render_homes(config: &Config, viewer: &Player, session: &Session) {
    // The session goes in first: `open_for` re-enters this plugin to fire the
    // close event, and the close handler skips a marked viewer rather than
    // clearing this one.
    state::set_session(session.as_homes());
    open_for(viewer, build_homes(config, session).await).await;
}

/// The homes screen a page change produces.
///
/// The only thing that moves on its own. `openNewPage` in Paper decremented or
/// incremented `currentPage` and then swapped the inventory, keeping everything
/// else, so the page number is the one piece of state that survives a screen swap.
///
/// The `clamp(-1, 1)` is what a single click can actually be; the planner has
/// already refused to produce a next button on the last page, so this can only
/// matter if the limit shrank under a viewer already on page nine.
pub fn turned(session: &Session, delta: i64) -> Session {
    let mut next = session.clone();
    next.page = session
        .page
        .saturating_add_signed(delta.clamp(-1i64, 1i64) as isize)
        .max(1);
    next
}

pub fn first_page(session: &Session) -> Session {
    let mut next = session.as_homes();
    next.page = 1;
    next
}

/// Builds the homes screen's `Gui`.
async fn build_homes(config: &Config, session: &Session) -> Gui {
    let layout = config.homes_layout();
    let homes = homes_of(session);
    let grid = homes_gui::plan(config, &homes, session, layout);
    let pages = grid.total_pages;
    let server_id = config.server_id();

    // `allow_grab_items` and `allow_put_items` are deliberately never set. Paper set
    // neither; nothing comes out of these screens because `OnClick` sets
    // `cancelled = true` before it dispatches, which is what Paper did.
    //
    // They cannot be the cause of the cursor jumping to the centre on every update,
    // which is worth writing down because it was the first thing blamed: both flags
    // are read in exactly one place, the `on_slot_click` guard at
    // `entity/player.rs:6256-6301`, and every read is `if !flag { cancel(); return }`.
    // Nothing serialises them, so they never reach the client and cannot move a
    // pointer.
    let gui = Gui::new(
        layout.screen().unwrap_or(Screen::Generic9x6),
        TextComponent::from_legacy_string_with_code(
            &text::decorate(&homes_gui::title(config, session)),
            text::CODE,
        ),
    );

    for (index, cell) in grid.cells.iter().enumerate() {
        let Some(bed) = grid.home_slots.get(index).copied() else {
            continue;
        };

        let placeholders = match cell {
            Slot::Existing(home) => homes_gui::home(
                config,
                session,
                &homes,
                pages,
                home.home_number,
                home,
                &server_id,
            ),
            Slot::Empty(number) => homes_gui::empty(config, session, &homes, pages, *number),
            Slot::Locked => homes_gui::global(config, session, &homes, pages),
        };

        let (bed_key, dye_key) = match cell {
            Slot::Existing(_) => ("tp-home-bed", "delete-home"),
            Slot::Empty(_) => ("no-home-bed", "no-home"),
            Slot::Locked => ("no-perm-bed", "no-perm"),
        };

        let bed_item = item(config.homes_gui(), bed_key, &placeholders, true).await;

        // The one thing an item carries that no config file describes: a
        // cross-server home's bed gains a line naming the server its home is on.
        // The dye does not, and `add_lore` appends to the configured lore rather
        // than replacing it, which is what the Java copy-then-append did.
        if let Slot::Existing(home) = cell
            && !home.is_local(&server_id)
        {
            bed_item
                .add_lore(items::component(&format!(
                    "&8Server: &7{}",
                    home.server_or_empty()
                )))
                .await;
        }
        gui.set_item(bed, bed_item).await;

        // A bed on the bottom row of the layout has no row below it, so its home
        // has no delete button. The `if let` is that guard, which the Java
        // `if (dyeSlot >= 0)` also was.
        if let Some(dye) = grid.dye_slots.get(index).copied().flatten() {
            gui.set_item(
                dye,
                item(config.homes_gui(), dye_key, &placeholders, true).await,
            )
            .await;
        }
    }

    let global = homes_gui::global(config, session, &homes, pages);
    if let Some(slot) = grid.previous {
        gui.set_item(
            slot,
            item(config.homes_gui(), "previous-page", &global, true).await,
        )
        .await;
    }
    if let Some(slot) = grid.next {
        gui.set_item(
            slot,
            item(config.homes_gui(), "next-page", &global, true).await,
        )
        .await;
    }

    // Fillers last, over everything, with no button behind them. A filler that
    // lands on a bed hides it while the bed's action still runs - the same thing
    // happened in Java, because the button map was never cleared before the
    // fillers were placed over it.
    for filler in items::fillers(config.homes_gui()) {
        if (filler.slot as usize) < layout.rows * 9 {
            gui.set_item(filler.slot, filler_item(&filler, &global).await)
                .await;
        }
    }

    gui
}

/// Opens the delete-confirmation screen for `home`.
pub async fn show_confirm(viewer: &Player, session: &Session, home: &Home) {
    let config = config();
    let confirm = Session::confirm(session, home.clone());
    state::set_session(confirm);
    open_for(viewer, build_confirm(&config, session, home).await).await;
}

/// Builds the confirmation screen's `Gui`.
async fn build_confirm(config: &Config, session: &Session, home: &Home) -> Gui {
    let layout = config.confirm_layout();
    let placeholders = confirm_gui::placeholders(&session.viewer_name, &session.target_name, home);

    let gui = Gui::new(
        layout.screen().unwrap_or(Screen::Generic9x3),
        TextComponent::from_legacy_string_with_code(
            &text::decorate(&config.confirm_title()),
            text::CODE,
        ),
    );
    // Left at the default, for the reason given in `build_homes`.
    gui.set_allow_grab_items(false).await;
    gui.set_allow_put_items(false).await;

    for cell in confirm_gui::plan(config.confirm_gui(), layout).cells {
        let stack = match &cell.item {
            confirm_gui::Item::Configured(key) => {
                item(config.confirm_gui(), key, &placeholders, false).await
            }
            confirm_gui::Item::HomeDisplay => summary(home).await,
        };
        gui.set_item(cell.slot, stack).await;
    }

    for filler in items::fillers(config.confirm_gui()) {
        if (filler.slot as usize) < layout.rows * 9 {
            gui.set_item(filler.slot, filler_item(&filler, &placeholders).await)
                .await;
        }
    }

    gui
}

/// What a click on the confirmation screen means.
///
/// The session is not needed: the confirmation screen's buttons come from the
/// layout and the config alone. `DeleteConfirmGui.onClick` looked its own handler up
/// by inventory, and the buttons were on that inventory rather than on the home
/// being deleted, so nothing about *which* home is pending reaches a click.
pub fn confirm_click(slot: u32) -> Option<confirm_gui::Button> {
    let config = config();
    let layout = config.confirm_layout();
    if (slot as usize) >= layout.rows * 9 {
        return None;
    }
    confirm_gui::plan(config.confirm_gui(), layout).button_at(slot)
}

/// The home summary, which is built in code and is not configurable.
///
/// A sheet of paper named `"&9Home <number> <name>"` with the world and the
/// coordinates below it. An unnamed home's name ends in a space, because the Java
/// string concatenation put an empty custom name between two spaces.
async fn summary(home: &Home) -> ItemStack {
    let stack = items::of("PAPER").await;
    stack
        .set_custom_name(Some(items::component(&confirm_gui::summary_name(home))))
        .await;
    let [world, coords] = confirm_gui::summary_lore(home);
    let world = TextComponent::from_legacy_string_with_code(&world, text::CODE);
    let coords = TextComponent::from_legacy_string_with_code(&coords, text::CODE);
    stack
        .set_lore(vec![items::plain(world), items::plain(coords)])
        .await;
    stack
}

/// Builds one configured item, or a plain stone when the section is missing.
///
/// `HomesGuiConfig.createItem` returned `new ItemStack(Material.STONE)` for a path
/// that was not in the file, so a typo in a key is a visible stone rather than an
/// empty slot.
///
/// `always_name` chooses between the two Java rules that differed by one
/// `isEmpty()`: the homes screen's own items set the display name whenever the key
/// exists, while the confirmation screen's and the fillers set it only when it is
/// non-empty.
async fn item(
    root: &serde_norway::Value,
    key: &str,
    placeholders: &[(&'static str, String)],
    always_name: bool,
) -> ItemStack {
    match root.get("items").and_then(|section| section.get(key)) {
        Some(section) => items::build_item(section, placeholders, always_name).await,
        None => items::of("STONE").await,
    }
}

/// Builds a filler, which is a slot and a shape rather than a configured section.
async fn filler_item(filler: &items::Filler, placeholders: &[(&'static str, String)]) -> ItemStack {
    use serde_norway::{Mapping, Value};

    let mut section = Mapping::new();
    section.insert(
        Value::String("material".into()),
        Value::String(filler.material.clone()),
    );
    section.insert(
        Value::String("name".into()),
        Value::String(filler.name.clone()),
    );
    section.insert(
        Value::String("lore".into()),
        Value::Sequence(filler.lore.iter().cloned().map(Value::String).collect()),
    );
    // Never `always_name`: a filler with an empty `name:` shows nothing.
    items::build_item(&Value::Mapping(section), placeholders, false).await
}

/// A message to the viewer, if chat is allowed.
pub fn say(config: &Config, viewer: &Player, key: &str, placeholders: &[(&'static str, String)]) {
    if config.allow_chat() {
        messages::chat(config, viewer, key, placeholders);
    }
}

/// A message to the viewer on the actionbar, if that is allowed.
pub fn show(config: &Config, viewer: &Player, key: &str, placeholders: &[(&'static str, String)]) {
    if config.allow_actionbar() {
        messages::actionbar(config, viewer, key, placeholders);
    }
}

/// Send the rename prompts configured for the homes screen.
pub fn rename_prompt(config: &Config, viewer: &Player) {
    if config.allow_chat() {
        let text = text::decorate(&config.rename_prompt());
        viewer.send_system_message(
            TextComponent::from_legacy_string_with_code(&text, text::CODE),
            false,
        );
    }
    if config.allow_actionbar() {
        let text = text::decorate(&config.rename_prompt_actionbar());
        viewer.show_actionbar(TextComponent::from_legacy_string_with_code(
            &text,
            text::CODE,
        ));
    }
}

/// A message on both channels, which is what a cancelled countdown was.
pub fn announce(config: &Config, viewer: &Player, key: &str) {
    messages::cancel(config, viewer, key, &[]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deletion_returns_to_page_one_from_any_page() {
        let mut before = session(4);
        before.open = crate::state::Open::Confirm;
        let after = first_page(&before);
        assert_eq!(after.page, 1);
        assert_eq!(after.open, crate::state::Open::Homes);
        assert_eq!(after.target, before.target);
        assert!(after.pending_delete.is_none());
    }

    fn session(page: usize) -> Session {
        Session::homes(
            "viewer".into(),
            "Viewer".into(),
            "target".into(),
            "Target".into(),
            5,
        )
        .tap(page)
    }

    /// Small helper so the page can be set at construction, which is all this
    /// module's only pure function needs.
    trait Tap: Sized {
        fn tap(mut self, page: usize) -> Self {
            self.page_mut(page);
            self
        }
        fn page_mut(&mut self, page: usize);
    }

    impl Tap for Session {
        fn page_mut(&mut self, page: usize) {
            self.page = page;
        }
    }

    #[test]
    fn a_page_turn_clamps_to_the_edges() {
        // A click on a next button that is not there never reaches this, so the
        // only clamp that can matter is a reloaded config that shortened the
        // limit under a viewer already on page nine.
        assert_eq!(turned(&session(1), 1).page, 2);
        assert_eq!(turned(&session(2), -1).page, 1);
        assert_eq!(
            turned(&session(1), -1).page,
            1,
            "page one cannot go below one"
        );
        assert_eq!(turned(&session(1), 99).page, 2, "one click is one page");
        assert_eq!(turned(&session(1), -99).page, 1);
    }

    #[test]
    fn a_page_turn_keeps_everything_else() {
        let before = session(3);
        let after = turned(&before, 1);
        assert_eq!(after.target, before.target);
        assert_eq!(after.target_name, before.target_name);
        assert_eq!(after.max_homes, before.max_homes);
        assert_eq!(after.open, before.open);
    }

    #[test]
    fn the_countdown_cadence_is_one_tick_out_then_twenty() {
        // The same pair Paper's `runAtEntityTimer(player, tick, 1L, 20L)` used.
        assert_eq!(COUNTDOWN_DELAY, 1);
        assert_eq!(COUNTDOWN_PERIOD, 20);
    }
}
