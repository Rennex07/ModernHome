//! `/home`, `/sethome`, `/delhome` and `/ahome`.
//!
//! ## Where the permission checks live
//!
//! `context.register_command` attaches one node to the whole tree, so the coarse
//! check is a `ModernHome:` node on the root - which is `Allow`, deliberately,
//! because an unregistered node resolves to `false` for everyone at every op level
//! and the command would report "Unknown command" to everybody.
//!
//! The real gate is `home.admin`, checked inside every admin handler exactly where
//! Paper checked it. The per-node alternative,
//! `command-node.require-with-handler-id`, is a stub on this host that returns
//! `Err("require_with_handler_id not implemented")` and traps the command
//! (`wasm_host/wit/v0_1/commands/mod.rs:763`), so it is not used.
//!
//! ## Every failure is the plugin's own message
//!
//! `CommandError::CommandFailed` is how a handler would carry a sentence, and the
//! host maps every variant onto `DISPATCHER_PARSE_EXCEPTION`, which renders as
//! `Syntax error: Unexpected "<the whole message>"` with the text repeated and
//! quoted (`executor.rs:39-50`). A parser error being used to carry a sentence reads
//! as one. So every failure sends `messages.yml`'s own text and returns a non-zero
//! count, which is what Paper's `return true` meant: handled.
//!
//! ## Why the bare forms have their own handlers
//!
//! A command tree declares its arguments as *required*. `/delhome` with no argument
//! therefore never reaches a handler that declares one - the dispatcher refuses the
//! parse and the host prints its own syntax error, which is not
//! `delhome-usage`. So the root node carries the bare case as an `execute`, and the
//! argument hangs off it as a child. That is the same shape PumpkinPerm uses for
//! `/pumpkinperm list`, and it is what lets the plugin answer with the words the
//! admin put in `messages.yml`.
//!
//! ## `/delhome`'s completion is bounded, and Paper's was not
//!
//! Paper's `DelHomeCMD.tabComplete` listed `1..=getMaxHomes(player)`, and for an
//! admin - who has `Integer.MAX_VALUE` - that is two billion strings and an
//! out-of-memory. See [`crate::lib`] for the bound this uses.

use pumpkin_plugin_api::command_wit::Number;
use pumpkin_plugin_api::{
    Server,
    command::{
        Arg, CommandError, CommandSender, CommandSuggestion, CommandSuggestions, ConsumedArgs,
        SuggestionRequest,
    },
    commands::{CommandHandler, CommandSuggestionHandler},
};

use crate::{
    config::Config, done, homes::Home, permissions, reply, screens, soft_error, sounds, state,
    store::Store,
};

fn not_a_player(sender: &CommandSender) -> Result<i32, CommandError> {
    reply(sender, &screens::config(), "player-only", &[]);
    Ok(0)
}

fn is_admin(sender: &CommandSender, server: &Server) -> bool {
    if sender.has_permission(server, permissions::ADMIN) {
        return true;
    }
    match sender.as_player() {
        Some(player) => server.get_op_manager().is_op(player.get_id()),
        // A console has no uuid to ask about, and the host grants it every
        // permission anyway. Asked rather than assumed, so the answer is the
        // host's.
        None => sender.is_console(),
    }
}

fn guard(sender: &CommandSender, server: &Server) -> Result<bool, CommandError> {
    if is_admin(sender, server) {
        return Ok(true);
    }
    reply(sender, &screens::config(), "no-permission", &[]);
    Ok(false)
}

fn say(
    sender: &CommandSender,
    config: &Config,
    key: &str,
    placeholders: &[(&'static str, String)],
) {
    if config.allow_chat() {
        reply(sender, config, key, placeholders);
    }
}

pub struct OpenHomes;

impl CommandHandler for OpenHomes {
    fn handle(
        &self,
        sender: CommandSender,
        server: Server,
        _args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let Some(player) = sender.as_player() else {
            return not_a_player(&sender);
        };
        let uuid = state::uuid_of(&player);
        let name = player.get_name();
        screens::show_homes(&server, &player, uuid, name);
        done()
    }
}

pub struct SetHome;

impl CommandHandler for SetHome {
    fn handle(
        &self,
        sender: CommandSender,
        server: Server,
        _args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let Some(player) = sender.as_player() else {
            return not_a_player(&sender);
        };
        let config = screens::config();
        let viewer = state::uuid_of(&player);
        let world = player.get_world();
        let world_name = world.get_name();
        let dimension = world.get_dimension();

        if config.is_blacklisted(&world_name) {
            say(&sender, &config, "home-blacklisted-world", &[]);
            sounds::play(&config, &player, "error");
            return done();
        }

        let max = permissions::max_homes(&server, &player, config.limit_scan_max());
        let taken = state::with_store(|store: &mut Store| store.home_numbers(&viewer));

        if taken.len() >= max.max(1) as usize {
            say(&sender, &config, "home-limit-reached", &[]);
            sounds::play(&config, &player, "error");
            return done();
        }

        // The first gap, not the count. Paper walked upwards from 1 and stopped at
        // the first number not already used, so deleting home 2 and setting a new
        // one puts it back at 2 rather than appending.
        let Some(number) = next_slot(&taken, max) else {
            say(&sender, &config, "home-limit-reached", &[]);
            sounds::play(&config, &player, "error");
            return done();
        };

        let position = player.get_position();
        let rotation = (player.get_yaw(), player.get_pitch());
        let saved = state::with_store(|store| {
            store.update(|store| {
                store.set_home(
                    &viewer,
                    Home::new(
                        number,
                        Some(world_name),
                        position,
                        rotation,
                        None,
                        // Every home records this server, which is what the `server_id`
                        // column did.
                        Some(config.server_id()),
                    )
                    .in_dimension(dimension),
                )
            })
        });
        if let Err(error) = saved {
            tracing::error!("Could not save home: {error}");
            say(&sender, &config, "home-set-fail", &[]);
            sounds::play(&config, &player, "error");
            return done();
        }

        say(&sender, &config, "home-set", &[]);
        done()
    }
}

fn next_slot(taken: &[i32], limit: i32) -> Option<i32> {
    (1..=limit).find(|number| !taken.contains(number))
}

pub struct DeleteUsage;

impl CommandHandler for DeleteUsage {
    fn handle(
        &self,
        sender: CommandSender,
        _server: Server,
        _args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let Some(player) = sender.as_player() else {
            return not_a_player(&sender);
        };
        let config = screens::config();
        say(&sender, &config, "delhome-usage", &[]);
        sounds::play(&config, &player, "error");
        done()
    }
}

pub struct Delete;

impl CommandHandler for Delete {
    fn handle(
        &self,
        sender: CommandSender,
        _server: Server,
        args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let Some(player) = sender.as_player() else {
            return not_a_player(&sender);
        };
        let config = screens::config();
        let viewer = state::uuid_of(&player);

        // `Integer.parseInt` refused `abc`, `3abc` and `1.5` alike, all three with
        // `delhome-invalid-number`. An empty string is `delhome-usage` instead, but
        // this node only exists when an argument was supplied.
        let Some(number) = number_of(&args) else {
            say(&sender, &config, "delhome-invalid-number", &[]);
            sounds::play(&config, &player, "error");
            return done();
        };

        let exists = state::with_store(|store: &mut Store| {
            store.with_player(&viewer, |homes| homes.find(number).is_some())
        });
        if !exists {
            say(
                &sender,
                &config,
                "home-not-exist",
                &[("%index%", number.to_string())],
            );
            sounds::play(&config, &player, "error");
            return done();
        }

        let deleted = match state::with_store(|store| {
            store.update(|store| store.delete_home(&viewer, number))
        }) {
            Ok(deleted) => deleted,
            Err(error) => {
                tracing::error!("Could not delete home: {error}");
                say(&sender, &config, "home-delete-fail", &[]);
                sounds::play(&config, &player, "error");
                return done();
            }
        };

        say(&sender, &config, "home-deleted", &[]);
        if !deleted {
            // Only reachable if two deletes raced, which the main thread prevents.
            // Logged rather than messaged, because the player has already been told
            // the home is gone and it is.
            tracing::debug!("{viewer}'s home #{number} had already gone");
        }
        done()
    }
}

fn number_of(args: &ConsumedArgs) -> Option<i32> {
    match args.get_value("number") {
        Arg::Num(Ok(Number::Int32(value))) => Some(value),
        Arg::Num(Ok(Number::Int64(value))) => i32::try_from(value).ok(),
        // A float is not what `/delhome` takes.
        Arg::Num(_) | Arg::Simple(_) => None,
        _ => None,
    }
}

pub struct HomeNumbers;

impl CommandSuggestionHandler for HomeNumbers {
    fn suggest(
        &self,
        sender: CommandSender,
        _server: Server,
        request: SuggestionRequest,
    ) -> CommandSuggestions {
        let token_start = request.input[..request.start as usize]
            .rfind(' ')
            .map_or(request.start as usize, |index| index + 1);
        let prefix = &request.input[token_start..];

        let numbers = sender.as_player().map(|player| {
            let viewer = state::uuid_of(&player);
            state::with_store(|store: &mut Store| {
                store
                    .homes(&viewer)
                    .map(|homes| homes.numbers())
                    .unwrap_or_default()
            })
        });

        let values = numbers
            .unwrap_or_default()
            .into_iter()
            .filter(|number| number.to_string().starts_with(prefix))
            .map(|number| CommandSuggestion {
                value: number.to_string(),
                tooltip: None,
            })
            .collect();

        CommandSuggestions {
            start: token_start as u32,
            length: (request.input.len() - token_start) as u32,
            values,
        }
    }
}

pub struct AdminHelp;

impl CommandHandler for AdminHelp {
    fn handle(
        &self,
        sender: CommandSender,
        server: Server,
        _args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        if !guard(&sender, &server)? {
            return done();
        }
        help(&sender)
    }
}

pub struct Reload;

impl CommandHandler for Reload {
    fn handle(
        &self,
        sender: CommandSender,
        server: Server,
        _args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        if !guard(&sender, &server)? {
            return done();
        }
        let config = screens::config();

        match crate::reload() {
            None => {
                say(&sender, &config, "admin-plugin-not-init", &[]);
                done()
            }
            Some((true, _)) => {
                say(&sender, &config, "admin-reload", &[]);
                tracing::info!("reloaded every configuration file");
                done()
            }
            Some((false, problems)) => soft_error(
                &sender,
                format!(
                    "Reloaded with errors, so the previous configuration is still in force: {}",
                    problems.join("; ")
                ),
            ),
        }
    }
}

pub struct Help;

impl CommandHandler for Help {
    fn handle(
        &self,
        sender: CommandSender,
        server: Server,
        _args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        if !guard(&sender, &server)? {
            return done();
        }
        help(&sender)
    }
}

fn help(sender: &CommandSender) -> Result<i32, CommandError> {
    let config = screens::config();
    for key in [
        "admin-help-header",
        "admin-help-reload",
        "admin-help-help",
        "admin-help-see",
        "admin-help-osee",
    ] {
        reply(sender, &config, key, &[]);
    }
    done()
}

pub struct See;

impl CommandHandler for See {
    fn handle(
        &self,
        sender: CommandSender,
        server: Server,
        args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        if !guard(&sender, &server)? {
            return done();
        }
        let Some(player) = sender.as_player() else {
            return not_a_player(&sender);
        };
        let config = screens::config();

        let Some(target_name) = word(&args, "player") else {
            say(&sender, &config, "admin-see-usage", &[]);
            return done();
        };
        let Some(target) = server.get_player_by_name(&target_name) else {
            say(&sender, &config, "admin-player-not-online", &[]);
            return done();
        };

        screens::show_homes(&server, &player, state::uuid_of(&target), target.get_name());
        done()
    }
}

pub struct Osee;

impl CommandHandler for Osee {
    fn handle(
        &self,
        sender: CommandSender,
        server: Server,
        args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        if !guard(&sender, &server)? {
            return done();
        }
        let Some(player) = sender.as_player() else {
            return not_a_player(&sender);
        };
        let config = screens::config();

        let Some(target_name) = word(&args, "player") else {
            say(&sender, &config, "admin-osee-usage", &[]);
            return done();
        };
        let Some(target) = server.get_player_by_name(&target_name) else {
            say(&sender, &config, "admin-player-not-found", &[]);
            return done();
        };

        screens::show_homes(&server, &player, state::uuid_of(&target), target.get_name());
        done()
    }
}

fn word(args: &ConsumedArgs, name: &str) -> Option<String> {
    match args.get_value(name) {
        Arg::Simple(value) if !value.is_empty() => Some(value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_free_slot_is_the_first_gap() {
        // The rule `/sethome` uses, and the one a player notices after deleting
        // home 2: the next `/sethome` goes back to 2 rather than appending.
        assert_eq!(next_slot(&[], 5), Some(1));
        assert_eq!(next_slot(&[1], 5), Some(2));
        assert_eq!(next_slot(&[1, 2, 4], 5), Some(3));
        assert_eq!(next_slot(&[1, 2, 3], 3), None, "full up to the limit");
        assert_eq!(next_slot(&[1], 1), None);
    }

    #[test]
    fn a_deleted_slot_is_reused_rather_than_appended() {
        let mut taken = vec![1, 2, 3, 4, 5];
        taken.retain(|number| *number != 2);
        assert_eq!(next_slot(&taken, 5), Some(2));
    }

    #[test]
    fn the_slot_search_stops_at_the_limit() {
        // A limit of one home means slot 2 is never handed out, however free.
        assert_eq!(next_slot(&[1], 1), None);
        assert_eq!(next_slot(&[], 1), Some(1));
    }
}
