//! Case-sensitive Pumpkin permissions in the `ModernHome:` namespace.
//!
//! `ModernHome:admin` is registered with an operator-level-two default and gates
//! both command visibility and administrative actions. All checks go through
//! Pumpkin's permission manager so explicit denials are not bypassed by an op check.
//!
//! The highest granted `ModernHome:limit.N` wins, bounded by
//! `home-limit-scan-max`. Limits are refreshed before menu actions so an open
//! screen cannot retain a revoked grant.

use pumpkin_plugin_api::{Server, player::Player};

/// The node that makes a player an admin: unlimited homes, no teleport delay.
pub const ADMIN: &str = crate::USE_ADMIN;

/// The prefix of a per-player home limit, for example `ModernHome:limit.5`.
pub const LIMIT_PREFIX: &str = "ModernHome:limit.";

/// Whether `player` is an admin.
///
/// The registered default grants operators access; explicit denials still win.
pub fn is_admin(_server: &Server, player: &Player) -> bool {
    player.has_permission(ADMIN)
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
/// Everyone else gets the largest `ModernHome:limit.N` they hold, with a floor
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
    fn permission_nodes_use_the_exact_plugin_namespace() {
        assert_eq!(ADMIN, "ModernHome:admin");
        assert_eq!(LIMIT_PREFIX, "ModernHome:limit.");
        assert!(ADMIN.starts_with(&format!("{}:", crate::PLUGIN)));
        assert!(LIMIT_PREFIX.starts_with(&format!("{}:", crate::PLUGIN)));
    }
}
