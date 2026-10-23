//! Month, week/day and agenda views.

use calcraft_ui::{
    cal::{
        date::{DAY, clock, format_time, iso_week, long_day, weekday},
        layout::{columns, lanes},
        moves::{Change, day_piece},
        nav::{EventKey, STEP_MINUTES, View, event_key, grid_move},
        zone::to_local,
    },
    text as t,
};
use leptos::{ev, html, prelude::*};

use super::{Cal, DragMode, Item, focus_later, start_drag, zone::Browser};

/// Chips per month cell before “+n more”.
const MONTH_CHIPS: usize = 3;
/// Hour shown at the top of the time grid.
const FIRST_HOUR: i32 = 7;

/// A round color mark (SVG fill, validated color).
#[component]
pub fn Dot(color: Option<String>) -> impl IntoView {
    view! {
        <svg class="dot" viewBox="0 0 10 10" aria-hidden="true" focusable="false">
            <circle
                cx="5"
                cy="5"
                r="5"
                fill=color.unwrap_or_else(|| "currentColor".to_owned())
            />
        </svg>
    }
}

/// Keyboard use of a selected event: move, resize, open.
fn event_keydown(cal: Cal, item: &Item, event: &ev::KeyboardEvent) {
    let all_day = item.span.all_day;
    match event_key(&event.key(), event.shift_key(), event.alt_key(), all_day) {
        Some(EventKey::Move { days, minutes }) => {
            event.prevent_default();
            event.stop_propagation();
            if item.instance.writable {
                cal.refocus.set(Some(item.key.clone()));
                cal.apply(
                    item,
                    Change {
                        days,
                        minutes,
                        resize: 0,
                    },
                );
            }
        }
        Some(EventKey::Resize { minutes }) => {
            event.prevent_default();
            event.stop_propagation();
            if item.instance.writable {
                cal.refocus.set(Some(item.key.clone()));
                cal.apply(
                    item,
                    Change {
                        days: 0,
                        minutes: 0,
                        resize: minutes,
                    },
                );
            }
        }
        Some(EventKey::Delete) => {
            event.prevent_default();
            event.stop_propagation();
            cal.open(item.instance.clone());
        }
        // Enter clicks the button.
        Some(EventKey::Open) | None => {}
    }
}

/// An event as a compact chip (month cells, all-day row).
fn chip(cal: Cal, item: Item, day: i64, with_time: bool) -> impl IntoView {
    let label = item.label();
    let title = item.title();
    let time = (with_time && !item.span.all_day).then(|| clock(item.span.start));
    let open_item = item.instance.clone();
    let drag_item = item.clone();
    let key_item = item.clone();
    view! {
        <button
            type="button"
            class="chip"
            class:all-day=item.span.all_day
            class:busy=item.instance.busy
            class:cancelled=item.instance.cancelled
            class:dragging=item.dragging
            class:draggable=item.instance.writable
            data-key=item.key.clone()
            aria-label=label
            on:click=move |e| {
                e.stop_propagation();
                cal.open(open_item.clone());
            }
            on:pointerdown=move |e| start_drag(cal, &drag_item, DragMode::Move, day, &e)
            on:keydown=move |e| event_keydown(cal, &key_item, &e)
        >
            <Dot color=item.color.clone() />
            {time.map(|time| view! { <span class="chip-time">{time}</span> })}
            <span class="chip-title">{title}</span>
        </button>
    }
}

/// Moves the keyboard focus in a day grid; switches the period when it
/// leaves it.
fn grid_keydown(cal: Cal, view: View, day: i64, event: &ev::KeyboardEvent) {
    if event.target() != event.current_target() {
        return;
    }
    if event.key() == "Enter" {
        event.prevent_default();
        cal.go_to_day(day);
        return;
    }
    let Some(target) = grid_move(&event.key(), day) else {
        return;
    };
    event.prevent_default();
    cal.focus.set(target);
    let (first, end) = view.range(cal.cursor.get_untracked());
    if target < first || target >= end {
        cal.cursor.set(target);
    }
    focus_later(format!("[data-day=\"{target}\"][role=\"gridcell\"]"));
}

#[component]
pub fn MonthView() -> impl IntoView {
    let cal = Cal::get();
    let range = move || View::Month.range(cal.cursor.get());
    // Keep the focused day inside the shown month grid.
    Effect::new(move |_| {
        let (first, end) = range();
        let focus = cal.focus.get_untracked();
        if focus < first || focus >= end {
            cal.focus.set(cal.cursor.get_untracked());
        }
    });
    let month = move || calcraft_ui::cal::date::civil(cal.cursor.get()).1;
    view! {
        <div class="month" role="grid" aria-labelledby="cal-title">
            <div class="month-head" role="row">
                {t::WEEKDAYS_SHORT
                    .iter()
                    .zip(t::WEEKDAYS_LONG)
                    .map(|(short, long)| {
                        view! {
                            <div class="month-wd" role="columnheader">
                                <abbr title=long>{*short}</abbr>
                            </div>
                        }
                    })
                    .collect_view()}
            </div>
            {move || {
                let (first, _) = range();
                let items = cal.items.get();
                let today = calcraft_ui::cal::zone::to_local(&Browser, cal.now.get()).div_euclid(DAY);
                let month = month();
                (0..6)
                    .map(|week| {
                        view! {
                            <div class="month-row" role="row">
                                {(0..7)
                                    .map(|column| {
                                        let day = first + week * 7 + column;
                                        let in_day: Vec<Item> = items
                                            .iter()
                                            .filter(|item| {
                                                let (a, b) = item.span.days();
                                                a <= day && day <= b
                                            })
                                            .cloned()
                                            .collect();
                                        month_cell(cal, day, today, month, in_day)
                                    })
                                    .collect_view()}
                            </div>
                        }
                    })
                    .collect_view()
            }}
        </div>
    }
}

fn month_cell(cal: Cal, day: i64, today: i64, month: u32, items: Vec<Item>) -> impl IntoView {
    let (_, cell_month, number) = calcraft_ui::cal::date::civil(day);
    let count = items.len();
    let label = format!(
        "{}{}",
        long_day(day),
        if count == 0 {
            String::new()
        } else {
            format!(", {count}")
        }
    );
    let more = count.saturating_sub(MONTH_CHIPS);
    let shown: Vec<Item> = if more > 0 {
        items.into_iter().take(MONTH_CHIPS - 1).collect()
    } else {
        items
    };
    let more = if more > 0 { more + 1 } else { 0 };
    let weekend = weekday(day) >= 5;
    view! {
        <div
            class="month-cell"
            class:today=day == today
            class:other-month=cell_month != month
            class:weekend=weekend
            role="gridcell"
            data-day=day.to_string()
            tabindex=move || if cal.focus.get() == day { "0" } else { "-1" }
            aria-selected=move || if cal.focus.get() == day { "true" } else { "false" }
            aria-label=label
            on:keydown=move |e| grid_keydown(cal, View::Month, day, &e)
            on:click=move |_| cal.focus.set(day)
            on:dblclick=move |_| cal.create(day * DAY + 9 * 3600, false)
        >
            <button
                type="button"
                class="day-number"
                tabindex="-1"
                aria-label=long_day(day)
                on:click=move |e| {
                    e.stop_propagation();
                    cal.go_to_day(day);
                }
            >
                {number}
            </button>
            <div class="month-chips">
                {shown.into_iter().map(|item| chip(cal, item, day, true)).collect_view()}
                {(more > 0)
                    .then(|| {
                        view! {
                            <button
                                type="button"
                                class="more"
                                on:click=move |e| {
                                    e.stop_propagation();
                                    cal.go_to_day(day);
                                }
                            >
                                {format!("+{more} {}", t::CAL_MORE)}
                            </button>
                        }
                    })}
            </div>
        </div>
    }
}

/// A timed event block in a day column.
#[derive(Debug, Clone, PartialEq)]
struct Block {
    item: Item,
    from: i64,
    to: i64,
    column: usize,
    columns: usize,
}

fn percent(value: f64) -> String {
    format!("{value:.4}%")
}

#[component]
pub fn TimeGrid(days: i64) -> impl IntoView {
    let cal = Cal::get();
    let view_kind = if days == 1 { View::Day } else { View::Week };
    let first = move || view_kind.range(cal.cursor.get()).0;
    let scroll = NodeRef::<html::Div>::new();
    // Start at 07:00.
    Effect::new(move |_| {
        if let Some(element) = scroll.get() {
            let hour = f64::from(element.scroll_height()) / 24.0;
            element.set_scroll_top((hour * f64::from(FIRST_HOUR)) as i32);
        }
    });
    let today = move || to_local(&Browser, cal.now.get()).div_euclid(DAY);

    let heads = move || {
        let first = first();
        let today = today();
        (0..days)
            .map(|offset| {
                let day = first + offset;
                let (_, _, number) = calcraft_ui::cal::date::civil(day);
                view! {
                    <div class="tg-dayhead" class:today=day == today>
                        <button
                            type="button"
                            class="link-like"
                            aria-label=long_day(day)
                            on:click=move |_| cal.go_to_day(day)
                        >
                            <span class="tg-wd">{t::WEEKDAYS_SHORT[weekday(day)]}</span>
                            " "
                            <strong>{number}</strong>
                        </button>
                    </div>
                }
            })
            .collect_view()
    };

    let all_day = move || {
        let first = first();
        let items: Vec<Item> = cal
            .items
            .get()
            .into_iter()
            .filter(|i| i.span.all_day)
            .filter(|i| {
                let (a, b) = i.span.days();
                b >= first && a < first + days
            })
            .collect();
        let ranges: Vec<(i64, i64)> = items
            .iter()
            .map(|i| {
                let (a, b) = i.span.days();
                (a.max(first) - first, b.min(first + days - 1) - first)
            })
            .collect();
        let lane_of = lanes(&ranges);
        (0..days)
            .map(|offset| {
                let day = first + offset;
                let mut in_day: Vec<(usize, Item)> = items
                    .iter()
                    .zip(&lane_of)
                    .filter(|(item, _)| {
                        let (a, b) = item.span.days();
                        a <= day && day <= b
                    })
                    .map(|(item, lane)| (*lane, item.clone()))
                    .collect();
                in_day.sort_by_key(|(lane, _)| *lane);
                view! {
                    <div
                        class="tg-alldaycell"
                        data-day=day.to_string()
                        on:dblclick=move |_| cal.create(day * DAY, true)
                    >
                        {in_day.into_iter().map(|(_, item)| chip(cal, item, day, false)).collect_view()}
                    </div>
                }
            })
            .collect_view()
    };

    let columns_view = move || {
        let first = first();
        let today = today();
        let now_local = to_local(&Browser, cal.now.get());
        let items = cal.items.get();
        (0..days)
            .map(|offset| {
                let day = first + offset;
                let pieces: Vec<(Item, i64, i64)> = items
                    .iter()
                    .filter_map(|item| {
                        day_piece(&item.span, day).map(|(from, to)| (item.clone(), from, to))
                    })
                    .collect();
                let spans: Vec<(i64, i64)> = pieces.iter().map(|(_, a, b)| (*a, *b)).collect();
                let placed = columns(&spans, 20);
                let blocks: Vec<Block> = pieces
                    .into_iter()
                    .zip(placed)
                    .map(|((item, from, to), placed)| Block {
                        item,
                        from,
                        to,
                        column: placed.column,
                        columns: placed.columns,
                    })
                    .collect();
                let now_line = (day == today).then(|| {
                    let minutes = now_local.rem_euclid(DAY) as f64 / 60.0;
                    view! {
                        <div class="tg-now" style:top=percent(minutes / 1440.0 * 100.0)>
                            <span class="visually-hidden">{clock(now_local)}</span>
                        </div>
                    }
                });
                view! {
                    <div
                        class="tg-col"
                        class:today=day == today
                        data-day=day.to_string()
                        role="group"
                        aria-label=long_day(day)
                        on:click=move |e: ev::MouseEvent| {
                            if e.target() != e.current_target() {
                                return;
                            }
                            let height = e
                                .current_target()
                                .and_then(|t| {
                                    wasm_bindgen::JsCast::dyn_into::<web_sys::Element>(t).ok()
                                })
                                .map(|el| el.get_bounding_client_rect().height())
                                .unwrap_or(1.0)
                                .max(1.0);
                            let minutes = (f64::from(e.offset_y()) / height * 1440.0) as i64;
                            let minutes = minutes - minutes.rem_euclid(30);
                            cal.create(day * DAY + minutes * 60, false);
                        }
                    >
                        {blocks.into_iter().map(|block| event_block(cal, day, block)).collect_view()}
                        {now_line}
                    </div>
                }
            })
            .collect_view()
    };

    view! {
        <div class="tgrid" class:days-1=days == 1 class:days-7=days == 7>
            <div class="tg-head">
                <div class="tg-corner muted small">
                    {move || format!("{} {}", t::CAL_VIEW_WEEK, iso_week(first()))}
                </div>
                {heads}
            </div>
            <div class="tg-allday">
                <div class="tg-corner muted small">{t::CAL_ALL_DAY}</div>
                {all_day}
            </div>
            <div class="tg-scroll" node_ref=scroll>
                <div class="tg-body">
                    <div class="tg-hours" aria-hidden="true">
                        {(0..24)
                            .map(|hour| view! { <div class="tg-hour"><span>{format_time(hour * 60)}</span></div> })
                            .collect_view()}
                    </div>
                    {columns_view}
                </div>
            </div>
        </div>
    }
}

fn event_block(cal: Cal, day: i64, block: Block) -> impl IntoView {
    let item = block.item;
    let top = block.from as f64 / 1440.0 * 100.0;
    let height = ((block.to - block.from).max(STEP_MINUTES) as f64) / 1440.0 * 100.0;
    let width = 100.0 / block.columns as f64;
    let left = width * block.column as f64;
    let short = block.to - block.from < 45;
    let tiny = block.to - block.from < 30;
    let label = item.label();
    let title = item.title();
    let time = item.time_label();
    let location = item.instance.location.clone();
    let open_item = item.instance.clone();
    let drag_item = item.clone();
    let resize_item = item.clone();
    let key_item = item.clone();
    let writable = item.instance.writable;
    view! {
        <button
            type="button"
            class="ev"
            class:short=short
            class:tiny=tiny
            class:busy=item.instance.busy
            class:cancelled=item.instance.cancelled
            class:dragging=item.dragging
            class:draggable=writable
            data-key=item.key.clone()
            aria-label=label
            style:top=percent(top)
            style:height=percent(height)
            style:left=percent(left)
            style:width=percent(width)
            on:click=move |e| {
                e.stop_propagation();
                cal.open(open_item.clone());
            }
            on:pointerdown=move |e| start_drag(cal, &drag_item, DragMode::Move, day, &e)
            on:keydown=move |e| event_keydown(cal, &key_item, &e)
        >
            <svg class="ev-bg" viewBox="0 0 1 1" preserveAspectRatio="none" aria-hidden="true">
                <rect width="1" height="1" fill=item.color.clone().unwrap_or_else(|| "currentColor".to_owned()) />
            </svg>
            <span class="ev-title">{title}</span>
            <span class="ev-time">{time}</span>
            {location.filter(|_| !short).map(|l| view! { <span class="ev-location">{l}</span> })}
            {writable
                .then(|| {
                    view! {
                        <span
                            class="ev-resize"
                            aria-hidden="true"
                            title=t::CAL_RESIZE
                            on:pointerdown=move |e| {
                                start_drag(cal, &resize_item, DragMode::Resize, day, &e)
                            }
                        ></span>
                    }
                })}
        </button>
    }
}

#[component]
pub fn AgendaView() -> impl IntoView {
    let cal = Cal::get();
    move || {
        let (first, end) = View::Agenda.range(cal.cursor.get());
        let items = cal.items.get();
        let today = to_local(&Browser, cal.now.get()).div_euclid(DAY);
        let sections: Vec<_> = (first..end)
            .filter_map(|day| {
                let in_day: Vec<Item> = items
                    .iter()
                    .filter(|item| {
                        let (a, b) = item.span.days();
                        a <= day && day <= b
                    })
                    .cloned()
                    .collect();
                (!in_day.is_empty()).then(|| {
                    view! {
                        <section class="agenda-day" class:today=day == today>
                            <h2 class="agenda-date">{long_day(day)}</h2>
                            <ul class="agenda-list">
                                {in_day
                                    .into_iter()
                                    .map(|item| {
                                        let open_item = item.instance.clone();
                                        let key_item = item.clone();
                                        let calendar = cal
                                            .calendar(&item.instance.calendar)
                                            .and_then(|c| c.display_name)
                                            .unwrap_or_else(|| item.instance.calendar.clone());
                                        view! {
                                            <li>
                                                <button
                                                    type="button"
                                                    class="agenda-item"
                                                    class:cancelled=item.instance.cancelled
                                                    data-key=item.key.clone()
                                                    aria-label=item.label()
                                                    on:click=move |_| cal.open(open_item.clone())
                                                    on:keydown=move |e| event_keydown(cal, &key_item, &e)
                                                >
                                                    <span class="agenda-time">{item.time_label()}</span>
                                                    <Dot color=item.color.clone() />
                                                    <span class="agenda-text">
                                                        <span class="agenda-title">{item.title()}</span>
                                                        {item
                                                            .instance
                                                            .location
                                                            .clone()
                                                            .map(|l| view! { <span class="muted small">{l}</span> })}
                                                    </span>
                                                    <span class="muted small agenda-cal">{calendar}</span>
                                                </button>
                                            </li>
                                        }
                                    })
                                    .collect_view()}
                            </ul>
                        </section>
                    }
                })
            })
            .collect();
        if sections.is_empty() {
            if cal.loaded.get() {
                view! { <p class="muted empty">{t::CAL_NO_EVENTS}</p> }.into_any()
            } else {
                view! { <p class="muted">{t::LOADING}</p> }.into_any()
            }
        } else {
            view! { <div class="agenda">{sections}</div> }.into_any()
        }
    }
}
