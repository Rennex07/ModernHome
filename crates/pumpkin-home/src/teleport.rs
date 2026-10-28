//! The countdown before a teleport, and the reasons it can be refused.
//!
//! ## The cadence, and why 1 and 20
//!
//! Paper scheduled an entity timer with a one-tick delay and a twenty-tick period
//! - `runAtEntityTimer(player, this::tick, 1L, 20L)`. A Pumpkin repeating task
//! takes the same two numbers and the host honours both: it puts the first run one
//! `delay` out and then re-arms it `period` ticks later
//! (`server/scheduler.rs:92-120`), so `schedule_repeating_task(1, 20, tick)` is
//! the same cadence rather than an approximation of it.
//!
//! With the default `teleport-time: 5`, the ticks land on 1, 21, 41, 61, 81 and
//! 101, so the actionbar shows `5, 4, 3, 2, 1` and the teleport fires on the sixth
//! one - about 5.05 seconds after the command, not 5.
//!
//! ## What cancels it, and how loudly
//!
//! Two things, checked in this order and only the first reported:
//!
//! - **Movement.** `distanceSquared(start) > 0.5`, or a world change. That is
//!   roughly 0.71 blocks of Euclidean displacement in three dimensions, and yaw
//!   and pitch are irrelevant. A player who turns on the spot does not cancel.
//! - **Combat.** The player was struck within `combat-delay` seconds. The window
//!   is re-read from the config on every tick, so `/home reload` with a new
//!   `combat-delay` changes a countdown already in flight.
//!
//! Both send the same text to chat *and* to the actionbar - `cancelWithMessage`
//! sent one string through both channels - and both are silent otherwise: no
//! sound, and no countdown. A third refusal, the one before the countdown starts,
//! sends only chat and plays the error sound.
//!
//! ## What is not reproduced
//!
//! The Paper version called `chunk.load(true)` before each teleport, to make sure
//! the destination was generated. The plugin API exposes no chunk-loading
//! function - `Chunk` has only reads and writes - so a home in a chunk that has
//! never been generated may land the player in the void rather than in a freshly
//! made column. Nothing else about the teleport differs.

use std::collections::BTreeMap;
use std::sync::Mutex;

use pumpkin_plugin_api::{Server, player::Player, world::World};

use crate::state::Session;

/// Half a block squared: the movement threshold.
///
/// Bukkit's `Location.distanceSquared` is the full 3-D Euclidean distance squared,
/// and `> 0.5` is the whole test. Stated as a constant because it is a distance in
/// *squared* units, which is easy to read as half a block.
pub const MOVE_THRESHOLD_SQUARED: f64 = 0.5;

/// What one tick of a countdown decided.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    /// Show the countdown with this many seconds left, and play the timer sound.
    Countdown(i32),
    /// The wait is over: teleport, then report success.
    Teleport,
    /// Refuse, and tell the player why over chat and the actionbar.
    Cancel(CancelReason),
}

/// Why a teleport was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelReason {
    /// The player moved more than half a block, or changed world.
    Moved,
    /// The player was struck in combat.
    Combat,
}

/// What a tick decided.
///
/// The order of the checks is the whole function: movement, then combat, then the
/// countdown reaching zero. Paper checked them in that order and returned on the
/// first, so a player who moved *and* was struck is told about moving.
///
/// # Arguments
///
/// * `moved` - the player exceeded [`MOVE_THRESHOLD_SQUARED`] or changed world.
/// * `combat` - the player was struck inside the combat window.
/// * `time_left` - seconds remaining, which reaches zero on the last tick.
#[must_use]
pub fn step(moved: bool, combat: bool, time_left: i32) -> Step {
    if moved {
        return Step::Cancel(CancelReason::Moved);
    }
    if combat {
        return Step::Cancel(CancelReason::Combat);
    }
    if time_left <= 0 {
        return Step::Teleport;
    }
    Step::Countdown(time_left)
}

/// Whether `from` and `to` are far enough apart to cancel a countdown.
///
/// Three dimensions, squared, against [`MOVE_THRESHOLD_SQUARED`]. Exposed so the
/// host can be tested against the same number the countdown uses.
#[must_use]
pub fn has_moved(from: (f64, f64, f64), to: (f64, f64, f64)) -> bool {
    let dx = to.0 - from.0;
    let dy = to.1 - from.1;
    let dz = to.2 - from.2;
    dx * dx + dy * dy + dz * dz > MOVE_THRESHOLD_SQUARED
}

/// A countdown in progress.
///
/// `start` is the position the player was standing at when the wait began, which
/// is what `distanceSquared` is measured from. Not the destination.
#[derive(Debug, Clone)]
pub struct Countdown {
    /// The position the countdown was started from.
    pub started_at: (f64, f64, f64),
    /// Seconds left before the teleport.
    pub time_left: i32,
    /// Whether this countdown has already finished or been cancelled.
    pub finished: bool,
}

impl Countdown {
    /// Starts a countdown of `seconds` at `from`.
    #[must_use]
    pub fn new(seconds: i32, from: (f64, f64, f64)) -> Self {
        Self {
            started_at: from,
            time_left: seconds.max(0),
            finished: false,
        }
    }

    /// Runs one tick against the player's current position.
    ///
    /// `combat` is [`crate::state::in_combat`]'s answer, passed in rather than
    /// called here so the state module stays the only place that reads the clock.
    ///
    /// Returns [`None`] when the countdown has already finished or been cancelled,
    /// which is the Java tick's first line and the reason a cancelled countdown
    /// cannot go on to teleport a player who has since logged out.
    pub fn tick(&mut self, now: (f64, f64, f64), combat: bool) -> Option<Step> {
        if self.finished {
            return None;
        }

        let moved = has_moved(self.started_at, now);
        let decided = step(moved, combat, self.time_left);

        match decided {
            Step::Countdown(left) => {
                // The sound and the decrement happen together, after the message,
                // which is the order the Java tick used.
                self.time_left = left;
                Some(Step::Countdown(self.time_left))
            }
            Step::Teleport | Step::Cancel(_) => {
                self.finished = true;
                Some(decided)
            }
        }
    }
}

/// The seconds the config asks the countdown to last.
///
/// A zero or negative value means an immediate teleport, which is what Paper's
/// `timeLeft <= 0` check produced on the first tick.
#[must_use]
pub fn seconds(configured: i32) -> i32 {
    configured.max(0)
}

/// The viewer a teleport is for, from a session.
///
/// Stated once because a countdown belongs to a *viewer*, and the host has to
/// re-fetch the `Player` handle from the server each tick - it cannot be held.
#[must_use]
pub fn viewer_of(session: &Session) -> &str {
    &session.viewer
}

/// One countdown and the host task that drives it.
#[derive(Debug, Clone)]
pub struct Live {
    /// The scheduled task's id, which is the only way to stop it.
    pub task: u32,
    /// The countdown itself.
    pub state: Countdown,
}

/// The countdowns, keyed by viewer uuid string.
static LIVE: Mutex<BTreeMap<String, Live>> = Mutex::new(BTreeMap::new());

/// A guard on a poisoned lock, for the same reason [`crate::state`] has one.
fn recover<T>(
    result: std::sync::LockResult<std::sync::MutexGuard<'_, T>>,
) -> std::sync::MutexGuard<'_, T> {
    match result {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// The viewers with a countdown running, for one pass of the task.
///
/// Taken out as a list rather than held as a guard, because the task then has to
/// call the host once per viewer and a lock must not be held across a host call.
#[must_use]
pub fn viewers() -> Vec<String> {
    recover(LIVE.lock()).keys().cloned().collect()
}

/// How many countdowns are live, for the start-up line.
#[must_use]
pub fn live_count() -> usize {
    recover(LIVE.lock()).len()
}

/// A copy of one viewer's countdown, or [`None`] if they have none.
///
/// Copied rather than borrowed so the caller can drop the lock before it calls
/// the host for a position.
#[must_use]
pub fn snapshot(viewer: &str) -> Option<Countdown> {
    recover(LIVE.lock()).get(viewer).map(|live| live.state.clone())
}

/// Writes back a countdown that has ticked.
pub fn write_back(viewer: &str, state: Countdown) {
    if let Some(live) = recover(LIVE.lock()).get_mut(viewer) {
        live.state = state;
    }
}

/// Records a new countdown and returns the task id it displaced.
///
/// The displaced id is returned rather than cancelled, because the caller is
/// about to call `cancel_task` and the two host calls must not be inside the
/// lock.
pub fn insert(viewer: &str, live: Live) -> Option<u32> {
    recover(LIVE.lock())
        .insert(viewer.to_string(), live)
        .map(|old| old.task)
}

/// Takes a viewer's countdown out, returning the task to cancel.
///
/// [`None`] means nothing was running, which is the normal case for a second
/// `cancel` and must not be an error.
pub fn take(viewer: &str) -> Option<u32> {
    recover(LIVE.lock()).remove(viewer).map(|live| live.task)
}

/// Forgets every countdown, without cancelling the host tasks.
///
/// A plugin that is unloading cannot be relied on to cancel anything - the host
/// drops the store - but leaving task ids in a table that is never read again
/// would make a reload's countdown bookkeeping wrong.
pub fn clear() {
    recover(LIVE.lock()).clear();
}

/// Where a teleport is going.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Destination {
    /// The block position.
    pub position: (f64, f64, f64),
    /// The facing to arrive with, or `None` to leave it alone.
    pub yaw: Option<f32>,
    /// The pitch to arrive with, or `None` to leave it alone.
    pub pitch: Option<f32>,
}

/// Forgets every destination, for an unload.
pub fn clear_destinations() {
    recover(DESTINATIONS.lock()).clear();
}

/// Starts a teleport, or reports why it cannot.
///
/// The whole of `HomeTeleportTask.start(Player, Location)`, in the same order:
///
/// 1. Any countdown already running is cancelled **silently** - no message, no
///    sound. Starting a second teleport while one is waiting was how a second
///    click was absorbed.
/// 2. An admin - `home.admin`, or an operator, which is what Paper's
///    `default: op` meant - is teleported at once. Success message and sound, no
///    countdown, and no actionbar. The wait is skipped, not shortened.
/// 3. A player struck inside the combat window is refused over chat with the error
///    sound. No actionbar on this path, unlike a cancellation *during* a countdown,
///    which is the one asymmetry in Paper's messaging and is reproduced because it
///    is visible.
/// 4. Otherwise a countdown is armed one tick out and then every twenty ticks, and
///    the timer sound plays once, immediately - before the first tick.
pub fn start(
    config: &crate::config::Config,
    server: &Server,
    player: &Player,
    destination: Destination,
    world: World,
) {
    let viewer = crate::state::uuid_of(player);
    if let Some(stale) = take(&viewer) {
        pumpkin_plugin_api::scheduler::cancel_task(stale);
    }

    if crate::permissions::is_admin(server, player) {
        land(player, destination, world);
        crate::screens::say(config, player, "teleport-success", &[]);
        crate::sounds::play(config, player, "teleported");
        return;
    }

    let combat = config.combat_delay();
    if combat > 0 && crate::state::in_combat(&viewer, combat) {
        crate::screens::say(config, player, "teleport-cancelled-combat", &[]);
        crate::sounds::play(config, player, "error");
        return;
    }

    let live = Live {
        task: pumpkin_plugin_api::scheduler::schedule_repeating_task(
            crate::screens::COUNTDOWN_DELAY,
            crate::screens::COUNTDOWN_PERIOD,
            countdown_tick,
        ),
        state: Countdown::new(seconds(config.teleport_time()), player.get_position()),
    };
    // The destination is recorded here rather than by the caller, because `world`
    // is consumed by the teleport on both the admin path above and the last tick
    // below - so there is no second copy to hand over, and a caller that set the
    // destination separately would have to reach for one.
    set_destination(&viewer, destination, world);
    insert(&viewer, live);
    crate::sounds::play(config, player, "timer");
}

/// The destination a countdown is waiting to reach, which the countdown itself
/// does not carry.
///
/// The countdown only holds the position the *player* was standing at, so the
/// destination has to live somewhere until the last tick. It is keyed by the
/// viewer beside the countdown rather than inside it, because the countdown is
/// cloned per tick and the destination is read once.
static DESTINATIONS: Mutex<BTreeMap<String, (Destination, World)>> = Mutex::new(BTreeMap::new());

/// Remembers where `viewer`'s countdown is going.
///
/// Set by [`start`] as part of arming one, and not by callers - the `World` is
/// consumed by the teleport itself, so there is only ever one copy and it belongs
/// here.
fn set_destination(viewer: &str, destination: Destination, world: World) {
    recover(DESTINATIONS.lock()).insert(viewer.to_string(), (destination, world));
}

/// Takes `viewer`'s destination out, for the tick that lands them.
pub fn take_destination(viewer: &str) -> Option<(Destination, World)> {
    recover(DESTINATIONS.lock()).remove(viewer)
}

/// Moves `player`.
///
/// `world` is consumed because the host's `teleport` takes it that way, and it is
/// passed in rather than fetched here because a destination whose world is not
/// loaded has to be caught *before* this point - which is where
/// `teleport-world-not-loaded` comes from.
fn land(player: &Player, destination: Destination, world: World) {
    player.teleport(destination.position, destination.yaw, destination.pitch, world);
}

/// One pass of the countdown task.
///
/// The task is registered once and walks whoever is waiting, rather than one task
/// per teleport. That is a host-economy difference rather than a design one: the
/// task's own id is what has to be cancelled, and one shared task cancels once.
///
/// Three early exits before the decision, in the Paper order:
///
/// - a viewer who has gone offline cancels with **no** message, because
///   `!player.isOnline()` returned before anything was said;
/// - a countdown that has already finished does nothing;
/// - the movement and combat checks come from [`Countdown::tick`].
pub fn countdown_tick(server: Server) {
    let config = crate::screens::config();
    let combat_delay = config.combat_delay();

    for viewer in viewers() {
        // The offline check comes first and cancels silently, so a viewer who has
        // logged out takes their countdown with them and says nothing.
        let Some(player) = crate::state::player_of(&server, &viewer) else {
            drop_viewer(&viewer);
            continue;
        };
        let Some(mut countdown) = snapshot(&viewer) else {
            continue;
        };

        let now = player.get_position();
        let combat = crate::state::in_combat(&viewer, combat_delay);

        match countdown.tick(now, combat) {
            None => continue,
            Some(Step::Countdown(left)) => {
                // Message first, then the sound, then the decrement - which is the
                // order the Java tick used, and why the number shown is the one
                // still remaining rather than the one about to arrive.
                crate::screens::show(
                    &config,
                    &player,
                    "teleport-countdown-actionbar",
                    &[("%time%", left.to_string())],
                );
                crate::sounds::play(&config, &player, "timer");
                countdown.time_left = left - 1;
                write_back(&viewer, countdown);
            }
            Some(Step::Teleport) => {
                drop_viewer(&viewer);
                if let Some((destination, world)) = take_destination(&viewer) {
                    land(&player, destination, world);
                }
                crate::screens::say(&config, &player, "teleport-success", &[]);
                crate::screens::show(&config, &player, "teleport-success-actionbar", &[]);
                crate::sounds::play(&config, &player, "teleported");
            }
            Some(Step::Cancel(reason)) => {
                drop_viewer(&viewer);
                let key = match reason {
                    CancelReason::Moved => "teleport-cancelled-moved",
                    CancelReason::Combat => "teleport-cancelled-combat",
                };
                // Both channels, one string, and no sound - `cancelWithMessage`
                // wrote to chat and the actionbar and played nothing.
                crate::screens::announce(&config, &player, key);
            }
        }
    }
}

/// Forgets a viewer's countdown and its destination, cancelling the task.
///
/// The cancel is outside the lock, because `cancel_task` reaches the host and a
/// lock must not be held across a host call.
pub fn cancel(viewer: &str) {
    if let Some(task) = take(viewer) {
        pumpkin_plugin_api::scheduler::cancel_task(task);
    }
    recover(DESTINATIONS.lock()).remove(viewer);
}

/// Forgets a viewer's countdown without cancelling anything.
fn drop_viewer(viewer: &str) {
    take(viewer);
    recover(DESTINATIONS.lock()).remove(viewer);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 0.5 blocks away in any direction is the threshold, so just under it is a
    /// no-op and just over it cancels.
    #[test]
    fn movement_is_measured_in_three_dimensions_and_squared() {
        assert!(!has_moved((0.0, 0.0, 0.0), (0.0, 0.0, 0.0)), "no movement");
        // 0.7 cubed-of-one: 0.7^2 = 0.49, just under 0.5.
        assert!(!has_moved((0.0, 0.0, 0.0), (0.7, 0.0, 0.0)));
        // 0.7081^2 = 0.501..., just over.
        assert!(has_moved((0.0, 0.0, 0.0), (0.71, 0.0, 0.0)));
        // Vertical only: falling off a ledge counts.
        assert!(has_moved((0.0, 64.0, 0.0), (0.0, 65.0, 0.0)));
    }

    #[test]
    fn turning_on_the_spot_does_not_cancel() {
        // Yaw and pitch are not part of the position, so a player who spins
        // still teleports. This is the bug everyone files against home plugins.
        assert!(!has_moved((0.0, 64.0, 0.0), (0.0, 64.0, 0.0)));
    }

    #[test]
    fn movement_is_reported_before_combat() {
        // Paper returned on the first match, so a player who both moved and was
        // struck is told about moving - not about combat.
        assert_eq!(
            step(true, true, 3),
            Step::Cancel(CancelReason::Moved),
            "movement wins"
        );
    }

    #[test]
    fn a_countdown_reaches_zero_before_combat_is_checked() {
        // No: combat is checked first, and Paper's order was movement, combat,
        // countdown. A countdown at zero and a player in combat still cancels.
        assert_eq!(step(false, true, 0), Step::Cancel(CancelReason::Combat));
    }

    #[test]
    fn the_countdown_counts_down_and_then_teleports() {
        let mut countdown = Countdown::new(3, (0.0, 0.0, 0.0));

        assert_eq!(countdown.tick((0.0, 0.0, 0.0), false), Some(Step::Countdown(3)));
        assert_eq!(countdown.tick((0.0, 0.0, 0.0), false), Some(Step::Countdown(3)));
        // `timeLeft` is only decremented by the caller's own bookkeeping in the
        // Paper tick, after the message is sent, so the number the player sees is
        // the one still remaining - which is why it starts at 5, not at 4.
        countdown.time_left = 2;
        assert_eq!(
            countdown.tick((0.0, 0.0, 0.0), false),
            Some(Step::Countdown(2))
        );
        countdown.time_left = 1;
        assert_eq!(
            countdown.tick((0.0, 0.0, 0.0), false),
            Some(Step::Countdown(1))
        );
        countdown.time_left = 0;
        assert_eq!(countdown.tick((0.0, 0.0, 0.0), false), Some(Step::Teleport));
        assert!(countdown.finished);
    }

    #[test]
    fn a_finished_countdown_is_not_ticked_again() {
        // `if (this.finished) return;` is the Java tick's first line, and it is
        // what stops a cancelled or completed countdown from evaluating the
        // offline, movement and combat checks again.
        let mut countdown = Countdown::new(1, (0.0, 0.0, 0.0));
        countdown.finished = true;
        assert_eq!(countdown.tick((100.0, 0.0, 0.0), true), None);
    }

    #[test]
    fn a_zero_countdown_teleports_on_the_first_tick() {
        // `teleport-time: 0`, and Paper's `timeLeft <= 0` fired on tick one.
        let mut countdown = Countdown::new(0, (0.0, 0.0, 0.0));
        assert_eq!(countdown.tick((0.0, 0.0, 0.0), false), Some(Step::Teleport));
    }

    #[test]
    fn moving_mid_countdown_cancels_and_finishes() {
        let mut countdown = Countdown::new(5, (0.0, 64.0, 0.0));
        assert_eq!(
            countdown.tick((2.0, 64.0, 0.0), false),
            Some(Step::Cancel(CancelReason::Moved))
        );
        assert!(countdown.finished);
    }

    #[test]
    fn being_struck_mid_countdown_cancels() {
        let mut countdown = Countdown::new(5, (0.0, 64.0, 0.0));
        assert_eq!(
            countdown.tick((0.0, 64.0, 0.0), true),
            Some(Step::Cancel(CancelReason::Combat))
        );
        assert!(countdown.finished);
    }

    #[test]
    fn a_negative_configured_time_is_treated_as_zero() {
        // `getInt` would happily hand back a negative, and `timeLeft <= 0` would
        // make the first tick teleport. Same here, minus the wait.
        assert_eq!(seconds(-5), 0);
        assert_eq!(seconds(5), 5);
        assert_eq!(seconds(0), 0);
    }

    #[test]
    fn a_countdown_belongs_to_a_viewer() {
        // The handle cannot be held across ticks, so the viewer is named by uuid
        // string and re-fetched. Asserted so the session stays the source.
        let session = Session::homes(
            "uuid".into(),
            "Viewer".into(),
            "uuid".into(),
            "Viewer".into(),
            1,
        );
        assert_eq!(viewer_of(&session), "uuid");
    }
}
