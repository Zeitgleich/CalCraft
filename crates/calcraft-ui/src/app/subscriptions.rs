//! Calendar subscriptions (webcal/ICS feeds) on the overview page.
//!
//! The server fetches the feeds; the browser never contacts them (the
//! Content-Security-Policy stays unchanged). Everything is rendered as
//! text. The section is hidden while subscriptions are disabled on the
//! server (the API answers 404).

use calcraft_core::api::{CreateSubscription, SubscriptionInfo};
use calcraft_ui::{
    format_timestamp, text as t,
    validate::{MAX_DISPLAY_NAME, MAX_FEED_URL, check_feed_url, check_length, is_valid_color},
};
use leptos::prelude::*;

use super::{
    Ctx, ErrorText, api,
    collections::{ColorField, DEFAULT_COLOR, Swatch},
    submit,
};

fn path(info: &SubscriptionInfo) -> String {
    format!("/v1/subscriptions/{}", info.slug)
}

/// The subscriptions section; `on_change` reloads the collection lists.
#[component]
pub fn Subscriptions(on_change: Callback<()>) -> impl IntoView {
    let ctx = Ctx::get();
    let items = RwSignal::new(None::<Vec<SubscriptionInfo>>);
    let available = RwSignal::new(true);
    let error = RwSignal::new(None::<String>);
    let reload = Callback::new(move |()| {
        leptos::task::spawn_local(async move {
            match api::get::<Vec<SubscriptionInfo>>("/v1/subscriptions").await {
                Ok(list) => {
                    error.try_set(None);
                    items.try_set(Some(list));
                }
                // Disabled on this server (or not compiled in).
                Err(failure) if failure.status == 404 => {
                    available.try_set(false);
                }
                Err(failure) => {
                    error.try_set(Some(ctx.fail(&failure)));
                }
            }
        });
    });
    reload.run(());
    let changed = Callback::new(move |()| {
        reload.run(());
        on_change.run(());
    });

    view! {
        <Show when=move || available.get()>
            <section>
                <h2>{t::SUBSCRIPTIONS}</h2>
                <p class="hint">{t::SUBSCRIPTIONS_HINT}</p>
                <ErrorText error />
                {move || match items.get() {
                    None => view! { <p class="muted">{t::LOADING}</p> }.into_any(),
                    Some(list) if list.is_empty() => {
                        view! { <p class="muted empty">{t::NO_SUBSCRIPTIONS}</p> }.into_any()
                    }
                    Some(list) => {
                        view! {
                            <ul class="items">
                                {list
                                    .into_iter()
                                    .map(|info| view! { <SubscriptionRow info on_change=changed /> })
                                    .collect_view()}
                            </ul>
                        }
                            .into_any()
                    }
                }}
                <SubscribeForm on_created=changed />
            </section>
        </Show>
    }
}

#[component]
fn SubscriptionRow(info: SubscriptionInfo, on_change: Callback<()>) -> impl IntoView {
    let ctx = Ctx::get();
    let confirming = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let notice = RwSignal::new(None::<&'static str>);

    let title = info
        .display_name
        .clone()
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| info.slug.clone());
    let last = info
        .last_success_at
        .map_or_else(|| t::NEVER.to_owned(), format_timestamp);
    let status = t::subscription_status(&last, info.objects, info.effective_refresh_minutes);
    let feed_error = info.last_error_message.clone();
    let skipped = (info.skipped > 0).then(|| t::subscription_skipped(info.skipped));
    let confirm_text = t::confirm_unsubscribe(&title);

    let refresh_path = format!("{}/refresh", path(&info));
    let refresh = move |_| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        notice.set(None);
        let path = refresh_path.clone();
        leptos::task::spawn_local(async move {
            let result = api::post_for::<SubscriptionInfo>(&path).await;
            busy.try_set(false);
            match result {
                Ok(updated) => {
                    if updated.last_error.is_none() {
                        notice.try_set(Some(t::REFRESH_DONE));
                    }
                    on_change.run(());
                }
                Err(failure) => {
                    error.try_set(Some(ctx.fail(&failure)));
                }
            }
        });
    };

    let delete_path = path(&info);
    let delete = move |_| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        let path = delete_path.clone();
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

    view! {
        <li class="item">
            <div class="item-head">
                <Swatch color=info.color.clone() />
                <div class="item-title">
                    <strong>{title}</strong>
                    <span class="muted">{status}</span>
                </div>
                <div class="actions">
                    <button type="button" class="secondary" on:click=refresh disabled=move || busy.get()>
                        {t::REFRESH_NOW}
                    </button>
                    <button type="button" class="danger" on:click=move |_| confirming.set(true)>
                        {t::UNSUBSCRIBE}
                    </button>
                </div>
            </div>
            <p class="dav-url">
                <span class="muted">{t::FEED_LINK} " "</span>
                <code>{info.url.clone()}</code>
            </p>
            {feed_error.map(|message| view! { <p class="error">{message}</p> })}
            {skipped.map(|message| view! { <p class="muted small">{message}</p> })}
            {move || notice.get().map(|message| view! { <p class="muted small" role="status">{message}</p> })}
            <Show when=move || confirming.get()>
                <div class="confirm" role="alertdialog">
                    <p>{confirm_text.clone()}</p>
                    <button type="button" class="danger" on:click=delete.clone() disabled=move || busy.get()>
                        {t::UNSUBSCRIBE}
                    </button>
                    <button type="button" class="secondary" on:click=move |_| confirming.set(false)>
                        {t::CANCEL}
                    </button>
                </div>
            </Show>
            <ErrorText error />
        </li>
    }
}

#[component]
fn SubscribeForm(on_created: Callback<()>) -> impl IntoView {
    let ctx = Ctx::get();
    let url = RwSignal::new(String::new());
    let display_name = RwSignal::new(String::new());
    let has_color = RwSignal::new(true);
    let color = RwSignal::new(DEFAULT_COLOR.to_owned());
    let refresh = RwSignal::new(0u32);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);

    let on_submit = move |ev| {
        submit(
            ev,
            busy,
            error,
            move || {
                check_feed_url(&url.get_untracked())?;
                check_length(&display_name.get_untracked(), MAX_DISPLAY_NAME)?;
                if has_color.get_untracked() && !is_valid_color(&color.get_untracked()) {
                    return Err(t::ERR_COLOR_INVALID);
                }
                Ok(())
            },
            move || async move {
                let name = display_name.get_untracked().trim().to_owned();
                let request = CreateSubscription {
                    url: url.get_untracked().trim().to_owned(),
                    slug: None,
                    display_name: (!name.is_empty()).then_some(name),
                    color: has_color.get_untracked().then(|| color.get_untracked()),
                    refresh_minutes: Some(refresh.get_untracked()),
                };
                api::send::<_, SubscriptionInfo>("POST", "/v1/subscriptions", &request)
                    .await
                    .map_err(|failure| ctx.fail(&failure))?;
                url.set(String::new());
                display_name.set(String::new());
                on_created.run(());
                Ok(())
            },
        );
    };

    view! {
        <form class="card" on:submit=on_submit>
            <h3>{t::SUBSCRIBE}</h3>
            <label>
                {t::FEED_URL}
                <input
                    type="text"
                    inputmode="url"
                    required
                    autocapitalize="none"
                    spellcheck="false"
                    maxlength=MAX_FEED_URL
                    bind:value=url
                />
            </label>
            <label>
                {t::DISPLAY_NAME}
                <input type="text" maxlength=MAX_DISPLAY_NAME bind:value=display_name />
            </label>
            <ColorField has_color color />
            <label>
                {t::REFRESH_INTERVAL}
                <select on:change=move |ev| {
                    refresh.set(event_target_value(&ev).parse().unwrap_or(0));
                }>
                    {t::REFRESH_CHOICES
                        .iter()
                        .map(|(minutes, label)| {
                            let minutes = *minutes;
                            view! {
                                <option value=minutes.to_string() selected=move || refresh.get() == minutes>
                                    {*label}
                                </option>
                            }
                        })
                        .collect_view()}
                </select>
            </label>
            <ErrorText error />
            <button type="submit" disabled=move || busy.get()>
                {t::SUBSCRIBE_BUTTON}
            </button>
        </form>
    }
}
