//! iTIP processing for scheduling: projections, change detection, reply
//! merging and free/busy must never panic, and merging an attendee's reply
//! into the organizer's copy may change nothing but that attendee's
//! PARTSTAT (plus new overrides, at most one per reply).
//!
//! The input is an organizer's copy and an attendee's copy, separated by a
//! line `--` (or the same object twice). The attendee is every address
//! containing "bob".
#![no_main]

use calcraft_ical::{
    Budget, Component, OBJECT_BUDGET,
    itip::{self, busy_periods},
    parse,
    write::to_string,
};
use libfuzzer_sys::fuzz_target;

fn is_bob(value: &str) -> bool {
    value.to_ascii_lowercase().contains("bob")
}

/// The component with bob's PARTSTAT removed, for comparisons.
fn without_partstat(component: &Component) -> Component {
    let mut component = component.clone();
    for property in &mut component.properties {
        if property.name == "ATTENDEE" && is_bob(&property.value) {
            property.parameters.retain(|p| p.name != "PARTSTAT");
        }
    }
    component
}

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let (first, second) = text.split_once("\n--\n").unwrap_or((text, text));
    let (Ok(organizer), Ok(attendee)) = (parse(first.as_bytes()), parse(second.as_bytes())) else {
        return;
    };
    let _ = itip::organizer(&organizer);
    let _ = itip::uid(&organizer);
    if let Some(view) = itip::project(&organizer, &is_bob) {
        assert!(itip::same_for_attendee(&view, &view));
        let copy = itip::attendee_copy(&view, Some(&attendee));
        let _ = to_string(&copy);
        let _ = to_string(&itip::message("REQUEST", &view, "20260101T000000Z"));
        let _ = itip::is_cancelled(&itip::cancelled(&view));
    }

    for replies in [
        itip::replies(&attendee, &organizer, &is_bob),
        itip::decline_all(&organizer),
    ] {
        let mut merged = organizer.clone();
        let changed = itip::apply_replies(&mut merged, &is_bob, &replies);
        assert!(merged.components.len() >= organizer.components.len());
        assert!(merged.components.len() - organizer.components.len() <= replies.len());
        assert!(merged.properties == organizer.properties);
        for (before, after) in organizer.components.iter().zip(&merged.components) {
            assert!(
                without_partstat(before) == without_partstat(after),
                "a reply changed more than the attendee's PARTSTAT"
            );
        }
        for added in &merged.components[organizer.components.len()..] {
            assert!(added.property("RECURRENCE-ID").is_some());
        }
        let _ = to_string(&itip::reply_message(&merged, &changed, &is_bob, "20260101T000000Z"));
    }

    let mut budget = Budget::new(OBJECT_BUDGET);
    if let Some(periods) = busy_periods(first.as_bytes(), 0, 4_000_000_000, &is_bob, &mut budget) {
        for (lo, hi, _) in periods {
            assert!(0 <= lo && lo < hi && hi <= 4_000_000_000);
        }
    }
});
