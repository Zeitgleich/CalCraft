//! Views, the periods they show, and the keyboard model.

use super::date::{add_months, month_grid_start, month_start, week_start};
use crate::text;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Month,
    Week,
    Day,
    Agenda,
}

/// Days the agenda shows.
pub const AGENDA_DAYS: i64 = 30;

impl View {
    pub const ALL: [View; 4] = [View::Month, View::Week, View::Day, View::Agenda];

    pub fn title(self) -> &'static str {
        match self {
            Self::Month => text::CAL_VIEW_MONTH,
            Self::Week => text::CAL_VIEW_WEEK,
            Self::Day => text::CAL_VIEW_DAY,
            Self::Agenda => text::CAL_VIEW_AGENDA,
        }
    }

    /// Single-letter shortcut.
    pub fn key(self) -> &'static str {
        match self {
            Self::Month => "m",
            Self::Week => "w",
            Self::Day => "d",
            Self::Agenda => "a",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Month => "month",
            Self::Week => "week",
            Self::Day => "day",
            Self::Agenda => "agenda",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == value)
    }

    /// The days shown for the period containing `cursor`: `[first, end)`.
    pub fn range(self, cursor: i64) -> (i64, i64) {
        match self {
            Self::Month => {
                let first = month_grid_start(cursor);
                (first, first + 42)
            }
            Self::Week => {
                let first = week_start(cursor);
                (first, first + 7)
            }
            Self::Day => (cursor, cursor + 1),
            Self::Agenda => (cursor, cursor + AGENDA_DAYS),
        }
    }

    /// The cursor of the previous (`-1`) or next (`1`) period.
    pub fn step(self, cursor: i64, direction: i64) -> i64 {
        match self {
            Self::Month => add_months(month_start(cursor), direction),
            Self::Week => cursor + 7 * direction,
            Self::Day => cursor + direction,
            Self::Agenda => cursor + AGENDA_DAYS * direction,
        }
    }
}

/// A command of the calendar page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Previous,
    Next,
    Today,
    Show(View),
    NewEvent,
}

/// Page-wide shortcuts (not while typing; no modifier keys).
pub fn shortcut(key: &str, modified: bool) -> Option<Command> {
    if modified {
        return None;
    }
    Some(match key {
        "t" => Command::Today,
        "n" => Command::NewEvent,
        "j" | "PageDown" => Command::Next,
        "k" | "PageUp" => Command::Previous,
        key => Command::Show(View::ALL.into_iter().find(|v| v.key() == key)?),
    })
}

/// Movement of the focused day in a day grid (month, week).
pub fn grid_move(key: &str, focused: i64) -> Option<i64> {
    Some(match key {
        "ArrowLeft" => focused - 1,
        "ArrowRight" => focused + 1,
        "ArrowUp" => focused - 7,
        "ArrowDown" => focused + 7,
        "Home" => week_start(focused),
        "End" => week_start(focused) + 6,
        _ => return None,
    })
}

/// A keyboard change of a selected event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKey {
    /// Move by days and minutes.
    Move {
        days: i64,
        minutes: i64,
    },
    /// Change the end by minutes.
    Resize {
        minutes: i64,
    },
    Open,
    Delete,
}

/// Step of keyboard moves and of drag and drop.
pub const STEP_MINUTES: i64 = 15;

/// Keys on a selected event: Shift+arrows move it (days left/right,
/// 15 minutes up/down), Alt+↑/↓ change its end, Enter opens, Delete
/// deletes. All-day events move by days only.
pub fn event_key(key: &str, shift: bool, alt: bool, all_day: bool) -> Option<EventKey> {
    let minutes = if all_day { 0 } else { STEP_MINUTES };
    Some(match (key, shift, alt) {
        ("Enter", false, false) => EventKey::Open,
        ("Delete", false, false) => EventKey::Delete,
        ("ArrowLeft", true, false) => EventKey::Move {
            days: -1,
            minutes: 0,
        },
        ("ArrowRight", true, false) => EventKey::Move {
            days: 1,
            minutes: 0,
        },
        ("ArrowUp", true, false) if all_day => EventKey::Move {
            days: -7,
            minutes: 0,
        },
        ("ArrowDown", true, false) if all_day => EventKey::Move {
            days: 7,
            minutes: 0,
        },
        ("ArrowUp", true, false) => EventKey::Move {
            days: 0,
            minutes: -minutes,
        },
        ("ArrowDown", true, false) => EventKey::Move { days: 0, minutes },
        ("ArrowUp", false, true) if !all_day => EventKey::Resize { minutes: -minutes },
        ("ArrowDown", false, true) if !all_day => EventKey::Resize { minutes },
        _ => return None,
    })
}
