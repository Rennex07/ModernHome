//! The permission nodes the plugin reads, and how a home limit is found.
//!
//! ## The nodes are the Paper ones, spelled out in full
//!
//! `advancedhomes.limit.N` and `home.admin` are what the Paper plugin read, and
//! they are what this one reads. Pumpkin passes a permission node through
//! verbatim - there is no namespace rewriting in `has_permission`, and the host
//! checks a player's own grants for an exact match *before* it consults the
//! registry (`pumpkin-util/src/permission.rs:336-391`) - so a grant from
//! PumpkinPerm resolves here with no plugin-to-plugin call.
//!
//! PumpkinPerm is what makes a grant possible, and it does so without knowing
//! this plugin exists: `groups::nodes_of` puts every entry of a group's
//! `permissions:` into the set the player is given, verbatim, whatever namespace
//! it is in, and `state::apply_to` hands it to `player.set_permission`. So a group
//! that wants ten homes and admin access says:
//!
//! ```yaml
//! groups:
//!   vip:
//!     weight: 10
//!     permissions:
//!       - "advancedhomes.limit.10"
//! ```
//!
//! and `/ahome` is gated on `home.admin` the same way. This is the same
//! relationship PumpkinTAB has with `PumpkinPerm:group.<name>` - one agreed string,
//! no IPC, no dependency in either plugin's metadata - and it is deliberately
//! *not* registered here. Nothing is prefixed with this plugin's own name,
//! because doing so would strand every grant that already exists.
//!
//! ## `default: op` became an explicit op check
//!
//! Paper declared `permissions: home.admin: default: op` in `plugin.yml`, which
//! made the node *exist* with a default of "ops have it". A Pumpkin plugin
//! cannot register a node outside its own namespace - the host rejects
//! `register_permission` for anything not prefixed with the plugin name
//! (`plugin/api/context.rs:278`) - so `home.admin` is not registered here and
//! would resolve to `false` for everyone by falling through to a registry with no
//! entry for it.
//!
//! [`is_admin`] therefore asks the op manager as well, which is what
//! `default: op` meant for the vanilla case, and a `home.admin` grant from
//! PumpkinPerm is still honoured on top of that. The consequence to know
//! about: an operator is an admin here, as they were there.
//!
//! ## Why the limit is scanned rather than read
//!
//! Paper walked `player.getEffectivePermissions()` and took the biggest
//! `advancedhomes.limit.N`. The Pumpkin API has no way to list a player's
//! permissions - only to ask about one node at a time - so the limit is found by
//! asking about each candidate from 1 up to `home-limit-scan-max`. That is one
//! host call per candidate. The result is refreshed before menu actions so an
//! already-open screen cannot retain a revoked limit.
//!
//! ## Placeholders are not PumpkinPIE's to resolve
//!
//! `%index%`, `%home%`, `%homes_count%`, `%max_homes%`, `%page%`, `%total_pages%`
//! and `%server%` are all filled in by this plugin, exactly as the Paper plugin
//! filled them in before it ever wrote to the screen. PumpkinPIE is for a plugin
//! asking *another* plugin about something it cannot know, and nothing here
//! needs to. Anything left in a template that this plugin did not recognise is
//! left exactly as written, which is what [`crate::text::apply`] does.

use pumpkin_plugin_api::{Server, player::Player};

/// The node that makes a player an admin: unlimited homes, no teleport delay.
pub const ADMIN: &str = "home.admin";

/// The prefix of a per-player home limit, so `advancedhomes.limit.5`.
pub const LIMIT_PREFIX: &str = "advancedhomes.limit.";

/// Whether `player` is an admin.
///
/// The op check is the `default: op` the Paper `plugin.yml` declared; the node
/// check is the direct grant. Either is enough, as `isOp() ||
/// hasPermission(...)` was either.
pub fn is_admin(server: &Server, player: &Player) -> bool {
    if player.has_permission(ADMIN) {
        return true;
    }
    server.get_op_manager().is_op(player.get_id())
}

/// Rechecked at each GUI action and when a pending chat rename is submitted.
pub fn can_manage(server: &Server, player: &Player, target: &str) -> bool {
    let own = crate::state::uuid_of(player) == target;
    let root = if own {
        crate::USE_HOME
    } else {
        crate::USE_ADMIN
    };
    access_allowed(
        own,
        player.has_permission(root),
        !own && is_admin(server, player),
    )
}

fn access_allowed(own: bool, root: bool, admin: bool) -> bool {
    root && (own || admin)
}

/// The most homes `player` may have.
///
/// An admin's answer is `i32::MAX`, which is what Java's `Integer.MAX_VALUE` was.
/// The pages that produces are computed in [`crate::homes_gui`], not here.
///
/// Everyone else gets the largest `advancedhomes.limit.N` they hold, with a floor
/// of 1: Paper's loop started at `max = 1` and only ever raised it, so a player
/// with no limit permission has a single home.
pub fn max_homes(server: &Server, player: &Player, scan_max: i32) -> i32 {
    if is_admin(server, player) {
        return i32::MAX;
    }

    let mut max = 1;
    for candidate in 1..=scan_max.max(1) {
        if player.has_permission(&format!("{LIMIT_PREFIX}{candidate}")) {
            max = candidate;
        }
    }
    max
}

/// The number of pages a player with `limit` homes needs, at `per_page` each.
///
/// `max(1, ceil(limit / per_page))`, which is `HomeGuiHandler.getTotalPages()`
/// exactly. For an admin the limit is `i32::MAX` and the answer is the same
/// 306,783,379 the Java version computed, which is why the next-page button
/// always shows for them.
///
/// A `per_page` of 0 returns 1 rather than dividing by zero. `parseLayout`
/// cannot produce 0 home slots while the shipped layout stands, but a layout of
/// all `-` would, and Java guarded it with an explicit early return.
#[must_use]
pub fn total_pages(limit: i32, per_page: usize) -> usize {
    if per_page == 0 {
        return 1;
    }
    let pages = (f64::from(limit) / per_page as f64).ceil();
    (pages as i64).clamp(1, i64::from(i32::MAX)) as usize
}

/// The lowest free slot between 1 and `limit`, or `-1` when there is none.
///
/// `/sethome` filled the first gap rather than appending, so deleting home 2 and
/// setting a new one puts it back at 2.
pub fn next_slot(taken: &[i32], limit: i32) -> i32 {
    for number in 1..=limit {
        if !taken.contains(&number) {
            return number;
        }
    }
    -1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revoked_root_or_admin_access_is_not_kept_by_an_open_menu() {
        assert!(access_allowed(true, true, false));
        assert!(!access_allowed(true, false, false));
        assert!(access_allowed(false, true, true));
        assert!(!access_allowed(false, true, false));
        assert!(!access_allowed(false, false, true));
    }

    #[test]
    fn the_page_count_is_rounded_up_never_down() {
        // 7 slots a page and 8 homes is two pages, not one. `ceil` is the whole
        // rule: an eighth home on a second page is useless if the button is not
        // there.
        assert_eq!(total_pages(7, 7), 1);
        assert_eq!(total_pages(8, 7), 2);
        assert_eq!(total_pages(14, 7), 2);
        assert_eq!(total_pages(15, 7), 3);
    }

    #[test]
    fn an_empty_layout_cannot_divide_by_zero() {
        // A layout of all `-` has no home slots, and `getTotalPages` had an
        // explicit guard for it. Same guard here.
        assert_eq!(total_pages(5, 0), 1);
        assert_eq!(total_pages(i32::MAX, 0), 1);
    }

    #[test]
    fn an_admins_page_count_is_the_huge_number_the_java_one_was() {
        // `Integer.MAX_VALUE / 7` rounded up is 306,783,379, and that is why the
        // next-page button never disappears for an admin. Asserted so the number
        // is known to be deliberate rather than an accident of the cast.
        assert_eq!(total_pages(i32::MAX, 7), 306_783_379);
    }

    #[test]
    fn a_limit_of_zero_still_shows_one_page() {
        assert_eq!(total_pages(0, 7), 1);
    }

    #[test]
    fn the_lowest_free_slot_is_the_first_gap() {
        assert_eq!(next_slot(&[1, 2, 4], 10), 3);
        assert_eq!(next_slot(&[], 5), 1);
        assert_eq!(next_slot(&[1, 2, 3], 3), -1, "a full set has no gap");
        assert_eq!(next_slot(&[1, 2, 3], 4), 4);
    }

    #[test]
    fn a_slot_above_the_limit_is_not_given_out() {
        // The scan stops at the limit, so a player with one home can never be
        // handed slot 2 even though it is free.
        assert_eq!(next_slot(&[1], 1), -1);
    }

    #[test]
    fn the_node_names_are_the_paper_ones_unchanged() {
        // Not prefixed with this plugin's namespace: Pumpkin passes nodes through
        // verbatim, and a renamed plugin must not strand existing grants. This is
        // also what makes the plugin's own `register_permission` - which rejects
        // anything outside its namespace - not apply to these.
        assert_eq!(ADMIN, "home.admin");
        assert_eq!(LIMIT_PREFIX, "advancedhomes.limit.");
        assert!(!ADMIN.contains(':') || ADMIN.starts_with("home"));
    }
}
