//! Overview: calendars and address books.

use calcraft_core::{
    CollectionKind,
    api::{CollectionInfo, CreateCollection, UpdateCollection},
};
use calcraft_ui::{
    text as t,
    validate::{
        MAX_DESCRIPTION, MAX_DISPLAY_NAME, check_length, check_slug, color_for_picker,
        is_valid_color, suggest_slug,
    },
};
use leptos::prelude::*;

use super::{
    Ctx, ErrorText, api,
    sharing::{SharePanel, SharedWithMe},
    submit,
    subscriptions::Subscriptions,
};

/// Default color for new calendars.
pub(super) const DEFAULT_COLOR: &str = "#3a7bd5";

fn origin() -> String {
    window().location().origin().unwrap_or_default()
}

fn collection_path(info: &CollectionInfo) -> String {
    format!("/v1/collections/{}/{}", info.kind.as_str(), info.slug)
}

#[component]
pub fn Overview() -> impl IntoView {
    let ctx = Ctx::get();
    let items = RwSignal::new(None::<Vec<CollectionInfo>>);
    let error = RwSignal::new(None::<String>);
    let reload = Callback::new(move |()| {
        leptos::task::spawn_local(async move {
            match api::get::<Vec<CollectionInfo>>("/v1/collections").await {
                Ok(list) => {
                    error.try_set(None);
                    items.try_set(Some(list));
                }
                Err(failure) => {
                    error.try_set(Some(ctx.fail(&failure)));
                }
            }
        });
    });
    reload.run(());

    let section = move |kind: CollectionKind, title: &'static str, empty: &'static str| {
        view! {
            <section>
                <h2>{title}</h2>
                {move || {
                    let list: Vec<CollectionInfo> = items
                        .get()
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|c| c.kind == kind)
                        .collect();
                    if items.get().is_none() {
                        view! { <p class="muted">{t::LOADING}</p> }.into_any()
                    } else if list.is_empty() {
                        view! { <p class="muted empty">{empty}</p> }.into_any()
                    } else {
                        view! {
                            <ul class="items">
                                {list
                                    .into_iter()
                                    .map(|info| view! { <CollectionRow info on_change=reload /> })
                                    .collect_view()}
                            </ul>
                        }
                            .into_any()
                    }
                }}
            </section>
        }
    };

    view! {
        <h1>{t::OVERVIEW_TITLE}</h1>
        <p class="hint">
            {t::DAV_SETUP_HINT} " " <code>{format!("{}/", origin())}</code>
        </p>
        <ErrorText error />
        {section(CollectionKind::Calendar, t::CALENDARS, t::NO_CALENDARS)}
        <Subscriptions on_change=reload />
        {section(CollectionKind::Addressbook, t::ADDRESS_BOOKS, t::NO_ADDRESS_BOOKS)}
        {move || ctx.me().is_some_and(|me| me.sharing).then(|| view! { <SharedWithMe /> })}
        <CreateForm on_created=reload />
    }
}

/// Color accent along the left edge of the card, drawn as SVG so that no
/// `style` attribute is needed (the shape comes from the stylesheet).
#[component]
pub(super) fn Swatch(color: Option<String>) -> impl IntoView {
    let color = color.filter(|c| is_valid_color(c));
    let class = if color.is_some() {
        "swatch"
    } else {
        "swatch none"
    };
    view! {
        <svg class=class viewBox="0 0 1 1" preserveAspectRatio="none" aria-hidden="true">
            <rect width="1" height="1" fill=color.unwrap_or_else(|| "none".to_owned()) />
        </svg>
    }
}

#[component]
fn CollectionRow(info: CollectionInfo, on_change: Callback<()>) -> impl IntoView {
    let ctx = Ctx::get();
    let editing = RwSignal::new(false);
    let confirming = RwSignal::new(false);
    let sharing = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let share_info = info.clone();
    let can_share = ctx.me().is_some_and(|me| me.sharing);
    let can_publish = ctx.me().is_some_and(|me| me.publish);

    let path = collection_path(&info);
    let title = info
        .display_name
        .clone()
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| info.slug.clone());
    let kind_label = match info.kind {
        CollectionKind::Calendar => t::CALENDAR,
        CollectionKind::Addressbook => t::ADDRESS_BOOK,
    };
    let url = format!("{}{}", origin(), info.href);
    let components = info.components.join(", ");
    // Subscriptions: name and color only; the content comes from the feed.
    let read_only = info.read_only;

    // Edit form state, initialised from the current values.
    let display_name = RwSignal::new(info.display_name.clone().unwrap_or_default());
    let description = RwSignal::new(info.description.clone().unwrap_or_default());
    let picker = color_for_picker(info.color.as_deref());
    let has_color = RwSignal::new(picker.is_some());
    let color = RwSignal::new(picker.unwrap_or_else(|| DEFAULT_COLOR.to_owned()));

    let save_path = path.clone();
    let on_save = move |ev| {
        let path = save_path.clone();
        submit(
            ev,
            busy,
            error,
            move || {
                check_length(&display_name.get_untracked(), MAX_DISPLAY_NAME)?;
                check_length(&description.get_untracked(), MAX_DESCRIPTION)?;
                if has_color.get_untracked() && !is_valid_color(&color.get_untracked()) {
                    return Err(t::ERR_COLOR_INVALID);
                }
                Ok(())
            },
            move || async move {
                let update = UpdateCollection {
                    display_name: Some(display_name.get_untracked().trim().to_owned()),
                    description: (!read_only)
                        .then(|| description.get_untracked().trim().to_owned()),
                    color: Some(if has_color.get_untracked() {
                        color.get_untracked()
                    } else {
                        String::new()
                    }),
                };
                api::send_no_content("PATCH", &path, &update)
                    .await
                    .map_err(|failure| ctx.fail(&failure))?;
                on_change.run(());
                Ok(())
            },
        );
    };

    let delete = move |_| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        let path = path.clone();
        leptos::task::spawn_local(async move {
            let result = api::send_empty("DELETE", &path).await;
            busy.try_set(false);
            match result {
                Ok(()) => on_change.run(()),
                Err(failure) => {
                    error.try_set(Some(ctx.fail(&failure)));
                }
            }
        });
    };

    let kind_label = if read_only {
        t::READ_ONLY_SUBSCRIPTION
    } else {
        kind_label
    };
    let slug_line = format!("{kind_label} · {}", info.slug);
    let description_text = info.description.clone().unwrap_or_default();
    let confirm_text = if read_only {
        t::confirm_unsubscribe(&title)
    } else {
        t::confirm_delete_collection(&title)
    };

    view! {
        <li class="item">
            <div class="item-head">
                <Swatch color=info.color.clone() />
                <div class="item-title">
                    <strong>{title}</strong>
                    <span class="muted">{slug_line}</span>
                </div>
                <div class="actions">
                    {can_share
                        .then(|| {
                            view! {
                                <button
                                    type="button"
                                    class="secondary"
                                    on:click=move |_| sharing.update(|s| *s = !*s)
                                >
                                    {t::SHARE}
                                </button>
                            }
                        })}
                    <button
                        type="button"
                        class="secondary"
                        on:click=move |_| {
                            editing.update(|e| *e = !*e);
                            confirming.set(false);
                        }
                    >
                        {move || if editing.get() { t::CANCEL } else { t::EDIT }}
                    </button>
                    <button
                        type="button"
                        class="danger"
                        on:click=move |_| {
                            confirming.set(true);
                            editing.set(false);
                        }
                    >
                        {t::DELETE}
                    </button>
                </div>
            </div>
            {(!description_text.is_empty()).then(|| view! { <p class="description">{description_text}</p> })}
            <p class="dav-url">
                <span class="muted">{t::DAV_URL} " "</span>
                <code>{url}</code>
            </p>
            {(!components.is_empty())
                .then(|| view! { <p class="muted small">{t::COMPONENTS} " " {components}</p> })}
            <Show when=move || confirming.get()>
                <div class="confirm" role="alertdialog">
                    <p>{confirm_text.clone()}</p>
                    <button type="button" class="danger" on:click=delete.clone() disabled=move || busy.get()>
                        {t::DELETE_CONFIRM}
                    </button>
                    <button type="button" class="secondary" on:click=move |_| confirming.set(false)>
                        {t::CANCEL}
                    </button>
                </div>
            </Show>
            <Show when=move || sharing.get()>
                <SharePanel info=share_info.clone() publish=can_publish />
            </Show>
            <Show when=move || editing.get()>
                <form class="edit" on:submit=on_save.clone()>
                    <label>
                        {t::DISPLAY_NAME}
                        <input type="text" maxlength=MAX_DISPLAY_NAME bind:value=display_name />
                    </label>
                    {(!read_only)
                        .then(|| {
                            view! {
                                <label>
                                    {t::DESCRIPTION}
                                    <textarea rows="2" maxlength=MAX_DESCRIPTION bind:value=description></textarea>
                                </label>
                            }
                        })}
                    <ColorField has_color color />
                    <button type="submit" disabled=move || busy.get()>
                        {t::SAVE}
                    </button>
                </form>
            </Show>
            <ErrorText error />
        </li>
    }
}

/// Color picker with a "no color" option.
#[component]
pub(super) fn ColorField(has_color: RwSignal<bool>, color: RwSignal<String>) -> impl IntoView {
    view! {
        <fieldset class="color-field">
            <legend>{t::COLOR}</legend>
            <label class="inline">
                <input type="checkbox" bind:checked=has_color />
                {t::USE_COLOR}
            </label>
            <input
                type="color"
                aria-label=t::COLOR
                disabled=move || !has_color.get()
                bind:value=color
            />
        </fieldset>
    }
}

#[component]
fn CreateForm(on_created: Callback<()>) -> impl IntoView {
    let ctx = Ctx::get();
    let kind = RwSignal::new(CollectionKind::Calendar);
    let slug = RwSignal::new(String::new());
    let slug_edited = RwSignal::new(false);
    let display_name = RwSignal::new(String::new());
    let description = RwSignal::new(String::new());
    let has_color = RwSignal::new(true);
    let color = RwSignal::new(DEFAULT_COLOR.to_owned());
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);

    let on_submit = move |ev| {
        submit(
            ev,
            busy,
            error,
            move || {
                check_slug(&slug.get_untracked())?;
                check_length(&display_name.get_untracked(), MAX_DISPLAY_NAME)?;
                check_length(&description.get_untracked(), MAX_DESCRIPTION)?;
                if has_color.get_untracked() && !is_valid_color(&color.get_untracked()) {
                    return Err(t::ERR_COLOR_INVALID);
                }
                Ok(())
            },
            move || async move {
                let non_empty = |s: String| {
                    let s = s.trim().to_owned();
                    (!s.is_empty()).then_some(s)
                };
                let request = CreateCollection {
                    kind: kind.get_untracked(),
                    slug: slug.get_untracked(),
                    display_name: non_empty(display_name.get_untracked()),
                    description: non_empty(description.get_untracked()),
                    color: has_color.get_untracked().then(|| color.get_untracked()),
                };
                api::send::<_, CollectionInfo>("POST", "/v1/collections", &request)
                    .await
                    .map_err(|failure| ctx.fail(&failure))?;
                slug.set(String::new());
                slug_edited.set(false);
                display_name.set(String::new());
                description.set(String::new());
                on_created.run(());
                Ok(())
            },
        );
    };

    view! {
        <section>
            <h2>{t::CREATE_COLLECTION}</h2>
            <form class="card" on:submit=on_submit>
                <label>
                    {t::KIND}
                    <select on:change=move |ev| {
                        kind.set(
                            if event_target_value(&ev) == "addressbook" {
                                CollectionKind::Addressbook
                            } else {
                                CollectionKind::Calendar
                            },
                        );
                    }>
                        <option value="calendar" selected=move || kind.get() == CollectionKind::Calendar>
                            {t::CALENDAR}
                        </option>
                        <option value="addressbook" selected=move || kind.get() == CollectionKind::Addressbook>
                            {t::ADDRESS_BOOK}
                        </option>
                    </select>
                </label>
                <label>
                    {t::DISPLAY_NAME}
                    <input
                        type="text"
                        maxlength=MAX_DISPLAY_NAME
                        prop:value=move || display_name.get()
                        on:input=move |ev| {
                            let value = event_target_value(&ev);
                            if !slug_edited.get_untracked() {
                                slug.set(suggest_slug(&value));
                            }
                            display_name.set(value);
                        }
                    />
                </label>
                <label>
                    {t::SLUG}
                    <input
                        type="text"
                        required
                        autocapitalize="none"
                        spellcheck="false"
                        maxlength="64"
                        prop:value=move || slug.get()
                        on:input=move |ev| {
                            slug_edited.set(true);
                            slug.set(event_target_value(&ev));
                        }
                    />
                    <span class="hint">{t::SLUG_HINT}</span>
                </label>
                <label>
                    {t::DESCRIPTION}
                    <textarea rows="2" maxlength=MAX_DESCRIPTION bind:value=description></textarea>
                </label>
                <ColorField has_color color />
                <ErrorText error />
                <button type="submit" disabled=move || busy.get()>
                    {t::CREATE}
                </button>
            </form>
        </section>
    }
}
