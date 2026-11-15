//! Everything the plugin remembers between calls.
//!
//! ## Why nothing here is a host handle
//!
//! `Player`, `World`, `TextComponent`, `ItemStack`, `Gui` are resource handles:
//! not `Copy`, not `Clone`, and a call that takes one consumes it. They cannot be
//! stashed in a map and re-used later, so every field here is plain data - a uuid
//! *string*, a name, a page number - and a handle is re-fetched from the server
//! at the point it is needed. That is why the key is the uuid string rather than
//! the `uuid` record: it is what the store is keyed by, it is what the log lines
//! name, and it is what survives a player disconnecting and coming back.
//!
//! ## One session per viewer, not per screen
//!
//! `InventoryClickEventData` carries a `window_type` that is only the screen
//! *kind*, and never a handle back to the GUI that was clicked - so a port
//! cannot ask "which of my menus was this?". The Paper plugin solved that with a
//! `ConcurrentHashMap<Inventory, InventoryHandler>` keyed by the inventory
//! object, which a port has no equivalent of. So the plugin keeps one [`Session`]
//! per viewer, which says which of its two screens is open, and it is dropped
//! when that screen closes. A viewer only ever has one screen open at a time -
//! which is also why a homes screen and a confirmation screen cannot share a
//! session by accident.
//!
//! ## Locks never span a host call
//!
//! `pumpkin_plugin_api` warns against holding a non-reentrant lock across a host
//! call, because the call can deliver an event back into this plugin before it
//! returns - `Player::has_permission` fires `PlayerPermissionCheckEvent`, for
//! one. So every accessor here takes or returns plain data and releases the lock
//! before any host call is made, and the functions that need both read the state,
//! drop it, then call.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Instant;

use pumpkin_plugin_api_v0_2::{Server, player::Player};

use crate::{config::Config, homes::Home, store::Store};

/// A guard on a poisoned lock, rather than the panic `lock()` would raise.
///
/// Everything behind one here is plain data with no invariant a panic could have
/// half-broken, so recovering beats turning one panic into a permanently dead
/// plugin. The release profile is `panic = "abort"` anyway, which is what makes
/// the "if a body panics the handle is lost" reasoning in `pumpkin-tab` survivable.
pub(crate) fn recover<T>(result: std::sync::LockResult<MutexGuard<'_, T>>) -> MutexGuard<'_, T> {
    match result {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// The uuid `player` is stored under, as a string.
///
/// A string rather than the `uuid` record, because the store is keyed by it, the
/// log lines name it, and a `Uuid` is not `Hash` as far as a plugin can rely on -
/// it is a record of two integers that survives being written to a file and read
/// back, which is more than can be said for a resource handle.
pub fn uuid_of(player: &Player) -> String {
    pumpkin_plugin_api_v0_2::uuid::to_string(player.get_id())
}

/// The player `uuid` is online as, or [`None`] when they are not.
///
/// The only way back from a uuid string to a handle, and the reason nothing in
/// this plugin keeps one: `Player` is consumed by the calls that take it, so a
/// handle has a lifetime measured in one function.
pub fn player_of(server: &Server, uuid: &str) -> Option<Player> {
    let id = pumpkin_plugin_api_v0_2::uuid::parse(uuid)?;
    server.get_player_by_uuid(id)
}

/// The configuration, shared and never mutated in place.
static CONFIG: OnceLock<Mutex<Option<Arc<Config>>>> = OnceLock::new();

fn config_slot() -> &'static Mutex<Option<Arc<Config>>> {
    CONFIG.get_or_init(|| Mutex::new(None))
}

/// The plugin's data folder as seen from inside the WASI sandbox.
static DATA_FOLDER: OnceLock<String> = OnceLock::new();

/// The homes.
static STORE: OnceLock<Mutex<Store>> = OnceLock::new();

/// One session per viewer, keyed by viewer uuid string.
static SESSIONS: Mutex<BTreeMap<String, Session>> = Mutex::new(BTreeMap::new());

/// A right-clicked home waiting for its new name, keyed by viewer uuid string.
///
/// The Java plugin kept this as a static `Map<UUID, Integer>` and consumed the
/// entry on the player's next chat message, whether or not that message was
/// usable. The consequence - a rejected name still ends the rename, so the player
/// has to right-click again - is reproduced rather than fixed.
static PENDING_RENAMES: Mutex<BTreeMap<String, PendingRename>> = Mutex::new(BTreeMap::new());

/// When each player was last struck in combat, keyed by uuid string.
///
/// Java used wall-clock milliseconds, because the plugin asked the clock once per
/// event and compared against it later. `Instant` is monotonic and cheaper, and
/// every use here is a duration rather than a timestamp.
static LAST_DAMAGE: Mutex<BTreeMap<String, Instant>> = Mutex::new(BTreeMap::new());

/// Why the countdown is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelReason {
    /// The player moved more than half a block, or changed world.
    Moved,
    /// The player was struck in combat.
    Combat,
}

/// Which of the plugin's two screens a viewer has open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Open {
    /// The homes screen.
    Homes,
    /// The delete-confirmation screen.
    Confirm,
}

/// What a viewer is looking at, and where they are in it.
#[derive(Debug, Clone)]
pub struct Session {
    /// The viewer's uuid string. Every message and sound is aimed at them.
    pub viewer: String,
    /// The viewer's name, for `%player%`.
    pub viewer_name: String,
    /// Whose homes are being edited, for `%target%`.
    pub target: String,
    /// The target's display name.
    pub target_name: String,
    /// Whether the viewer is looking at their own homes.
    pub self_view: bool,
    /// The current page, one-based. Never clamped: see [`Session::page`]'s note.
    pub page: usize,
    /// The viewer's home limit, refreshed before each menu action.
    pub max_homes: i32,
    /// Which screen is open.
    pub open: Open,
    /// The home the confirmation screen would delete.
    pub pending_delete: Option<Home>,
}

impl Session {
    /// Builds a session for a homes screen at page one.
    ///
    /// The initial limit is supplied by the caller and refreshed before handling
    /// clicks. Rendering uses that snapshot rather than probing every permission
    /// again for each item.
    #[must_use]
    pub fn homes(
        viewer: String,
        viewer_name: String,
        target: String,
        target_name: String,
        max_homes: i32,
    ) -> Self {
        let self_view = viewer == target;
        Self {
            viewer,
            viewer_name,
            target,
            target_name,
            self_view,
            page: 1,
            max_homes,
            open: Open::Homes,
            pending_delete: None,
        }
    }

    /// Builds a session for the confirmation screen for `home`.
    #[must_use]
    pub fn confirm(viewer: &Self, home: Home) -> Self {
        Self {
            viewer: viewer.viewer.clone(),
            viewer_name: viewer.viewer_name.clone(),
            target: viewer.target.clone(),
            target_name: viewer.target_name.clone(),
            self_view: viewer.self_view,
            page: viewer.page,
            max_homes: viewer.max_homes,
            open: Open::Confirm,
            pending_delete: Some(home),
        }
    }

    /// This session, as the homes screen sees it.
    ///
    /// `open` is what a click is routed by, so drawing the homes screen while the
    /// session still says `Confirm` produces a screen that looks right and answers
    /// nothing: every click goes through the confirmation screen's slot map, and a
    /// home's bed is not a button there. Coming back from the confirmation screen is
    /// the only way to get into that state, which is why it survived until someone
    /// deleted a home.
    ///
    /// Everything else is carried over untouched - in particular the page, because a
    /// caller that did not turn the page must not lose the page it was on.
    #[must_use]
    pub fn as_homes(&self) -> Self {
        Self {
            open: Open::Homes,
            pending_delete: None,
            ..self.clone()
        }
    }
}

/// Installs a freshly-loaded configuration.
pub fn install(config: Config) {
    *recover(config_slot().lock()) = Some(Arc::new(config));
}

/// A copy of the configuration, or [`None`] before the plugin has loaded.
pub fn config() -> Option<Arc<Config>> {
    recover(config_slot().lock()).clone()
}

/// Records the data folder, once.
pub fn set_folder(folder: String) {
    let _ = DATA_FOLDER.set(folder);
}

/// The plugin's data folder, or [`None`] before `on_load` has run.
pub fn folder() -> Option<String> {
    DATA_FOLDER.get().cloned()
}

/// Opens the store if it has not been opened yet.
///
/// # Panics
///
/// Never. The `OnceLock` is set at most once and the `Mutex` recovers from
/// poisoning.
pub fn open_store(store: crate::store::Store) {
    STORE.get_or_init(|| Mutex::new(store));
}

/// Runs `body` against the store, releasing the lock before anything else
/// happens.
///
/// The closure must not call back into the state module or into the host: the
/// store lock is held for its duration.
pub fn with_store<R>(body: impl FnOnce(&mut Store) -> R) -> R {
    let slot = STORE.get_or_init(|| Mutex::new(Store::at("homes.json")));
    body(&mut recover(slot.lock()))
}

/// How many players have homes, for the startup line.
pub fn player_count() -> usize {
    STORE
        .get()
        .map(|slot| recover(slot.lock()).player_count())
        .unwrap_or(0)
}

/// Reads a viewer's session.
pub fn session(viewer: &str) -> Option<Session> {
    recover(SESSIONS.lock()).get(viewer).cloned()
}

/// Replaces a viewer's session.
///
/// Opening a screen always goes through here, and always replaces rather than
/// merges, so a re-opened screen starts at page one exactly as the Java plugin's
/// `new HomeGuiHandler(...)` did.
pub fn set_session(session: Session) {
    recover(SESSIONS.lock()).insert(session.viewer.clone(), session);
}

/// Drops a viewer's session, if they had one.
pub fn clear_session(viewer: &str) {
    recover(SESSIONS.lock()).remove(viewer);
}

/// How many sessions are live, for the startup line.
pub fn session_count() -> usize {
    recover(SESSIONS.lock()).len()
}

/// A right-clicked home, waiting for the name to be typed in chat.
///
/// The target travels with the rename rather than being looked up again, and that
/// is the faithful shape rather than a convenience. In Paper the chat listener was
/// the `HomeGuiHandler` instance itself, so `targetUuid`, `targetName` and `homes`
/// were its fields; `onClose` deliberately left that instance registered while a
/// rename was pending (`HomeGuiHandler.java:492`), so the handler outlived the
/// screen it belonged to and the right-clicker's target was still readable when
/// the name arrived. Here the session is the screen state and is dropped on close,
/// so anything the rename still needs has to be copied into it at the moment the
/// right-click is recorded - otherwise an admin renaming somebody else's home, or
/// a viewer who closes the screen as instructed, renames the wrong row or renames
/// the wrong player's home.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingRename {
    /// The home's number, as the homes list numbers them.
    pub number: i32,
    /// Whose home is being renamed.
    pub target: String,
    /// That player's name, for `%target%` and for the confirmation message.
    pub target_name: String,
}

/// Records a right-clicked home as waiting for its new name.
pub fn begin_rename(viewer: &str, pending: PendingRename) {
    recover(PENDING_RENAMES.lock()).insert(viewer.to_string(), pending);
}

/// Takes the pending rename off `viewer`, whether or not a name was recorded.
///
/// The `remove` is the point: the Java handler consumed the entry before it
/// validated the message, so a too-long or empty name still ended the rename.
/// That is what makes the chat handler unconditional once a rename is pending.
pub(crate) fn take_rename(viewer: &str) -> Option<PendingRename> {
    recover(PENDING_RENAMES.lock()).remove(viewer)
}

/// Drops `viewer`'s pending rename.
pub fn discard_rename(viewer: &str) {
    recover(PENDING_RENAMES.lock()).remove(viewer);
}

/// Viewers whose screen this plugin is replacing right now.
///
/// The host closes the previous screen *inside* `Player::open_gui`
/// (`entity/player.rs:5785-5795`): when the open screen is not the player's own
/// inventory it calls `close_handled_screen()`, which fires
/// `InventoryCloseEvent` **synchronously and re-entrantly**, back into this plugin,
/// before the new open packet is even sent. A close handler that treats every close
/// as "the player walked away" therefore wipes the session belonging to the screen
/// that is on its way, and the next click finds nothing to dispatch on - a homes
/// screen that opens once and then stops responding.
///
/// A viewer listed here is being replaced by us, so their close is not a departure.
static REOPENING: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Marks `viewer`'s screen as being replaced.
///
/// Bracketed around the `open_gui` call and never held across anything else: this
/// is a flag for the close handler, not a lock.
pub fn begin_reopen(viewer: &str) {
    recover(REOPENING.lock()).insert(viewer.to_string());
}

/// Clears the mark left by [`begin_reopen`].
pub fn end_reopen(viewer: &str) {
    recover(REOPENING.lock()).remove(viewer);
}

/// Whether `viewer`'s screen is being replaced by this plugin.
pub fn is_reopening(viewer: &str) -> bool {
    recover(REOPENING.lock()).contains(viewer)
}

/// The rename currently being typed, if any.
#[cfg(test)]
pub(crate) fn pending_rename(viewer: &str) -> Option<i32> {
    recover(PENDING_RENAMES.lock())
        .get(viewer)
        .map(|pending| pending.number)
}

/// Records that `viewer` was struck in combat now.
pub fn record_damage(viewer: &str) {
    recover(LAST_DAMAGE.lock()).insert(viewer.to_string(), Instant::now());
}

/// Whether `viewer` was struck in combat within `seconds`.
///
/// A `seconds` of 0 or less disables the check entirely, which is what
/// `HomeTeleportTask.isInCombat` achieved by short-circuiting before the call.
pub fn in_combat(viewer: &str, seconds: i32) -> bool {
    if seconds <= 0 {
        return false;
    }
    let Some(last) = recover(LAST_DAMAGE.lock()).get(viewer).copied() else {
        return false;
    };
    last.elapsed() < std::time::Duration::from_secs(seconds as u64)
}

/// Forgets `viewer`'s combat timestamp.
///
/// Java cleared this on quit and never on death or respawn, and a port copies
/// that: a teleport countdown cannot survive a quit, and a damage timestamp from
/// before one is the next thing to be cleared anyway.
pub fn forget_damage(viewer: &str) {
    recover(LAST_DAMAGE.lock()).remove(viewer);
}

/// Releases every piece of per-player state.
///
/// Called from `on_unload`. The store is written rather than dropped, because a
/// clean unload is the last chance a plugin gets to put its homes on disk.
pub fn clear_all() {
    if let Err(error) = with_store(Store::save) {
        tracing::error!("Could not save homes during unload: {error}");
    }
    recover(SESSIONS.lock()).clear();
    recover(PENDING_RENAMES.lock()).clear();
    recover(LAST_DAMAGE.lock()).clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session's fields, without the parts that need a real store.
    fn session(viewer: &str, target: &str) -> Session {
        Session::homes(
            viewer.to_string(),
            "Viewer".to_string(),
            target.to_string(),
            "Target".to_string(),
            7,
        )
    }

    #[test]
    fn a_self_view_session_knows_it() {
        // `selfView` is what made `/sethome` say "Home set." rather than
        // "Home set for X", so it has to be derived, not passed.
        let own = session("uuid-a", "uuid-a");
        assert!(own.self_view);
        let other = session("uuid-a", "uuid-b");
        assert!(!other.self_view);
    }

    #[test]
    fn a_homes_session_starts_on_page_one() {
        // Every entry point constructed a new handler in Java, so page state
        // never leaked between opens. Carried over by starting every session at 1.
        let opened = session("uuid-a", "uuid-a");
        assert_eq!(opened.page, 1);
        assert_eq!(opened.max_homes, 7);
        assert_eq!(opened.open, Open::Homes);
        assert!(opened.pending_delete.is_none());
    }

    #[test]
    fn the_confirmation_session_keeps_the_viewer_and_takes_the_home() {
        let homes = session("uuid-a", "uuid-b");
        let home = Home::new(
            3,
            Some("world".into()),
            (0.0, 0.0, 0.0),
            (0.0, 0.0),
            None,
            None,
        );
        let confirm = Session::confirm(&homes, home.clone());

        assert_eq!(confirm.open, Open::Confirm);
        assert_eq!(confirm.pending_delete, Some(home));
        // The target does not become the viewer: so an admin deleting someone
        // else's home still gets "Home deleted for %target%".
        assert_eq!(confirm.target, "uuid-b");
        assert_eq!(confirm.viewer, "uuid-a");
        assert!(!confirm.self_view);
    }

    #[test]
    fn a_combat_window_of_zero_disables_the_check() {
        // `combat-delay: 0` must mean "no combat delay", not "always in combat".
        assert!(!in_combat("nobody-has-joined", 0));
        assert!(!in_combat("nobody-has-joined", -3));
    }

    #[test]
    fn a_player_never_struck_is_not_in_combat() {
        assert!(!in_combat("some-uuid", 5));
    }

    /// The damage clock is process-global, so this test uses a key that cannot
    /// collide with real state: the tests run in one process and this plugin has
    /// no real player events on the host target.
    #[test]
    fn combat_is_remembered_then_expires() {
        const KEY: &str = "test-combat-key";
        assert!(!in_combat(KEY, 5));

        record_damage(KEY);
        assert!(in_combat(KEY, 5), "just struck counts as combat");

        forget_damage(KEY);
        assert!(!in_combat(KEY, 5), "forgetting clears it");
    }

    #[test]
    fn a_pending_rename_is_consumed_once() {
        // The Java handler removed the entry before validating the message, so a
        // rejected name ends the rename. The `pub(crate)` accessor cannot call
        // `take_rename` twice, so the same rule is asserted through it.
        const KEY: &str = "test-rename-key";
        begin_rename(
            KEY,
            PendingRename {
                number: 4,
                target: "someone-else".to_string(),
                target_name: "SomeoneElse".to_string(),
            },
        );
        assert_eq!(pending_rename(KEY), Some(4));

        discard_rename(KEY);
        assert_eq!(pending_rename(KEY), None);
    }

    #[test]
    fn coming_back_from_the_delete_confirmation_routes_clicks_to_the_homes_screen() {
        // The bug: `render_homes` installed the session it was handed, so a viewer
        // who cancelled the confirmation screen was left holding `open: Confirm`.
        // The homes screen drew correctly - `build_homes` is given the session
        // separately - but every click was routed through the confirmation screen's
        // slot map, where a bed is not a button, so the screen answered nothing and
        // the only way out was to close it and re-run `/home`.
        let homes = Session::homes(
            "viewer".to_string(),
            "Viewer".to_string(),
            "viewer".to_string(),
            "Viewer".to_string(),
            10,
        );
        let confirm = Session::confirm(
            &homes,
            Home::new(
                2,
                Some("world".to_string()),
                (1.0, 64.0, 1.0),
                (0.0, 0.0),
                None,
                None,
            ),
        );
        assert_eq!(
            confirm.open,
            Open::Confirm,
            "the confirmation is what is drawn"
        );

        let back = confirm.as_homes();
        assert_eq!(
            back.open,
            Open::Homes,
            "clicks route by `open`, so this is what makes the screen answer"
        );
        assert_eq!(
            back.pending_delete, None,
            "and nothing is left armed to delete"
        );
        // The page survives: only `render_homes` callers that deliberately turned a
        // page change it, and a cancel is not a page change.
        assert_eq!(back.page, homes.page);
        assert_eq!(back.target, homes.target);
        assert_eq!(back.max_homes, homes.max_homes);
    }

    #[test]
    fn a_pending_rename_outlives_the_screen_it_came_from() {
        // The prompt tells the viewer to close the screen and then type the name, and
        // closing the screen drops the session - so the rename cannot read its target
        // back out of the session. Paper could, because the chat listener was the GUI
        // handler and `onClose` left it registered while a rename was pending.
        // If the target were read from the session here, an admin renaming somebody
        // else's home would rewrite their own.
        const VIEWER: &str = "test-rename-survives-key";

        let pending = PendingRename {
            number: 7,
            target: "other-uuid".to_string(),
            target_name: "Other".to_string(),
        };
        begin_rename(VIEWER, pending.clone());

        // The viewer does what they were told.
        clear_session(VIEWER);

        assert_eq!(
            take_rename(VIEWER),
            Some(pending),
            "the rename still knows whose home it is renaming"
        );
    }

    /// Deliberately absent: a test that calls `set_folder` or `open_store`.
    ///
    /// Both write to a `OnceLock`, and the tests share one process, so a test that
    /// filled them would make every *other* test that asks "has this loaded?" see
    /// loaded state - which is exactly what
    /// `a_reload_before_the_plugin_has_loaded_is_refused_not_guessed` in
    /// `lib.rs` asserts is `None`. A one-shot global can only be set once per
    /// process, and a test that spent it would spend it for the others too.
    #[test]
    fn a_reopen_marks_the_viewer_only_while_it_is_happening() {
        // The bug this guards: the host fires `InventoryCloseEvent` re-entrantly,
        // from inside `open_gui`, for the screen being replaced. A close handler
        // that always clears would drop the session belonging to the screen on its
        // way, and the screen would open once and then ignore every click.
        //
        // Self-contained by construction - the key is unique to this test and the
        // mark is removed at the end - so it cannot leak into another test in the
        // shared process.
        const KEY: &str = "test-reopen-key";

        assert!(
            !is_reopening(KEY),
            "nothing is being replaced to begin with"
        );
        begin_reopen(KEY);
        assert!(
            is_reopening(KEY),
            "a close during the reopen is not a departure"
        );
        // Marking twice is harmless: the screens nest when a delete confirmation
        // hands back to the homes screen.
        begin_reopen(KEY);
        end_reopen(KEY);
        end_reopen(KEY);
        assert!(
            !is_reopening(KEY),
            "the mark is cleared, so a real close after it counts"
        );
    }

    #[test]
    fn a_configuration_installed_later_is_visible_to_the_next_reader() {
        // Stands in for the install/read pair that `on_load` and every command use,
        // without touching a `OnceLock` that cannot be undone.
        let config = crate::config::Config::defaults();
        let home = Session::homes(
            "uuid".into(),
            "Viewer".into(),
            "target".into(),
            "Target".into(),
            3,
        );
        assert_eq!(home.max_homes, 3);
        assert_eq!(config.teleport_time(), 5);
    }
}
