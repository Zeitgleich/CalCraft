//! The event dialog: create, show, change and delete an event, the series
//! or one instance of it. A native modal `<dialog>`: the rest of the page
//! is inert while it is open, Escape closes it, and the focus returns to
//! where it was.

use calcraft_core::api::{EventDetail, EventInput, EventSaved, MoveEvent, TimeZoneInfo};
use calcraft_ui::{
    ApiFailure,
    cal::{
        date::{civil, parse_date},
        form::{Ends, EventForm, Repeat, WEEKDAY_CODES, describe},
        zone::definition,
    },
    text as t,
};
use leptos::{html, prelude::*, task::spawn_local};
use wasm_bindgen::JsCast;

use super::{
    Cal, Opened, event_path,
    zone::{Browser, Named},
};
use crate::app::{Ctx, ErrorText, api};

/// The dialog's inputs.
#[derive(Clone, Copy)]
struct Fields {
    calendar: RwSignal<String>,
    summary: RwSignal<String>,
    location: RwSignal<String>,
    description: RwSignal<String>,
    all_day: RwSignal<bool>,
    start_date: RwSignal<String>,
    start_time: RwSignal<String>,
    end_date: RwSignal<String>,
    end_time: RwSignal<String>,
    tzid: RwSignal<String>,
    repeat: RwSignal<Repeat>,
    interval: RwSignal<String>,
    ends: RwSignal<Ends>,
    count: RwSignal<String>,
    until: RwSignal<String>,
    weekdays: RwSignal<[bool; 7]>,
    alarms: RwSignal<Vec<String>>,
    private: RwSignal<bool>,
}

impl Fields {
    fn new(form: &EventForm) -> Self {
        Self {
            calendar: RwSignal::new(form.calendar.clone()),
            summary: RwSignal::new(form.summary.clone()),
            location: RwSignal::new(form.location.clone()),
            description: RwSignal::new(form.description.clone()),
            all_day: RwSignal::new(form.all_day),
            start_date: RwSignal::new(form.start_date.clone()),
            start_time: RwSignal::new(form.start_time.clone()),
            end_date: RwSignal::new(form.end_date.clone()),
            end_time: RwSignal::new(form.end_time.clone()),
            tzid: RwSignal::new(form.tzid.clone()),
            repeat: RwSignal::new(form.repeat),
            interval: RwSignal::new(form.interval.clone()),
            ends: RwSignal::new(form.ends),
            count: RwSignal::new(form.count.clone()),
            until: RwSignal::new(form.until.clone()),
            weekdays: RwSignal::new(form.weekdays),
            alarms: RwSignal::new(form.alarms.clone()),
            private: RwSignal::new(form.private),
        }
    }

    fn set(&self, form: &EventForm) {
        self.calendar.set(form.calendar.clone());
        self.summary.set(form.summary.clone());
        self.location.set(form.location.clone());
        self.description.set(form.description.clone());
        self.all_day.set(form.all_day);
        self.start_date.set(form.start_date.clone());
        self.start_time.set(form.start_time.clone());
        self.end_date.set(form.end_date.clone());
        self.end_time.set(form.end_time.clone());
        self.tzid.set(form.tzid.clone());
        self.repeat.set(form.repeat);
        self.interval.set(form.interval.clone());
        self.ends.set(form.ends);
        self.count.set(form.count.clone());
        self.until.set(form.until.clone());
        self.weekdays.set(form.weekdays);
        self.alarms.set(form.alarms.clone());
        self.private.set(form.private);
    }

    fn form(&self) -> EventForm {
        EventForm {
            calendar: self.calendar.get_untracked(),
            summary: self.summary.get_untracked(),
            location: self.location.get_untracked(),
            description: self.description.get_untracked(),
            all_day: self.all_day.get_untracked(),
            start_date: self.start_date.get_untracked(),
            start_time: self.start_time.get_untracked(),
            end_date: self.end_date.get_untracked(),
            end_time: self.end_time.get_untracked(),
            tzid: self.tzid.get_untracked(),
            repeat: self.repeat.get_untracked(),
            interval: self.interval.get_untracked(),
            ends: self.ends.get_untracked(),
            count: self.count.get_untracked(),
            until: self.until.get_untracked(),
            weekdays: self.weekdays.get_untracked(),
            alarms: self.alarms.get_untracked(),
            private: self.private.get_untracked(),
        }
    }
}

/// The browser's definitions of the zone the form uses. A zone the
/// browser does not know is only accepted if the event already has it
/// (the server keeps the event's own definition).
fn zone_definitions(
    form: &EventForm,
    browser: &str,
    known: Option<&str>,
) -> Result<Vec<TimeZoneInfo>, &'static str> {
    let Some(zone) = form.zone() else {
        return Ok(Vec::new());
    };
    let year = parse_date(&form.start_date).map_or(2026, |d| civil(d).0);
    if zone == browser {
        return Ok(vec![definition(&Browser, &zone, year)]);
    }
    match Named::new(&zone) {
        Some(named) => Ok(vec![definition(&named, &zone, year)]),
        None if known == Some(zone.as_str()) => Ok(Vec::new()),
        None => Err(t::ERR_EV_ZONE),
    }
}

/// Only the server's own links: managed downloads and plain web links.
fn safe_link(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    url.starts_with("/api/v1/attachments/")
        || lower.starts_with("https://")
        || lower.starts_with("http://")
}

#[component]
pub fn EventDialog(opened: Opened) -> impl IntoView {
    let cal = Cal::get();
    let ctx = Ctx::get();
    let dialog = NodeRef::<html::Dialog>::new();
    let opener = StoredValue::new_local(
        document()
            .active_element()
            .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok()),
    );
    let browser_zone = cal.tzid.get_untracked();

    let (instance, is_new) = match &opened {
        Opened::Existing(instance) => (Some(instance.clone()), false),
        Opened::New { .. } => (None, true),
    };
    let recurring = instance.as_ref().is_some_and(|i| i.recurring);
    let default_calendar = cal
        .writable()
        .into_iter()
        .find(|c| !cal.hidden.get_untracked().contains(&c.slug))
        .or_else(|| cal.writable().into_iter().next())
        .map(|c| c.slug)
        .unwrap_or_default();
    let initial = match &opened {
        Opened::New { start, all_day } => {
            EventForm::new_at(&default_calendar, *start, *all_day, &browser_zone)
        }
        Opened::Existing(instance) => {
            EventForm::new_at(&instance.calendar, 0, false, &browser_zone)
        }
    };
    let fields = Fields::new(&initial);
    let detail = RwSignal::new(None::<EventDetail>);
    let loading = RwSignal::new(!is_new);
    let scope_all = RwSignal::new(!recurring);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let confirm_delete = RwSignal::new(false);

    let close = move || {
        cal.dialog.set(None);
    };
    let fail = move |failure: ApiFailure| {
        if failure.is_conflict() {
            cal.reload.update(|n| *n += 1);
        }
        error.try_set(Some(ctx.fail(&failure)));
    };

    // Loads the series (`all`) or the instance of an existing event.
    let load_instance = instance.clone();
    let load = Callback::new(move |all: bool| {
        let Some(instance) = load_instance.clone() else {
            return;
        };
        loading.set(true);
        let browser_zone = browser_zone.clone();
        spawn_local(async move {
            let mut path = event_path(&instance.calendar, &instance.name);
            if !all && let Some(rid) = &instance.recurrence_id {
                path.push_str("?recurrence_id=");
                path.push_str(&String::from(js_sys::encode_uri_component(rid)));
            }
            match api::get::<EventDetail>(&path).await {
                Ok(loaded) => {
                    fields.set(&EventForm::from_detail(&loaded, &browser_zone));
                    detail.try_set(Some(loaded));
                }
                Err(failure) => fail(failure),
            }
            loading.try_set(false);
        });
    });
    if !is_new {
        load.run(!recurring);
    }

    // Open modally, and focus the title once it can be edited.
    Effect::new(move |_| {
        let ready = !loading.get();
        if let Some(element) = dialog.get() {
            if !element.open() {
                let _ = element.show_modal();
            }
            if !ready {
                return;
            }
            if let Some(input) = document()
                .get_element_by_id("ev-summary")
                .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
            {
                let _ = input.focus();
            }
        }
    });
    // Back to where the focus was; an event that was re-rendered in the
    // meantime is found again by its key (also after the reload).
    let opener_key = instance.as_ref().map(super::instance_key);
    // Deferred: while the modal dialog is still in the page, the rest of it
    // is inert and cannot take the focus.
    on_cleanup(move || {
        let element = opener
            .try_with_value(|opener| opener.clone())
            .flatten()
            .filter(|e| e.tag_name() != "BODY");
        request_animation_frame(move || match element {
            Some(element) if element.is_connected() => {
                let _ = element.focus();
            }
            _ => {
                if let Some(key) = opener_key {
                    super::focus_later(format!("[data-key=\"{key}\"]"));
                }
            }
        });
    });

    let writable = move || is_new || detail.get().is_some_and(|d| d.writable && !d.busy);
    let read_only = move || !loading.get() && !writable();

    let save_instance = instance.clone();
    let save_zone = cal.tzid.get_untracked();
    let save = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        if busy.get_untracked() || !writable() {
            return;
        }
        let form = fields.form();
        let current = detail.get_untracked();
        let known = current.as_ref().and_then(|d| d.fields.start.tzid.clone());
        let zones = match zone_definitions(&form, &save_zone, known.as_deref()) {
            Ok(zones) => zones,
            Err(message) => {
                error.set(Some(message.to_owned()));
                return;
            }
        };
        let rid = if scope_all.get_untracked() {
            None
        } else {
            save_instance.as_ref().and_then(|i| i.recurrence_id.clone())
        };
        let input: EventInput = match form.to_input(rid, zones) {
            Ok(input) => input,
            Err(message) => {
                error.set(Some(message.to_owned()));
                return;
            }
        };
        if form.calendar.is_empty() || cal.calendar(&form.calendar).is_none_or(|c| !c.writable) {
            error.set(Some(t::ERR_EV_CALENDAR.to_owned()));
            return;
        }
        error.set(None);
        busy.set(true);
        let saved_key = save_instance.as_ref().map(super::instance_key);
        spawn_local(async move {
            let result: Result<(), ApiFailure> = async {
                match current {
                    None => {
                        let _: EventSaved = api::send(
                            "POST",
                            &format!(
                                "/v1/calendars/{}/events",
                                String::from(js_sys::encode_uri_component(&form.calendar))
                            ),
                            &input,
                        )
                        .await?;
                    }
                    Some(current) => {
                        let path = event_path(&current.calendar, &current.name);
                        let saved: EventSaved =
                            api::send_if_match("PUT", &path, &current.etag, &input).await?;
                        if form.calendar != current.calendar
                            && let Some(etag) = saved.etag
                        {
                            let _: EventSaved = api::send_if_match(
                                "POST",
                                &format!("{path}/move"),
                                &etag,
                                &MoveEvent {
                                    calendar: form.calendar.clone(),
                                },
                            )
                            .await?;
                        }
                    }
                }
                Ok(())
            }
            .await;
            busy.try_set(false);
            match result {
                Ok(()) => {
                    // The event is drawn anew after the reload: focus it there.
                    cal.refocus.set(saved_key);
                    cal.reload.update(|n| *n += 1);
                    close();
                }
                Err(failure) => fail(failure),
            }
        });
    };

    let delete_instance = instance.clone();
    let delete = Callback::new(move |one: bool| {
        let Some(current) = detail.get_untracked() else {
            return;
        };
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        let mut path = event_path(&current.calendar, &current.name);
        if one
            && let Some(rid) = delete_instance
                .as_ref()
                .and_then(|i| i.recurrence_id.clone())
        {
            path.push_str("?recurrence_id=");
            path.push_str(&String::from(js_sys::encode_uri_component(&rid)));
        }
        spawn_local(async move {
            let result = api::delete_if_match(&path, &current.etag).await;
            busy.try_set(false);
            match result {
                Ok(()) => {
                    cal.reload.update(|n| *n += 1);
                    close();
                }
                Err(failure) => fail(failure),
            }
        });
    });

    let title = move || {
        if is_new {
            t::EV_NEW
        } else if writable() {
            t::EV_EDIT
        } else {
            t::EV_VIEW
        }
    };
    let calendars = move || {
        let current = detail.get().map(|d| d.calendar);
        cal.calendars
            .get()
            .unwrap_or_default()
            .into_iter()
            .filter(|c| c.writable || current.as_deref() == Some(c.slug.as_str()))
            .collect::<Vec<_>>()
    };

    view! {
        <dialog
            node_ref=dialog
            class="ev-dialog"
            aria-labelledby="ev-dialog-title"
            on:cancel=move |e: web_sys::Event| {
                e.prevent_default();
                close();
            }
        >
            <form class="ev-form" on:submit=save novalidate>
                <header class="ev-head">
                    <h2 id="ev-dialog-title">{title}</h2>
                    <button type="button" class="secondary icon" aria-label=t::EV_CLOSE title=t::EV_CLOSE on:click=move |_| close()>
                        "×"
                    </button>
                </header>
                <ErrorText error />
                <Show when=move || loading.get()>
                    <p class="muted" role="status">{t::LOADING}</p>
                </Show>
                <Show when=read_only>
                    <p class="hint">
                        {move || {
                            if detail.get().is_some_and(|d| d.busy) { t::EV_BUSY_NOTE } else { t::EV_READ_ONLY }
                        }}
                    </p>
                </Show>
                {(recurring)
                    .then(|| {
                        view! {
                            <Show when=writable>
                                <fieldset class="ev-scope">
                                    <legend>{t::EV_SCOPE_QUESTION}</legend>
                                    <label class="inline">
                                        <input
                                            type="radio"
                                            name="ev-scope"
                                            prop:checked=move || !scope_all.get()
                                            on:change=move |_| {
                                                scope_all.set(false);
                                                load.run(false);
                                            }
                                        />
                                        {t::EV_SCOPE_ONE}
                                    </label>
                                    <label class="inline">
                                        <input
                                            type="radio"
                                            name="ev-scope"
                                            prop:checked=move || scope_all.get()
                                            on:change=move |_| {
                                                scope_all.set(true);
                                                load.run(true);
                                            }
                                        />
                                        {t::EV_SCOPE_ALL}
                                    </label>
                                </fieldset>
                            </Show>
                        }
                    })}
                <Show when=move || detail.get().is_some_and(|d| d.is_override) && !scope_all.get()>
                    <p class="hint">{t::EV_OVERRIDE_NOTE}</p>
                </Show>
                <fieldset class="ev-fields" prop:disabled=move || read_only() || loading.get()>
                    <label>
                        {t::EV_TITLE}
                        <input id="ev-summary" type="text" maxlength="1000" bind:value=fields.summary />
                    </label>
                    <label class="inline">
                        <input type="checkbox" bind:checked=fields.all_day />
                        {t::EV_ALL_DAY}
                    </label>
                    <div class="ev-times">
                        <fieldset class="ev-when">
                            <legend>{t::EV_START}</legend>
                            <input type="date" aria-label=format!("{} ({})", t::EV_START, "date") bind:value=fields.start_date />
                            <Show when=move || !fields.all_day.get()>
                                <input type="time" step="300" aria-label=format!("{} ({})", t::EV_START, "time") bind:value=fields.start_time />
                            </Show>
                        </fieldset>
                        <fieldset class="ev-when">
                            <legend>{t::EV_END}</legend>
                            <input type="date" aria-label=format!("{} ({})", t::EV_END, "date") bind:value=fields.end_date />
                            <Show when=move || !fields.all_day.get()>
                                <input type="time" step="300" aria-label=format!("{} ({})", t::EV_END, "time") bind:value=fields.end_time />
                            </Show>
                        </fieldset>
                    </div>
                    <Show when=move || !fields.all_day.get()>
                        <label>
                            {t::EV_TIMEZONE}
                            <input type="text" maxlength="64" autocomplete="off" spellcheck="false" bind:value=fields.tzid />
                            <span class="hint">{t::EV_TIMEZONE_HINT}</span>
                        </label>
                    </Show>
                    <label>
                        {t::EV_CALENDAR}
                        <select
                            prop:value=move || fields.calendar.get()
                            on:change=move |e| fields.calendar.set(event_target_value(&e))
                        >
                            {move || {
                                calendars()
                                    .into_iter()
                                    .map(|c| {
                                        let name = c.display_name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| c.slug.clone());
                                        let selected = fields.calendar.get_untracked() == c.slug;
                                        view! { <option value=c.slug.clone() selected=selected>{name}</option> }
                                    })
                                    .collect_view()
                            }}
                        </select>
                    </label>
                    <Show when=move || scope_all.get()>
                        <RepeatFields fields />
                    </Show>
                    <label>
                        {t::EV_LOCATION}
                        <input type="text" maxlength="1000" bind:value=fields.location />
                    </label>
                    <label>
                        {t::EV_DESCRIPTION}
                        <textarea rows="3" bind:value=fields.description></textarea>
                    </label>
                    <AlarmFields fields />
                    <label class="inline">
                        <input type="checkbox" bind:checked=fields.private />
                        {t::EV_PRIVATE}
                    </label>
                </fieldset>
                {move || detail.get().map(|d| view! { <Participants detail=d /> })}
                <footer class="ev-actions">
                    <Show when=move || writable() && !loading.get()>
                        <button type="submit" disabled=move || busy.get()>{t::SAVE}</button>
                    </Show>
                    <button type="button" class="secondary" on:click=move |_| close()>
                        {move || if writable() { t::CANCEL } else { t::EV_CLOSE }}
                    </button>
                    <Show when=move || !is_new && writable() && !loading.get()>
                        <button type="button" class="danger push" on:click=move |_| confirm_delete.set(true)>
                            {t::DELETE}
                        </button>
                    </Show>
                </footer>
                <Show when=move || confirm_delete.get()>
                    <div class="confirm" role="alertdialog" aria-labelledby="ev-delete-text">
                        <p id="ev-delete-text">{t::EV_DELETE_CONFIRM}</p>
                        {if recurring {
                            view! {
                                <button type="button" class="danger" disabled=move || busy.get() on:click=move |_| delete.run(true)>
                                    {t::EV_DELETE_ONE}
                                </button>
                                <button type="button" class="danger" disabled=move || busy.get() on:click=move |_| delete.run(false)>
                                    {t::EV_DELETE_ALL}
                                </button>
                            }
                                .into_any()
                        } else {
                            view! {
                                <button type="button" class="danger" disabled=move || busy.get() on:click=move |_| delete.run(false)>
                                    {t::DELETE}
                                </button>
                            }
                                .into_any()
                        }}
                        <button type="button" class="secondary" on:click=move |_| confirm_delete.set(false)>
                            {t::CANCEL}
                        </button>
                    </div>
                </Show>
            </form>
        </dialog>
    }
}

#[component]
fn RepeatFields(fields: Fields) -> impl IntoView {
    view! {
        <fieldset class="ev-repeat">
            <legend>{t::EV_REPEAT}</legend>
            <select
                aria-label=t::EV_REPEAT
                prop:value=move || fields.repeat.get().as_str()
                on:change=move |e| fields.repeat.set(Repeat::parse(&event_target_value(&e)))
            >
                {move || {
                    let custom = fields.repeat.get() == Repeat::Custom;
                    Repeat::CHOICES
                        .into_iter()
                        .chain(custom.then_some(Repeat::Custom))
                        .map(|r| {
                            view! {
                                <option value=r.as_str() selected=move || fields.repeat.get() == r>
                                    {r.label()}
                                </option>
                            }
                        })
                        .collect_view()
                }}
            </select>
            <Show when=move || !matches!(fields.repeat.get(), Repeat::None | Repeat::Custom)>
                <div class="ev-repeat-row">
                    <label>
                        {t::EV_INTERVAL}
                        <input type="number" min="1" max="999" inputmode="numeric" bind:value=fields.interval />
                    </label>
                    <label>
                        {t::EV_ENDS}
                        <select
                            prop:value=move || fields.ends.get().as_str()
                            on:change=move |e| fields.ends.set(Ends::parse(&event_target_value(&e)))
                        >
                            <option value="never" selected=move || fields.ends.get() == Ends::Never>{t::EV_ENDS_NEVER}</option>
                            <option value="count" selected=move || fields.ends.get() == Ends::Count>{t::EV_ENDS_COUNT}</option>
                            <option value="until" selected=move || fields.ends.get() == Ends::Until>{t::EV_ENDS_UNTIL}</option>
                        </select>
                    </label>
                    <Show when=move || fields.ends.get() == Ends::Count>
                        <label>
                            {t::EV_COUNT}
                            <input type="number" min="1" max="5000" inputmode="numeric" bind:value=fields.count />
                        </label>
                    </Show>
                    <Show when=move || fields.ends.get() == Ends::Until>
                        <label>
                            {t::EV_UNTIL}
                            <input type="date" bind:value=fields.until />
                        </label>
                    </Show>
                </div>
            </Show>
            <Show when=move || fields.repeat.get() == Repeat::Weekly>
                <fieldset class="ev-weekdays">
                    <legend>{t::EV_ON_DAYS}</legend>
                    {WEEKDAY_CODES
                        .iter()
                        .enumerate()
                        .map(|(index, _)| {
                            view! {
                                <label class="inline">
                                    <input
                                        type="checkbox"
                                        prop:checked=move || fields.weekdays.get()[index]
                                        on:change=move |e| {
                                            let on = event_target_checked(&e);
                                            fields.weekdays.update(|days| days[index] = on);
                                        }
                                    />
                                    <abbr title=t::WEEKDAYS_LONG[index]>{t::WEEKDAYS_SHORT[index]}</abbr>
                                </label>
                            }
                        })
                        .collect_view()}
                </fieldset>
            </Show>
        </fieldset>
    }
}

#[component]
fn AlarmFields(fields: Fields) -> impl IntoView {
    view! {
        <fieldset class="ev-alarms">
            <legend>{t::EV_REMINDERS}</legend>
            {move || {
                fields
                    .alarms
                    .get()
                    .into_iter()
                    .enumerate()
                    .map(|(index, minutes)| {
                        let label = minutes
                            .trim()
                            .parse::<i64>()
                            .map(t::alarm_label)
                            .unwrap_or_default();
                        view! {
                            <div class="ev-alarm">
                                <input
                                    type="number"
                                    min="0"
                                    max="40320"
                                    inputmode="numeric"
                                    aria-label=t::EV_MINUTES_BEFORE
                                    prop:value=minutes
                                    on:change=move |e| {
                                        let value = event_target_value(&e);
                                        fields.alarms.update(|list| {
                                            if let Some(slot) = list.get_mut(index) {
                                                *slot = value;
                                            }
                                        });
                                    }
                                />
                                <span class="muted small">{label}</span>
                                <button
                                    type="button"
                                    class="secondary small-button"
                                    on:click=move |_| {
                                        fields.alarms.update(|list| {
                                            if index < list.len() {
                                                list.remove(index);
                                            }
                                        })
                                    }
                                >
                                    {t::EV_REMOVE_REMINDER}
                                </button>
                            </div>
                        }
                    })
                    .collect_view()
            }}
            <button
                type="button"
                class="secondary small-button"
                on:click=move |_| fields.alarms.update(|list| list.push("15".to_owned()))
            >
                {t::EV_ADD_REMINDER}
            </button>
        </fieldset>
    }
}

/// Organizer, attendees and attachments (display only).
#[component]
fn Participants(detail: EventDetail) -> impl IntoView {
    let rule = detail.recurrence.as_ref().map(describe);
    let organizer = detail.organizer.clone();
    let attendees = detail.attendees.clone();
    let attachments: Vec<_> = detail
        .attachments
        .iter()
        .filter(|a| safe_link(&a.url))
        .cloned()
        .collect();
    let person = |p: &calcraft_core::api::Participant| match &p.name {
        Some(name) => format!("{name} <{}>", p.address),
        None => p.address.clone(),
    };
    view! {
        {rule.map(|rule| view! { <p class="muted small">{t::EV_REPEAT} ": " {rule}</p> })}
        {organizer
            .map(|o| {
                view! {
                    <p class="small">
                        <strong>{t::EV_ORGANIZER} ": "</strong>
                        {person(&o)}
                    </p>
                }
            })}
        {(!attendees.is_empty())
            .then(|| {
                view! {
                    <div class="small">
                        <strong>{t::EV_ATTENDEES}</strong>
                        <ul class="ev-people">
                            {attendees
                                .iter()
                                .map(|a| {
                                    let status = a.status.as_deref().map(t::partstat).unwrap_or(t::partstat(""));
                                    view! { <li>{person(a)} " · " <span class="muted">{status}</span></li> }
                                })
                                .collect_view()}
                        </ul>
                    </div>
                }
            })}
        {(!attachments.is_empty())
            .then(|| {
                view! {
                    <div class="small">
                        <strong>{t::EV_ATTACHMENTS}</strong>
                        <ul class="ev-files">
                            {attachments
                                .into_iter()
                                .map(|a| {
                                    let name = a.name.clone().unwrap_or_else(|| a.url.clone());
                                    let size = a.size.map(|s| format!(" ({})", calcraft_ui::format_size(s))).unwrap_or_default();
                                    if a.managed {
                                        let download = name.clone();
                                        view! { <li><a href=a.url.clone() download=download>{name}</a>{size}</li> }.into_any()
                                    } else {
                                        view! { <li><a href=a.url.clone() rel="noopener noreferrer" target="_blank">{name}</a>{size}</li> }.into_any()
                                    }
                                })
                                .collect_view()}
                        </ul>
                    </div>
                }
            })}
    }
}
