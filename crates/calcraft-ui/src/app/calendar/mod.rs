//! The calendar page: month, week, day and agenda views of all calendars
//! of the user's home, the event dialog, drag and drop and keyboard use.
//!
//! Data comes from `/api/v1/calendars` and `/api/v1/events` (instances in
//! the visible range, expanded by the server); every change is a request
//! with the ETag the page last saw (`If-Match`). A conflict (412) shows a
//! message and reloads.
//!
//! Positions in the time grid are set as CSS properties through the CSSOM
//! (`style.setProperty`, percentages computed here), which the Content
//! Security Policy does not restrict; there are no `style` attributes in
//! the markup, and colors are still SVG `fill` values.

mod dialog;
mod views;
mod zone;

use calcraft_core::api::{CalendarSource, CalendarView, EventInstance, EventList, EventSaved};
use calcraft_ui::{
    cal::{
        date::{DAY, long_day, month_title, range_title},
        moves::{Change, LocalSpan, changed_span, changed_times, local_span},
        nav::{Command, View, shortcut},
        zone::day_start,
    },
    text as t,
    validate::is_valid_color,
};
use leptos::{ev, prelude::*, task::spawn_local};
use wasm_bindgen::JsCast;

use super::{Ctx, ErrorText, api};
use zone::Browser;

/// Key of the hidden calendars in the browser's local storage.
const HIDDEN_KEY: &str = "calcraft.calendar.hidden";
/// Clicks this soon after a drag are part of the drag, not a click.
const CLICK_AFTER_DRAG_MS: f64 = 400.0;

/// An instance as the views show it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Item {
    pub instance: EventInstance,
    /// In local time, with a running drag applied.
    pub span: LocalSpan,
    pub color: Option<String>,
    pub key: String,
    pub dragging: bool,
}

impl Item {
    pub fn title(&self) -> String {
        self.instance
            .summary
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| t::CAL_UNTITLED.to_owned())
    }

    /// `09:00 – 10:30` or “All day”.
    pub fn time_label(&self) -> String {
        if self.span.all_day {
            return t::CAL_ALL_DAY.to_owned();
        }
        format!(
            "{} – {}",
            calcraft_ui::cal::date::clock(self.span.start),
            calcraft_ui::cal::date::clock(self.span.end)
        )
    }

    /// What a screen reader announces for the event.
    pub fn label(&self) -> String {
        let (first, last) = self.span.days();
        let mut label = format!("{}, {}", self.title(), long_day(first));
        if last > first {
            label.push_str(&format!(" – {}", long_day(last)));
        }
        if !self.span.all_day {
            label.push_str(&format!(", {}", self.time_label()));
        }
        if let Some(location) = &self.instance.location {
            label.push_str(&format!(", {location}"));
        }
        if self.instance.recurring {
            label.push_str(&format!(", {}", t::CAL_REPEATS));
        }
        if self.instance.cancelled {
            label.push_str(&format!(", {}", t::CAL_CANCELLED));
        }
        if !self.instance.writable {
            label.push_str(&format!(", {}", t::CAL_READ_ONLY));
        }
        label
    }
}

pub(super) fn instance_key(instance: &EventInstance) -> String {
    format!(
        "{}/{}/{}",
        instance.calendar,
        instance.name,
        instance.recurrence_id.as_deref().unwrap_or("")
    )
}

/// What the dialog shows.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Opened {
    /// A new event at a local time (seconds), all-day or timed.
    New {
        start: i64,
        all_day: bool,
    },
    Existing(EventInstance),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DragMode {
    Move,
    Resize,
}

/// A running drag of an event.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Drag {
    item: Item,
    mode: DragMode,
    x: f64,
    y: f64,
    /// Pixels per minute of the time grid; 0 outside it (days only).
    per_minute: f64,
    origin_day: i64,
    change: Change,
    moved: bool,
}

/// State of the calendar page, provided as context.
#[derive(Clone, Copy)]
pub(super) struct Cal {
    pub calendars: RwSignal<Option<Vec<CalendarView>>>,
    pub hidden: RwSignal<Vec<String>>,
    pub view: RwSignal<View>,
    /// A day of the shown period.
    pub cursor: RwSignal<i64>,
    /// The day with keyboard focus in day grids.
    pub focus: RwSignal<i64>,
    pub events: RwSignal<Vec<EventInstance>>,
    pub truncated: RwSignal<bool>,
    pub loaded: RwSignal<bool>,
    pub error: RwSignal<Option<String>>,
    /// Polite announcements (moves).
    pub status: RwSignal<String>,
    pub dialog: RwSignal<Option<Opened>>,
    pub drag: RwSignal<Option<Drag>>,
    pub suppress_until: RwSignal<f64>,
    pub reload: RwSignal<u32>,
    /// Focus this event (key) once the events are reloaded.
    pub refocus: RwSignal<Option<String>>,
    pub now: RwSignal<i64>,
    pub tzid: RwSignal<String>,
    pub items: Memo<Vec<Item>>,
    /// The application context (event handlers run outside the owner).
    pub ctx: Ctx,
}

impl Cal {
    pub fn get() -> Self {
        expect_context::<Cal>()
    }

    pub fn today(&self) -> i64 {
        calcraft_ui::cal::zone::to_local(&Browser, self.now.get_untracked()).div_euclid(DAY)
    }

    /// Opens the dialog for an event (unless a drag just ended).
    pub fn open(&self, instance: EventInstance) {
        if js_sys::Date::now() < self.suppress_until.get_untracked() {
            return;
        }
        self.dialog.set(Some(Opened::Existing(instance)));
    }

    /// Opens the dialog for a new event.
    pub fn create(&self, start: i64, all_day: bool) {
        if js_sys::Date::now() < self.suppress_until.get_untracked() {
            return;
        }
        self.dialog.set(Some(Opened::New { start, all_day }));
    }

    pub fn go_to_day(&self, day: i64) {
        self.cursor.set(day);
        self.focus.set(day);
        self.view.set(View::Day);
    }

    pub fn calendar(&self, slug: &str) -> Option<CalendarView> {
        self.calendars
            .with_untracked(|list| list.as_ref()?.iter().find(|c| c.slug == slug).cloned())
    }

    /// Writable calendars, for new events and moves.
    pub fn writable(&self) -> Vec<CalendarView> {
        self.calendars
            .get()
            .unwrap_or_default()
            .into_iter()
            .filter(|c| c.writable)
            .collect()
    }

    /// Applies a drag or keyboard change of an instance (`PATCH`).
    pub fn apply(&self, item: &Item, change: Change) {
        if change.is_none() || !item.instance.writable {
            return;
        }
        let cal = *self;
        let ctx = cal.ctx;
        let times = changed_times(&item.instance, &Browser, change);
        let key = item.key.clone();
        // Shown at once; the reload brings the server's view.
        cal.events.update(|list| {
            for instance in list.iter_mut().filter(|i| instance_key(i) == key) {
                instance.start = times.start;
                instance.end = times.end;
            }
        });
        let path = event_path(&item.instance.calendar, &item.instance.name);
        let etag = item.instance.etag.clone();
        spawn_local(async move {
            let result: Result<EventSaved, _> =
                api::send_if_match("PATCH", &path, &etag, &times).await;
            match result {
                Ok(_) => {
                    cal.error.try_set(None);
                    cal.status.try_set(t::CAL_MOVED.to_owned());
                }
                Err(failure) => {
                    cal.error.try_set(Some(ctx.fail(&failure)));
                }
            }
            cal.reload.try_update(|n| *n += 1);
        });
    }
}

/// API path of an event.
pub(super) fn event_path(calendar: &str, name: &str) -> String {
    let encode = |s: &str| String::from(js_sys::encode_uri_component(s));
    format!("/v1/calendars/{}/events/{}", encode(calendar), encode(name))
}

fn load_hidden() -> Vec<String> {
    window()
        .local_storage()
        .ok()
        .flatten()
        .and_then(|storage| storage.get_item(HIDDEN_KEY).ok().flatten())
        .map(|value| {
            value
                .split('\n')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn save_hidden(hidden: &[String]) {
    if let Some(storage) = window().local_storage().ok().flatten() {
        let _ = storage.set_item(HIDDEN_KEY, &hidden.join("\n"));
    }
}

/// Whether keys go to a form field rather than to the page.
fn typing(event: &web_sys::KeyboardEvent) -> bool {
    event
        .target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        .is_some_and(|element| {
            matches!(
                element.tag_name().to_ascii_lowercase().as_str(),
                "input" | "textarea" | "select"
            ) || element.closest("dialog").ok().flatten().is_some()
        })
}

/// Focuses the first element matching `selector` after the next render.
pub(super) fn focus_later(selector: String) {
    request_animation_frame(move || {
        if let Some(element) = document()
            .query_selector(&selector)
            .ok()
            .flatten()
            .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
        {
            let _ = element.focus();
        }
    });
}

/// The day under a screen position (an element with `data-day`).
fn day_at(x: f64, y: f64) -> Option<i64> {
    document()
        .element_from_point(x as f32, y as f32)?
        .closest("[data-day]")
        .ok()
        .flatten()?
        .get_attribute("data-day")?
        .parse()
        .ok()
}

/// Starts dragging an event (pointer down on it or its resize handle).
pub(super) fn start_drag(
    cal: Cal,
    item: &Item,
    mode: DragMode,
    origin_day: i64,
    event: &ev::PointerEvent,
) {
    if event.button() != 0 || !item.instance.writable || item.instance.busy {
        return;
    }
    let per_minute = event
        .target()
        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        .and_then(|e| e.closest(".tg-col").ok().flatten())
        .map(|col| col.get_bounding_client_rect().height() / (24.0 * 60.0))
        .unwrap_or(0.0);
    if mode == DragMode::Resize {
        event.stop_propagation();
    }
    cal.drag.set(Some(Drag {
        item: item.clone(),
        mode,
        x: f64::from(event.client_x()),
        y: f64::from(event.client_y()),
        per_minute,
        origin_day,
        change: Change::default(),
        moved: false,
    }));
}

fn drag_move(cal: Cal, event: &ev::PointerEvent) {
    let Some(mut drag) = cal.drag.get_untracked() else {
        return;
    };
    let (x, y) = (f64::from(event.client_x()), f64::from(event.client_y()));
    let (dx, dy) = (x - drag.x, y - drag.y);
    if !drag.moved && dx * dx + dy * dy < 36.0 {
        return;
    }
    event.prevent_default();
    drag.moved = true;
    let minutes = if drag.per_minute > 0.0 {
        calcraft_ui::cal::moves::snap(
            (dy / drag.per_minute) as i64,
            calcraft_ui::cal::nav::STEP_MINUTES,
        )
    } else {
        0
    };
    let change = match drag.mode {
        DragMode::Move => {
            let days = day_at(x, y)
                .map(|day| day - drag.origin_day)
                .unwrap_or(drag.change.days);
            Change {
                days,
                minutes,
                resize: 0,
            }
        }
        DragMode::Resize => Change {
            days: 0,
            minutes: 0,
            resize: minutes,
        },
    };
    if change != drag.change
        || !cal
            .drag
            .with_untracked(|d| d.as_ref().is_some_and(|d| d.moved))
    {
        drag.change = change;
        cal.drag.set(Some(drag));
    }
}

fn drag_end(cal: Cal, apply: bool) {
    let Some(drag) = cal.drag.get_untracked() else {
        return;
    };
    cal.drag.set(None);
    if drag.moved {
        cal.suppress_until
            .set(js_sys::Date::now() + CLICK_AFTER_DRAG_MS);
        if apply {
            cal.apply(&drag.item, drag.change);
        }
    }
}

#[component]
pub fn CalendarPage() -> impl IntoView {
    let ctx = Ctx::get();
    let now = RwSignal::new(zone::now());
    let today = calcraft_ui::cal::zone::to_local(&Browser, now.get_untracked()).div_euclid(DAY);
    let calendars = RwSignal::new(None::<Vec<CalendarView>>);
    let hidden = RwSignal::new(load_hidden());
    let events = RwSignal::new(Vec::<EventInstance>::new());
    let drag = RwSignal::new(None::<Drag>);
    let items = Memo::new(move |_| {
        let hidden = hidden.get();
        let drag = drag.get();
        let colors: Vec<(String, Option<String>)> = calendars
            .get()
            .unwrap_or_default()
            .into_iter()
            .map(|c| (c.slug, c.color.filter(|c| is_valid_color(c))))
            .collect();
        let mut items: Vec<Item> = events
            .get()
            .into_iter()
            .filter(|i| !hidden.contains(&i.calendar))
            .map(|instance| {
                let key = instance_key(&instance);
                let mut span = local_span(&instance, &Browser);
                let dragging = drag.as_ref().is_some_and(|d| d.moved && d.item.key == key);
                if dragging && let Some(d) = &drag {
                    span = changed_span(&span, d.change);
                }
                let color = colors
                    .iter()
                    .find(|(slug, _)| *slug == instance.calendar)
                    .and_then(|(_, color)| color.clone());
                Item {
                    instance,
                    span,
                    color,
                    key,
                    dragging,
                }
            })
            .collect();
        items.sort_by(|a, b| {
            (b.span.all_day, a.span.start, a.span.end, a.title()).cmp(&(
                a.span.all_day,
                b.span.start,
                b.span.end,
                b.title(),
            ))
        });
        items
    });
    let cal = Cal {
        calendars,
        hidden,
        view: RwSignal::new(View::Month),
        cursor: RwSignal::new(today),
        focus: RwSignal::new(today),
        events,
        truncated: RwSignal::new(false),
        loaded: RwSignal::new(false),
        error: RwSignal::new(None),
        status: RwSignal::new(String::new()),
        dialog: RwSignal::new(None),
        drag,
        suppress_until: RwSignal::new(0.0),
        reload: RwSignal::new(0),
        refocus: RwSignal::new(None),
        now,
        tzid: RwSignal::new(zone::browser_tzid().unwrap_or_default()),
        items,
        ctx,
    };
    provide_context(cal);

    spawn_local(async move {
        match api::get::<Vec<CalendarView>>("/v1/calendars").await {
            Ok(list) => {
                calendars.try_set(Some(list));
            }
            Err(failure) => {
                cal.error.try_set(Some(ctx.fail(&failure)));
                calendars.try_set(Some(Vec::new()));
            }
        }
    });

    // The events of the shown period; a later request wins.
    let generation = StoredValue::new(0u32);
    Effect::new(move |_| {
        let (view, cursor) = (cal.view.get(), cal.cursor.get());
        cal.reload.track();
        let (first, end) = view.range(cursor);
        let (start, stop) = (day_start(&Browser, first), day_start(&Browser, end));
        generation.update_value(|g| *g += 1);
        let mine = generation.get_value();
        spawn_local(async move {
            let result =
                api::get::<EventList>(&format!("/v1/events?start={start}&end={stop}")).await;
            if generation.try_get_value() != Some(mine) {
                return;
            }
            match result {
                Ok(list) => {
                    cal.truncated.try_set(list.truncated);
                    cal.events.try_set(list.instances);
                }
                Err(failure) => {
                    cal.error.try_set(Some(ctx.fail(&failure)));
                }
            }
            cal.loaded.try_set(true);
            if let Some(key) = cal.refocus.try_update(Option::take).flatten() {
                focus_later(format!("[data-key=\"{key}\"]"));
            }
        });
    });

    // The current-time line and "today" follow the clock.
    let ticker = set_interval_with_handle(
        move || {
            now.try_set(zone::now());
        },
        std::time::Duration::from_secs(60),
    );
    let pointer_move = window_event_listener(ev::pointermove, move |e| drag_move(cal, &e));
    let pointer_up = window_event_listener(ev::pointerup, move |_| drag_end(cal, true));
    let pointer_cancel = window_event_listener(ev::pointercancel, move |_| drag_end(cal, false));
    let keys = window_event_listener(ev::keydown, move |e| {
        if e.key() == "Escape" && cal.drag.get_untracked().is_some() {
            drag_end(cal, false);
            return;
        }
        if e.default_prevented() || typing(&e) || cal.dialog.get_untracked().is_some() {
            return;
        }
        let modified = e.ctrl_key() || e.meta_key() || e.alt_key();
        let Some(command) = shortcut(&e.key(), modified) else {
            return;
        };
        e.prevent_default();
        run(cal, command);
    });
    on_cleanup(move || {
        if let Ok(ticker) = ticker {
            ticker.clear();
        }
        pointer_move.remove();
        pointer_up.remove();
        pointer_cancel.remove();
        keys.remove();
    });

    view! {
        <div class="cal-page">
            <Toolbar />
            <div class="cal-layout">
                <Sidebar />
                <section class="cal-main" aria-labelledby="cal-title">
                    <ErrorText error=cal.error />
                    <Show when=move || cal.truncated.get()>
                        <p class="warning" role="status">{t::CAL_TRUNCATED}</p>
                    </Show>
                    {move || match cal.view.get() {
                        View::Month => view! { <views::MonthView /> }.into_any(),
                        View::Week => view! { <views::TimeGrid days=7 /> }.into_any(),
                        View::Day => view! { <views::TimeGrid days=1 /> }.into_any(),
                        View::Agenda => view! { <views::AgendaView /> }.into_any(),
                    }}
                </section>
            </div>
            <p class="visually-hidden" role="status" aria-live="polite">{move || cal.status.get()}</p>
            {move || cal.dialog.get().map(|opened| view! { <dialog::EventDialog opened /> })}
        </div>
    }
}

/// Runs a toolbar or keyboard command.
fn run(cal: Cal, command: Command) {
    match command {
        Command::Previous | Command::Next => {
            let direction = if command == Command::Next { 1 } else { -1 };
            let view = cal.view.get_untracked();
            let cursor = view.step(cal.cursor.get_untracked(), direction);
            cal.cursor.set(cursor);
            cal.focus.set(cursor);
        }
        Command::Today => {
            let today = cal.today();
            cal.cursor.set(today);
            cal.focus.set(today);
        }
        Command::Show(view) => {
            cal.cursor.set(cal.focus.get_untracked());
            cal.view.set(view);
        }
        Command::NewEvent => {
            let day = cal.focus.get_untracked();
            cal.create(day * DAY + 9 * 3600, false);
        }
    }
}

fn period_title(view: View, cursor: i64) -> String {
    match view {
        View::Month => month_title(cursor),
        View::Week => {
            let (first, end) = view.range(cursor);
            range_title(first, end - 1)
        }
        View::Day => long_day(cursor),
        View::Agenda => {
            let (first, end) = view.range(cursor);
            range_title(first, end - 1)
        }
    }
}

#[component]
fn Toolbar() -> impl IntoView {
    let cal = Cal::get();
    view! {
        <div class="cal-toolbar">
            <div class="cal-nav">
                <button
                    type="button"
                    class="secondary icon"
                    aria-label=t::CAL_PREVIOUS
                    title=t::CAL_PREVIOUS
                    on:click=move |_| run(cal, Command::Previous)
                >
                    "‹"
                </button>
                <button type="button" class="secondary" on:click=move |_| run(cal, Command::Today)>
                    {t::CAL_TODAY}
                </button>
                <button
                    type="button"
                    class="secondary icon"
                    aria-label=t::CAL_NEXT
                    title=t::CAL_NEXT
                    on:click=move |_| run(cal, Command::Next)
                >
                    "›"
                </button>
            </div>
            <h1 id="cal-title" class="cal-title">
                {move || period_title(cal.view.get(), cal.cursor.get())}
            </h1>
            <div class="segmented" role="group" aria-label=t::CAL_VIEW_LABEL>
                {View::ALL
                    .into_iter()
                    .map(|view| {
                        view! {
                            <button
                                type="button"
                                aria-pressed=move || if cal.view.get() == view { "true" } else { "false" }
                                title=format!("{} ({})", view.title(), view.key())
                                on:click=move |_| run(cal, Command::Show(view))
                            >
                                {view.title()}
                            </button>
                        }
                    })
                    .collect_view()}
            </div>
            <button
                type="button"
                class="cal-new"
                title=format!("{} (n)", t::CAL_NEW_EVENT)
                on:click=move |_| run(cal, Command::NewEvent)
            >
                "+ "
                {t::CAL_NEW_EVENT}
            </button>
        </div>
    }
}

#[component]
fn Sidebar() -> impl IntoView {
    let cal = Cal::get();
    let toggle = move |slug: String| {
        cal.hidden.update(|hidden| {
            if let Some(at) = hidden.iter().position(|s| *s == slug) {
                hidden.remove(at);
            } else {
                hidden.push(slug);
            }
            save_hidden(hidden);
        });
    };
    view! {
        <aside class="cal-side" aria-label=t::CAL_CALENDARS>
            <h2>{t::CAL_CALENDARS}</h2>
            {move || match cal.calendars.get() {
                None => view! { <p class="muted">{t::LOADING}</p> }.into_any(),
                Some(list) if list.is_empty() => {
                    view! { <p class="muted small">{t::CAL_NO_CALENDARS}</p> }.into_any()
                }
                Some(list) => {
                    view! {
                        <ul class="cal-list">
                            {list
                                .into_iter()
                                .map(|calendar| {
                                    let slug = calendar.slug.clone();
                                    let shown_slug = slug.clone();
                                    let name = calendar
                                        .display_name
                                        .clone()
                                        .filter(|n| !n.trim().is_empty())
                                        .unwrap_or_else(|| calendar.slug.clone());
                                    let note = match (calendar.source, calendar.writable) {
                                        (CalendarSource::Shared, writable) => {
                                            let owner = calendar.owner.clone().unwrap_or_default();
                                            Some(if writable {
                                                format!("{} {owner}", t::CAL_SHARED_BY)
                                            } else {
                                                format!("{} {owner}, {}", t::CAL_SHARED_BY, t::CAL_READ_ONLY)
                                            })
                                        }
                                        (CalendarSource::Subscription, _) => {
                                            Some(t::CAL_SUBSCRIPTION.to_owned())
                                        }
                                        (CalendarSource::Own, _) => None,
                                    };
                                    view! {
                                        <li>
                                            <label class="inline cal-toggle">
                                                <input
                                                    type="checkbox"
                                                    prop:checked=move || {
                                                        !cal.hidden.get().contains(&shown_slug)
                                                    }
                                                    on:change=move |_| toggle(slug.clone())
                                                />
                                                <views::Dot color=calendar.color.clone() />
                                                <span class="cal-name">{name}</span>
                                            </label>
                                            {note.map(|note| view! { <span class="muted small cal-note">{note}</span> })}
                                        </li>
                                    }
                                })
                                .collect_view()}
                        </ul>
                    }
                        .into_any()
                }
            }}
            <p class="muted small">
                {t::CAL_ZONE_NOTE} " "
                {move || {
                    let zone = cal.tzid.get();
                    if zone.is_empty() { "local time".to_owned() } else { zone }
                }}
            </p>
            <details class="cal-help">
                <summary>{t::CAL_SHORTCUTS}</summary>
                <p class="small">{t::CAL_SHORTCUTS_TEXT}</p>
            </details>
        </aside>
    }
}
