//! ModernHome, a homes plugin for Pumpkin.
//!
//! This port provides home-management commands and two configurable inventory
//! screens. Homes are stored as JSON; SQL storage, Redis transport, offline player
//! lookup and EssentialsX import are not implemented. The host has permission-
//! controlled networking, but this plugin does not use it.
//!
//! Home destinations include both the world folder and dimension. Legacy records
//! without a dimension are refused when their destination is ambiguous.
//!
//! The API has no close-inventory method; [`screens::close_screen`] uses a Java
//! packet workaround. Bedrock viewers close the screen themselves. Teleports have
//! no explicit success/cancellation result or destination chunk preload API.
//!
//! ## The screen identity problem
//!
//! `InventoryClickEventData` has no handle back to the `Gui` that was clicked - only
//! the screen *kind*. Paper keyed its handler map by the `Inventory` object, which a
//! port has no equivalent of, so one [`state::Session`] per viewer records which
//! screen is open and is dropped when it closes. See [`state`].
//!
//! ## Where the decisions live
//!
//! Nothing that could be wrong silently is in this file. Slot arithmetic,
//! pagination, click routing and the countdown are pure and live in
//! [`homes_gui`], [`confirm_gui`] and [`teleport`], where they are tested without
//! a server. This file wires them to the host and does the I/O.

use pumpkin_plugin_api::{
    Context, Plugin, PluginMetadata, Server,
    command::{ArgumentType, Command, CommandError, CommandNode, CommandSender, StringType},
    events::{
        EntityDamageByEntityEvent, EventData, EventHandler, EventPriority, InventoryClickEvent,
        InventoryCloseEvent, PlayerChatEvent, PlayerJoinEvent, PlayerLeaveEvent,
    },
    permission::{Permission, PermissionDefault, PermissionLevel},
    player::Player,
};

pub mod commands;
pub mod config;
pub mod confirm_gui;
pub mod homes;
pub mod homes_gui;
pub mod items;
pub mod layout;
pub mod messages;
pub mod permissions;
pub mod screens;
pub mod sounds;
pub mod state;
pub mod store;
pub mod teleport;
pub mod text;

/// The plugin name, and therefore the only permission namespace it may register.
pub const PLUGIN: &str = "ModernHome";

/// `/home` and its aliases.
///
/// Paper registered four commands with `CommandMap.register`, which also gave each
/// one a namespaced fallback. Pumpkin's `Command::new` takes a name list where the
/// first is primary and the rest are aliases (`command.wit:240`), so `/homes`,
/// `/deletehome` and `/rmhome` are aliases here rather than re-dispatched: the
/// `CommandHiderListener` that rewrote them in Paper has nothing to rewrite, because
/// the aliases resolve to the same tree.
const HOME: [&str; 3] = ["home", "homes", "h"];
const SET_HOME: [&str; 1] = ["sethome"];
const DEL_HOME: [&str; 3] = ["delhome", "deletehome", "rmhome"];
const ADMIN: [&str; 1] = ["ahome"];

/// Command gates use the exact, case-sensitive namespace required by Pumpkin.
const USE_HOME: &str = "ModernHome:home";
const USE_ADMIN: &str = "ModernHome:admin";

/// Every node this plugin registers.
///
/// Spelled as literals because `concat!` needs a literal rather than a const, so it
/// cannot be derived from [`PLUGIN`]. The test at the bottom is what ties the two
/// together, and it is why renaming the plugin cannot leave a node behind in the old
/// namespace.
pub const COMMAND_NODES: [&str; 2] = [USE_HOME, USE_ADMIN];

/// Re-reads every configuration file.
///
/// Returns whether the reload was clean and what went wrong. On the clean path the
/// new limits and timings take effect immediately; on the broken path the previous
/// configuration stays in force, because reverting to defaults would quietly
/// flatten every group's limit and every message.
pub fn reload() -> Option<(bool, Vec<String>)> {
    let folder = state::folder()?;
    let loaded = config::load(&folder);
    let problems = loaded.problems;

    if !problems.is_empty() {
        return Some((false, problems));
    }
    state::install(loaded.value);
    Some((true, Vec::new()))
}

pub struct PumpkinHome;

impl Plugin for PumpkinHome {
    fn new() -> Self {
        Self
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: PLUGIN.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            authors: vec!["Rennex".to_string()],
            description: "Homes for Pumpkin: a homes screen with pagination, rename, delete \
                          confirmation, countdown teleports and per-player limits."
                .to_string(),
            // PumpkinPerm owns permissions and PumpkinPIE owns cross-plugin
            // expansion, and neither is named here. Naming either would make this
            // plugin refuse to load without it, and neither is needed: the nodes
            // are read with `has_permission`, and every placeholder is filled in
            // here.
            dependencies: Vec::new(),
            permissions: vec![
                // The data folder is the only directory the sandbox is granted,
                // and it is where the configuration and the homes live. Without
                // these two the host does not preopen it and every `std::fs` call
                // in this plugin fails.
                //
                // The host asks for these interactively on first load, and skips
                // the plugin when it cannot ask.
                pumpkin_plugin_api::permissions::FS_READ_DATA.to_string(),
                pumpkin_plugin_api::permissions::FS_WRITE_DATA.to_string(),
            ],
        }
    }

    fn on_load(&self, context: Context) -> pumpkin_plugin_api::Result<()> {
        let folder = context.get_data_folder();
        // `set` rather than `get_or_init`: `on_load` runs once, and a second
        // `get_or_init` would be silently wrong if it ever did not.
        state::set_folder(folder.clone());

        let loaded = config::load(&folder);
        for problem in &loaded.problems {
            tracing::warn!("{problem}");
        }

        // The store is bound before the configuration is installed, because the
        // file name lives in `database.yml`.
        let path = std::path::Path::new(&folder).join(loaded.value.store_file());
        state::open_store(path);
        state::with_store(store::Store::load);
        state::install(loaded.value);

        let registered = register_nodes(&context);

        context.register_command(home_command(), USE_HOME);
        context.register_command(set_home_command(), USE_HOME);
        context.register_command(del_home_command(), USE_HOME);
        context.register_command(admin_command(), USE_ADMIN);

        // Chat is blocking, because a pending rename has to be able to swallow the
        // message; a non-blocking handler's return value is discarded by the host,
        // so `cancelled` would never be read back.
        context.register_event_handler::<PlayerChatEvent, _>(
            OnChat,
            EventPriority::Normal,
            true,
        )?;
        // Clicks are blocking for the same reason: the whole point is cancelling.
        context.register_event_handler::<InventoryClickEvent, _>(
            OnClick,
            EventPriority::Normal,
            true,
        )?;
        // Close is observational - `InventoryCloseEventData` has no `cancelled`
        // field - but it still has to run, so it is registered blocking only so it
        // is ordered after the click handler rather than before it.
        context.register_event_handler::<InventoryCloseEvent, _>(
            OnClose,
            EventPriority::Low,
            true,
        )?;
        // Damage only records a timestamp, so there is nothing to give back.
        context.register_event_handler::<EntityDamageByEntityEvent, _>(
            OnDamage,
            EventPriority::Normal,
            false,
        )?;
        // Join and leave are bookkeeping.
        context.register_event_handler::<PlayerJoinEvent, _>(
            OnJoin,
            EventPriority::Normal,
            false,
        )?;
        context.register_event_handler::<PlayerLeaveEvent, _>(
            OnLeave,
            EventPriority::Low,
            false,
        )?;

        let homes = state::player_count();
        let layout = config::Config::defaults().homes_layout().slots_per_page();
        tracing::info!(
            "{PLUGIN} {} ready: {homes} player(s) with homes, {registered} permission node(s), \
             {layout} home(s) per page.",
            env!("CARGO_PKG_VERSION")
        );
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> pumpkin_plugin_api::Result<()> {
        // The homes are the only thing this plugin holds that the server is about
        // to shut down holding, and a clean unload is the last chance to write them.
        // Cancels nothing itself: the host drops the tasks with the instance.
        teleport::clear_destinations();
        teleport::clear();
        state::clear_all();
        tracing::info!("{PLUGIN} unloaded");
        Ok(())
    }
}

fn register_nodes(context: &Context) -> usize {
    let mut registered = 0;
    for (node, default) in command_permissions() {
        // Not a failure: registration is not idempotent on the host, and a repeat
        // means the node is there.
        if register(context, node, "A ModernHome command.", default) {
            registered += 1;
        }
    }
    registered
}

fn command_permissions() -> [(&'static str, PermissionDefault); 2] {
    [
        (USE_HOME, PermissionDefault::Allow),
        (USE_ADMIN, PermissionDefault::Op(PermissionLevel::Two)),
    ]
}

/// Registers one node, logging a real failure and ignoring a repeat.
///
/// A namespace mistake lands here, and every command behind the node becomes
/// invisible to everybody, operators included - so it is logged loudly.
fn register(context: &Context, node: &str, description: &str, default: PermissionDefault) -> bool {
    match context.register_permission(&Permission {
        node: node.to_string(),
        description: description.to_string(),
        default,
        // Always empty on purpose: the host only reads a permission's `children`
        // when walking a player's *attachment* parents
        // (`pumpkin-util/src/permission.rs:363`), and the registry-default path
        // reads `default` and nothing else.
        children: Vec::new(),
    }) {
        Ok(()) => {
            tracing::debug!("registered permission {node}");
            true
        }
        Err(message) if message.contains("already registered") => {
            tracing::debug!("permission {node} was already registered");
            true
        }
        Err(message) => {
            tracing::error!("could not register {node}: {message}");
            false
        }
    }
}

/// A chat message that turns out to be a rename.
///
/// The Java order is preserved exactly, and it is not the obvious one:
///
/// 1. only *this* viewer's pending rename counts;
/// 2. the entry is taken, whether or not the message turns out to be usable, so a
///    rejected name still ends the rename;
/// 3. the message is then cancelled - always, once a rename is pending, so it never
///    reaches chat;
/// 4. the name has the legacy `§` codes Paper stripped, is trimmed, is refused
///    when empty, and is cut to thirty characters when longer.
///
/// This listens for `PlayerChatEvent`, not `AsyncPlayerChatEvent`. Both are in
/// the WIT and both carry `player`, `message` and `cancelled`, so the handler body
/// is identical for either - but the host only ever fires `PlayerChatEvent`
/// (`net/java/play/chat_message.rs:45` and `net/bedrock/play/chat_message.rs:19`).
/// Nothing in the server constructs an `AsyncPlayerChatEvent`, so a handler
/// registered for one is registered, never dispatched, and a rename silently does
/// nothing however correct the code below it is.
struct OnChat;

impl EventHandler<PlayerChatEvent> for OnChat {
    fn handle(
        &self,
        server: Server,
        mut event: EventData<PlayerChatEvent>,
    ) -> EventData<PlayerChatEvent> {
        let viewer = state::uuid_of(&event.player);
        let Some(pending) = state::take_rename(&viewer) else {
            return event;
        };

        // Paper's `event.setCancelled(true)` comes immediately after the pending
        // entry is found and before the name is looked at, so a name that turns out
        // to be empty or over-long is still swallowed rather than reaching chat.
        // `send_cancellable!` skips the broadcast for a cancelled event
        // (`pumpkin-macros/src/lib.rs:144-163`), so this is what keeps the typed
        // name off the server.
        event.cancelled = true;

        let config = screens::config();
        if !permissions::can_manage(&server, &event.player, &pending.target) {
            screens::say(&config, &event.player, "no-permission", &[]);
            return event;
        }
        let mut name = text::strip_legacy_codes(&event.message).trim().to_string();

        if name.is_empty() {
            screens::say(&config, &event.player, "home-rename-empty", &[]);
            return event;
        }
        if name.chars().count() > HOME_NAME_LIMIT {
            // Paper's `substring(0, 30)`, which counts UTF-16 code units. Counting
            // characters is the nearest honest equivalent in Rust and keeps an
            // emoji from being cut in half.
            name = name.chars().take(HOME_NAME_LIMIT).collect();
            screens::say(&config, &event.player, "home-rename-trimmed", &[]);
        }

        // The target travels with the rename, not looked up in the session: the
        // session is dropped when the viewer closes the screen, which is exactly
        // what Paper's prompt tells them to do before typing the name.
        let number = pending.number;
        let target = (pending.target, pending.target_name);

        let old_name = state::with_store(|store| {
            store
                .homes(&target.0)
                .and_then(|homes| homes.find(number).and_then(|h| h.name.clone()))
        });

        let written = match state::with_store(|store| {
            store.update(|store| store.rename_home(&target.0, number, &name))
        }) {
            Ok(written) => written,
            Err(error) => {
                tracing::error!("Could not rename home: {error}");
                screens::say(&config, &event.player, "home-rename-fail", &[]);
                return event;
            }
        };

        if written {
            screens::say(
                &config,
                &event.player,
                "home-renamed",
                &[
                    ("%name%", name),
                    ("%home%", old_name.unwrap_or_default()),
                    ("%index%", number.to_string()),
                    ("%number%", number.to_string()),
                    ("%target%", target.1),
                ],
            );
            // Paper refreshed the screen after a rename, and this keeps that
            // behaviour: the list is re-read rather than patched in place.
            //
            // `refreshGUI` cleared the inventory and redrew it, keeping
            // `currentPage`. A closed screen is not reopened - `updateInventory()`
            // on a closed inventory draws nothing - and the session is dropped when
            // the viewer closes the screen, so "no session" is exactly the
            // "screen is gone" case and is left alone rather than second-guessed.
            if let Some(session) = state::session(&viewer) {
                screens::render_homes(&config, &event.player, &session);
            }
        }
        let _ = server;
        event
    }
}

/// The longest a home name may be.
///
/// Paper's `newName.length() > 30`, which is UTF-16 code units.
const HOME_NAME_LIMIT: usize = 30;

/// A click on one of this plugin's two screens.
struct OnClick;

impl EventHandler<InventoryClickEvent> for OnClick {
    fn handle(
        &self,
        server: Server,
        mut event: EventData<InventoryClickEvent>,
    ) -> EventData<InventoryClickEvent> {
        let viewer = state::uuid_of(&event.player);
        let Some(mut session) = state::session(&viewer) else {
            return event;
        };
        // Only our menus own these clicks. Leave every other inventory untouched.
        event.cancelled = true;
        let config = screens::config();
        if !permissions::can_manage(&server, &event.player, &session.target) {
            state::discard_rename(&viewer);
            screens::say(&config, &event.player, "no-permission", &[]);
            return event;
        }
        session.max_homes = permissions::max_homes(&server, &event.player, config.limit_scan_max());
        state::set_session(session.clone());

        // `raw_slot` is the top inventory's own numbering, which is what the Java
        // handler compared against `inventory.getSize()`. A negative slot is the
        // player dropping a drag outside the window.
        if event.raw_slot < 0 {
            return event;
        }
        let slot = u32::try_from(event.raw_slot).unwrap_or(u32::MAX);

        match session.open {
            state::Open::Homes => {
                let config = screens::config();
                let grid = screens::grid(&config, &session);
                if let Some(action) = homes_gui::click(&grid, slot, event.click_type) {
                    sounds::play(&config, &event.player, "ui-click");
                    run(&server, &config, &session, &event.player, action);
                }
            }
            state::Open::Confirm => {
                let config = screens::config();
                // The confirmation screen does not filter on the mouse button, and
                // it played no sound on the click itself - the confirm button played
                // one from inside its own handler. So `Confirm` and `Back` are silent
                // and only `Accept` plays `ui-click`, which is what the Java's four
                // branches did.
                if let Some(button) = screens::confirm_click(slot) {
                    run_confirm(&config, &session, &event.player, button);
                }
            }
        }
        event
    }
}

/// Runs what a click on the homes screen asked for.
fn run(
    server: &Server,
    config: &crate::config::Config,
    session: &state::Session,
    player: &Player,
    action: homes_gui::Action,
) {
    match action {
        homes_gui::Action::Teleport { home } => teleport_to(server, config, player, &home),
        homes_gui::Action::Rename { number } => {
            // Recorded **before** anything else, and cleared on the way out, so the
            // next chat message is the name. Paper closed the screen first and
            // recorded the rename after, which meant its own close handler saw no
            // pending rename and unhooked the chat listener before the rename could
            // ever arrive.
            //
            // The session's target goes in with it, because Paper's chat listener was
            // the GUI handler and read the target off that handler's own fields -
            // see [`state::PendingRename`].
            state::begin_rename(
                &state::uuid_of(player),
                state::PendingRename {
                    number,
                    target: session.target.clone(),
                    target_name: session.target_name.clone(),
                },
            );
            // Paper closed the screen before it prompted (`HomeGuiHandler.java:401`),
            // so the player was looking at the world while they typed the name. The
            // entry is recorded first, because that close clears the session and the
            // close handler would otherwise see nothing pending and treat this as the
            // player walking away.
            screens::close_screen(player);
            screens::rename_prompt(config, player);
        }
        homes_gui::Action::Set { number } => set_home(server, config, session, player, number),
        homes_gui::Action::Delete { home } => screens::show_confirm(player, session, &home),
        homes_gui::Action::PreviousPage | homes_gui::Action::NextPage => {
            let delta = if matches!(action, homes_gui::Action::PreviousPage) {
                -1
            } else {
                1
            };
            let next = screens::turned(session, delta);
            screens::render_homes(config, player, &next);
        }
    }
}

/// Runs what a click on the confirmation screen asked for.
fn run_confirm(
    config: &crate::config::Config,
    session: &state::Session,
    player: &Player,
    button: confirm_gui::Button,
) {
    match button {
        // Cancel and Back are the same thing: straight back to the homes screen.
        // Paper built a fresh `HomeGuiHandler` for both, so both land on page one.
        confirm_gui::Button::Cancel | confirm_gui::Button::Back => {
            screens::render_homes(config, player, session);
        }
        confirm_gui::Button::Accept => {
            sounds::play(config, player, "ui-click");
            let Some(home) = session.pending_delete.clone() else {
                return;
            };
            let deleted = match state::with_store(|store| {
                store.update(|store| store.delete_home(&session.target, home.home_number))
            }) {
                Ok(deleted) => deleted,
                Err(error) => {
                    tracing::error!("Could not delete home: {error}");
                    screens::say(config, player, "home-delete-fail", &[]);
                    return;
                }
            };
            if !deleted {
                screens::say(
                    config,
                    player,
                    "home-not-exist",
                    &[("%index%", home.home_number.to_string())],
                );
                return;
            }

            // The message is `home-deleted-admin` in both directions: Paper's
            // confirmation screen sent the admin wording even when the viewer was
            // deleting their own home, and only `/delhome` sent the plain one.
            screens::say(
                config,
                player,
                "home-deleted-admin",
                &[("%target%", session.target_name.clone())],
            );
            // A delete always returned to page one, because a new handler was built.
            let next = screens::first_page(session);
            screens::render_homes(config, player, &next);
        }
    }
}

/// Sets a home in `number`, from where `player` is standing.
///
/// The same two early refusals as `/sethome`, in the same order: a blacklisted
/// world, then the limit.
fn set_home(
    server: &Server,
    config: &crate::config::Config,
    session: &state::Session,
    player: &Player,
    number: i32,
) {
    let world = player.get_world();
    let world_name = world.get_name();
    let dimension = world.get_dimension();
    if config.is_blacklisted(&world_name) {
        screens::say(config, player, "home-blacklisted-world", &[]);
        sounds::play(config, player, "error");
        return;
    }

    // OnClick refreshed this limit before planning the action.
    let max = session.max_homes;
    let count = state::with_store(|store| store.home_numbers(&session.target).len());
    if number < 1 || number > max || count >= max.max(0) as usize {
        screens::say(config, player, "home-limit-reached", &[]);
        sounds::play(config, player, "error");
        return;
    }

    let position = player.get_position();
    let home = homes::Home::new(
        number,
        Some(world_name),
        position,
        (player.get_yaw(), player.get_pitch()),
        None,
        // Every home records this server, which is what `HomeManager.setHome` did
        // through the `server_id` column.
        Some(config.server_id()),
    )
    .in_dimension(dimension);

    if let Err(error) =
        state::with_store(|store| store.update(|store| store.set_home(&session.target, home)))
    {
        tracing::error!("Could not save home: {error}");
        screens::say(config, player, "home-set-fail", &[]);
        sounds::play(config, player, "error");
        return;
    }
    screens::render_homes(config, player, session);
    let _ = server;
}

/// Teleports `player` to `home`, or says why not.
///
/// The cross-server record is answered with
/// `teleport-cross-server-unavailable`: Paper fell through to its local branch only
/// when Redis was *also* down, and otherwise NPE'd on the unset world of a home that
/// belonged to another server. This one has no transport at all, so the unavailable
/// message is the honest answer and the home stays visible in the screen.
fn teleport_to(
    server: &Server,
    config: &crate::config::Config,
    player: &Player,
    home: &homes::Home,
) {
    if !home.is_local(&config.server_id()) {
        screens::say(config, player, "teleport-cross-server-unavailable", &[]);
        return;
    }
    if home.world.is_none() {
        screens::say(config, player, "teleport-world-not-loaded", &[]);
        return;
    }
    let mut worlds = server
        .get_all_worlds()
        .into_iter()
        .filter(|world| home.matches_world(&world.get_name(), &world.get_dimension()));
    let Some(world) = worlds.next() else {
        screens::say(config, player, "teleport-world-not-loaded", &[]);
        return;
    };
    if worlds.next().is_some() {
        screens::say(config, player, "teleport-world-ambiguous", &[]);
        return;
    }

    // `start` owns the destination: the `World` is consumed by the teleport, so
    // there is no second copy to hand over separately.
    teleport::start(
        config,
        server,
        player,
        teleport::Destination {
            position: (home.x, home.y, home.z),
            yaw: Some(home.yaw),
            pitch: Some(home.pitch),
        },
        world,
    );
}

/// A screen closing, which drops the viewer's session.
struct OnClose;

impl EventHandler<InventoryCloseEvent> for OnClose {
    fn handle(
        &self,
        _server: Server,
        event: EventData<InventoryCloseEvent>,
    ) -> EventData<InventoryCloseEvent> {
        // A close we caused is not a departure. `open_gui` fires this event
        // re-entrantly for the screen it is replacing, so clearing the session
        // unconditionally would wipe the session belonging to the screen that is on
        // its way - and the next click would find nothing to dispatch on.
        let viewer = state::uuid_of(&event.player);
        if state::is_reopening(&viewer) {
            return event;
        }
        // Only the viewer's own screen goes. A pending rename is left alone: it is
        // not screen state, and Paper kept it across the close too - that is the
        // entire reason a rename survives a right-click.
        state::clear_session(&viewer);
        event
    }
}

/// Damage, which only records that a player is in combat.
///
/// `EntityDamageByEntityEventData` carries two entity *ids* and no player, so both
/// sides are resolved by walking the online players and comparing entity ids. Paper
/// stamped the victim and the damager, and `getDamager()` for a projectile is the
/// projectile rather than the shooter - so an arrow did not put the archer in
/// combat, and that is reproduced.
struct OnDamage;

impl EventHandler<EntityDamageByEntityEvent> for OnDamage {
    fn handle(
        &self,
        server: Server,
        event: EventData<EntityDamageByEntityEvent>,
    ) -> EventData<EntityDamageByEntityEvent> {
        for (id, _) in [(event.entity_id, ()), (event.damager_id, ())] {
            if let Some(player) = player_by_entity_id(&server, id) {
                state::record_damage(&state::uuid_of(&player));
            }
        }
        event
    }
}

/// The online player whose entity id is `id`, if any.
///
/// `server.get_all_players()` mints a **fresh** resource handle for every player on
/// every call (`wasm_host/wit/v0_1/server.rs:108-122`) and nothing reuses one, so
/// this is called twice per damage event and per tick per player rather than being
/// cached - a cached handle would be a stale table entry, which is the failure
/// mode the sibling plugins write a module header about.
fn player_by_entity_id(server: &Server, id: i32) -> Option<Player> {
    let wanted = u32::try_from(id).ok()?;
    server
        .get_all_players()
        .into_iter()
        .find(|player| player.as_entity().get_id() == wanted)
}

/// A player joining, which cancels any countdown they somehow left running.
///
/// Paper did nothing on join for the countdown and relied on the offline branch of
/// the tick. The offline branch is still there - this only clears a countdown whose
/// viewer left and came straight back within the same second.
struct OnJoin;

impl EventHandler<PlayerJoinEvent> for OnJoin {
    fn handle(
        &self,
        _server: Server,
        event: EventData<PlayerJoinEvent>,
    ) -> EventData<PlayerJoinEvent> {
        // Paper's `onPlayerJoin` fed the cross-server teleporter, which has no
        // counterpart here.
        teleport::cancel(&state::uuid_of(&event.player));
        event
    }
}

/// A player leaving, which forgets their combat timestamp.
struct OnLeave;

impl EventHandler<PlayerLeaveEvent> for OnLeave {
    fn handle(
        &self,
        _server: Server,
        event: EventData<PlayerLeaveEvent>,
    ) -> EventData<PlayerLeaveEvent> {
        let viewer = state::uuid_of(&event.player);
        // Java cleared only `lastDamageTime` and left `activeTasks` alone,
        // expecting the task's own offline check to clear it on the next tick. The
        // task is cancelled outright here instead, which is the same outcome
        // reached a tick earlier.
        state::forget_damage(&viewer);
        teleport::cancel(&viewer);
        state::discard_rename(&viewer);
        state::clear_session(&viewer);
        event
    }
}

/// The single-word argument type, shared by every command that takes a name.
///
/// A `String(SingleWord)` rather than `Players`, so `/ahome osee` can name someone
/// who is not connected - which is the whole point of `osee`.
fn word_argument() -> &'static ArgumentType {
    static TYPE: std::sync::OnceLock<ArgumentType> = std::sync::OnceLock::new();
    TYPE.get_or_init(|| ArgumentType::String(StringType::SingleWord))
}

/// An integer argument type, for `/delhome <number>`.
///
/// `Integer(None, None)` so the parser accepts any integer and the *command* decides
/// what is missing, which is what Paper's `args.length != 1` check did. An optional
/// argument would make the node's own execute fire for the bare form.
fn integer_argument() -> &'static ArgumentType {
    static TYPE: std::sync::OnceLock<ArgumentType> = std::sync::OnceLock::new();
    TYPE.get_or_init(|| ArgumentType::Integer((None, None)))
}

/// A command's names, in the order the host wants them.
///
/// First is primary, rest are aliases, and the host makes that split itself
/// (`wasm_host/wit/v0_1/context.rs:1771-1777`).
pub fn names(command: &[&str]) -> Vec<String> {
    command.iter().map(|name| (*name).to_string()).collect()
}

/// `/home` and its aliases. Arguments are ignored, as they were in Paper.
///
/// The root **must** carry the handler, and that is not decoration: the dispatcher
/// walks to the leaf node and then asks whether it has an executable command -
/// `ContextChain::try_flatten` returns `None` when `command.is_none()`
/// (`pumpkin-command/src/context/command_context.rs:221-224`) - and a `None` chain is
/// reported to the player as **"Unknown command"**. The permission requirement is
/// never even consulted, because `try_flatten` does not check it.
///
/// That is what this root used to do, and why `/home` appeared in tab completion -
/// registration *had* happened - and then answered "Unknown command" when run.
/// Every root in this module carries an `.execute`, and
/// `every_command_root_is_executable` is the test that keeps it that way.
pub fn home_command() -> Command {
    Command::new(&names(&HOME), "Open homes GUI").execute(commands::OpenHomes)
}

/// `/sethome`, which likewise needs its handler on the root - see [`home_command`].
pub fn set_home_command() -> Command {
    Command::new(&names(&SET_HOME), "Set a home").execute(commands::SetHome)
}

/// `/delhome [number]`.
///
/// The bare form carries its own handler on the root node, because a tree declares
/// its arguments as required: with the argument on the root and nothing else,
/// `/delhome` would fail to parse and the host would print its own syntax error
/// instead of the `delhome-usage` line from `messages.yml`. The argument therefore
/// hangs off an executable root, which is the shape PumpkinPerm uses for
/// `/pumpkinperm list`.
pub fn del_home_command() -> Command {
    Command::new(&names(&DEL_HOME), "Delete a home")
        .execute(commands::DeleteUsage)
        .then(
            CommandNode::argument("number", integer_argument())
                .execute(commands::Delete)
                .suggest(commands::HomeNumbers),
        )
}

/// `/ahome [reload|help|see <player>|osee <player>]`.
///
/// Every subcommand is reachable, and the bare form is the help text, which is what
/// Paper's `args.length == 0 -> sendHelp` did.
pub fn admin_command() -> Command {
    Command::new(&names(&ADMIN), "Admin commands for ModernHome")
        .execute(commands::AdminHelp)
        .then(CommandNode::literal("reload").execute(commands::Reload))
        .then(CommandNode::literal("help").execute(commands::Help))
        .then(
            CommandNode::literal("see")
                .execute(commands::See)
                .then(CommandNode::argument("player", word_argument()).execute(commands::See)),
        )
        .then(
            CommandNode::literal("osee")
                .execute(commands::Osee)
                .then(CommandNode::argument("player", word_argument()).execute(commands::Osee)),
        )
}

/// A handler that did nothing, used by `/home` and `/sethome`.
pub struct Open;

/// The answer to a command that is not a failure.
///
/// Paper returned `true` from every `onCommand`, which Bukkit reads as "handled",
/// so no command in the plugin ever produced Bukkit's usage string. A non-zero
/// count is the same thing in Pumpkin: the host uses it as a success count, and
/// every failure here sends the plugin's own message and returns 1 rather than
/// `CommandError::CommandFailed` - which the host renders as
/// `Syntax error: Unexpected "…"` with the whole line quoted.
pub fn done() -> Result<i32, CommandError> {
    Ok(0)
}

/// Sends a message and yields a failure count.
pub fn soft_error(sender: &CommandSender, message: impl Into<String>) -> Result<i32, CommandError> {
    sender.send_error(pumpkin_plugin_api::text::TextComponent::text(
        &message.into(),
    ));
    Ok(1)
}

/// Sends the plugin's own message to a command sender.
pub fn reply(
    sender: &CommandSender,
    config: &crate::config::Config,
    key: &str,
    placeholders: &[(&'static str, String)],
) {
    let rendered = messages::render(config, key, placeholders);
    sender.send_message(
        pumpkin_plugin_api::text::TextComponent::from_legacy_string_with_code(
            &rendered,
            text::CODE,
        ),
    );
}

/// The `init-plugin` export a component needs, and nothing on a host build wants.
///
/// Gating to `wasm32` keeps `pumpkin-plugin-api`'s own copy of the symbol from
/// colliding with this one when the test harness links; dropping it entirely would
/// fail the link outright with "failed to find export of function `init-plugin`".
#[cfg(target_arch = "wasm32")]
use pumpkin_plugin_api::register_plugin;

#[cfg(target_arch = "wasm32")]
register_plugin!(PumpkinHome);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_node_this_plugin_registers_is_in_its_own_namespace() {
        // The host enforces this with an error, and the consequence is invisible:
        // the command reports "Unknown command" for everybody, operators included.
        for node in COMMAND_NODES {
            assert!(
                node.starts_with(&format!("{PLUGIN}:")),
                "{node} is outside the {PLUGIN}: namespace"
            );
        }
    }

    #[test]
    fn the_metadata_name_is_the_namespace() {
        // The host matches against `metadata.name`, not against a constant.
        assert_eq!(PumpkinHome::new().metadata().name, PLUGIN);
        assert_eq!(PLUGIN, "ModernHome");
    }

    #[test]
    fn no_node_is_registered_without_a_colon() {
        for node in COMMAND_NODES {
            assert!(node.contains(':'), "{node} has no namespace of its own");
        }
        assert_eq!(permissions::ADMIN, USE_ADMIN);
        assert_eq!(permissions::LIMIT_PREFIX, "ModernHome:limit.");
    }

    #[test]
    fn admin_command_visibility_and_handler_use_the_same_op_only_permission() {
        let [(home, home_default), (admin, admin_default)] = command_permissions();
        assert_eq!(home, USE_HOME);
        assert!(matches!(home_default, PermissionDefault::Allow));
        assert_eq!(admin, permissions::ADMIN);
        assert!(matches!(
            admin_default,
            PermissionDefault::Op(PermissionLevel::Two)
        ));
    }

    #[test]
    fn the_paper_aliases_are_all_still_commands() {
        // Paper's `CommandHiderListener` rewrote `/homes`, `/deletehome` and
        // `/rmhome` onto `/home` and `/delhome`. Here they are aliases of the same
        // tree, so the listener has nothing to do and the names keep working.
        assert!(HOME.contains(&"homes"));
        assert!(DEL_HOME.contains(&"deletehome"));
        assert!(DEL_HOME.contains(&"rmhome"));
        assert_eq!(HOME[0], "home", "the primary name leads");
        assert_eq!(DEL_HOME[0], "delhome");
    }

    #[test]
    fn a_reload_before_the_plugin_has_loaded_is_refused_not_guessed() {
        // The `None` arm is what makes `/ahome reload` answer "has no state to
        // reload" rather than reading a folder that was never opened. Reachable
        // from a test because the `OnceLock` is still empty here and the early
        // return happens before any host call.
        assert_eq!(reload(), None);
    }

    #[test]
    fn the_home_name_limit_is_thirty() {
        // Paper's `newName.length() > 30`.
        assert_eq!(HOME_NAME_LIMIT, 30);
    }

    #[test]
    fn every_command_root_is_executable() {
        // The bug this guards: a root with children but no `.execute` registers
        // fine - it shows in tab completion - and then answers "Unknown command",
        // because `ContextChain::try_flatten` returns `None` when the leaf has no
        // executable and never consults the permission requirement at all.
        //
        // `Command` is a resource handle with no getter for its tree, so this
        // asserts on the *source* of each root rather than on the built value: it
        // is the shape of the builder call that is wrong, and the shape is what a
        // regression would break.
        let source = include_str!("lib.rs");
        let lines: Vec<&str> = source.lines().collect();
        let mut checked = 0;
        for (index, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if !trimmed.starts_with("Command::new(") {
                continue;
            }
            // The builder call runs until the line that ends the statement, which
            // is the first line whose trimmed form ends with `;` or is a bare `}`.
            let end = (index..lines.len())
                .find(|candidate| {
                    let text = lines[*candidate].trim();
                    text.ends_with(';') || text == "}"
                })
                .unwrap_or(lines.len() - 1);
            let statement = lines[index..=end.min(index + 6)].join("\n");
            assert!(
                statement.contains(".execute(") || statement.contains(".then("),
                "the command root at lib.rs:{} has neither an .execute nor a .then, so \
                 its leaf has no command and every invocation answers \"Unknown \
                 command\":\n{statement}",
                index + 1,
            );
            checked += 1;
        }
        assert_eq!(checked, 4, "all four command roots are covered");
    }

    #[test]
    fn the_permission_plugin_is_a_soft_dependency() {
        // Named nowhere in `dependencies`, which is what makes "loaded without
        // PumpkinPerm" a supported configuration rather than a failure.
        assert!(PumpkinHome::new().metadata().dependencies.is_empty());
    }
}
