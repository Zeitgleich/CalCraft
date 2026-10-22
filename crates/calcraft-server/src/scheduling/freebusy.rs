//! Free/busy time of a user (RFC 6638 §5): only periods, never event data.
//!
//! Counted are VEVENTs in the user's writable calendars (not
//! subscriptions, not calendars marked `schedule-calendar-transp`
//! transparent) that are not `TRANSP:TRANSPARENT`, not cancelled and not
//! declined by the user. Recurring events are expanded within budgets;
//! what cannot be evaluated in time counts as busy over its indexed span
//! (conservative: never "free" when it might be busy).

use calcraft_core::CollectionKind;
use calcraft_dav::xml::NS_CALDAV;
use calcraft_ical::{
    Budget, OBJECT_BUDGET,
    itip::{BusyType, busy_periods, merge_periods},
};
use calcraft_store::{StoreError, User};

use super::normalize;
use crate::app::AppState;

/// Work for one user's lookup (see [`calcraft_ical::Budget`]).
const LOOKUP_WORK: u64 = 2_000_000;
/// Objects evaluated exactly per lookup; further ones count as busy.
const MAX_OBJECTS: usize = 2_000;
/// Bytes of object data read per lookup.
const MAX_BYTES: usize = 64 * 1024 * 1024;
/// Candidates fetched per query.
const PAGE: u32 = 500;
/// Most busy periods reported per user.
const MAX_PERIODS: usize = 10_000;

/// Whether a calendar's `schedule-calendar-transp` dead property says
/// transparent (RFC 6638 §9.1).
fn transparent(value: &str) -> bool {
    calcraft_dav::tree::parse(value.as_bytes(), 16 * 1024)
        .is_ok_and(|node| node.is(NS_CALDAV, "transparent"))
}

/// The busy periods of `user` in `[start, end)`, merged and sorted.
pub async fn busy(
    state: &AppState,
    user: &User,
    addresses: &[String],
    start: i64,
    end: i64,
) -> Result<Vec<(i64, i64, BusyType)>, StoreError> {
    let store = &state.store;
    let calendars = store.collections(user.id, CollectionKind::Calendar).await?;
    let properties = store
        .dead_properties_of_owner(user.id, CollectionKind::Calendar)
        .await?;
    let mut periods = Vec::new();
    let mut contents = Vec::new();
    let mut bytes = 0usize;
    for calendar in calendars.iter().filter(|c| !c.read_only) {
        let skip = properties.iter().any(|(id, p)| {
            *id == calendar.id
                && p.namespace == NS_CALDAV
                && p.name == "schedule-calendar-transp"
                && transparent(&p.value)
        });
        if skip {
            continue;
        }
        let mut after: Option<String> = None;
        loop {
            let page = store
                .freebusy_candidates(calendar.id, start, end, after.as_deref(), PAGE)
                .await?;
            let Some(last) = page.last() else { break };
            after = Some(last.name.clone());
            for candidate in &page {
                let conservative = (
                    candidate.range_start.unwrap_or(start).max(start),
                    candidate.range_end.unwrap_or(end).min(end),
                    BusyType::Busy,
                );
                if contents.len() >= MAX_OBJECTS || bytes >= MAX_BYTES {
                    periods.push(conservative);
                    continue;
                }
                if let Some(item) = store.item(calendar.id, &candidate.name).await? {
                    bytes = bytes.saturating_add(item.content.len());
                    contents.push((item.content, conservative));
                }
            }
            if page.len() < PAGE as usize {
                break;
            }
        }
    }

    let Ok(permit) = state.expansion.clone().acquire_owned().await else {
        return Ok(merge_periods(periods));
    };
    let addresses = addresses.to_vec();
    let exact = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let is_user = |v: &str| addresses.contains(&normalize(v));
        let mut work = LOOKUP_WORK;
        let mut out = Vec::new();
        for (content, conservative) in contents {
            let granted = OBJECT_BUDGET.min(work);
            let mut budget = Budget::new(granted);
            match busy_periods(&content, start, end, &is_user, &mut budget) {
                Some(found) => out.extend(found),
                None => out.push(conservative),
            }
            work = work.saturating_sub(granted.saturating_sub(budget.remaining()));
        }
        out
    })
    .await
    .unwrap_or_default();
    periods.extend(exact);
    periods.retain(|(lo, hi, _)| hi > lo);
    let mut merged = merge_periods(periods);
    merged.truncate(MAX_PERIODS);
    Ok(merged)
}
