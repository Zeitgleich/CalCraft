//! Sharing: who a collection is shared with (owner), what is shared with
//! the user (sharee), and public links of calendars.

use calcraft_core::{
    CollectionKind,
    api::{
        CollectionInfo, CreatePublishLink, CreatedPublishLink, IncomingShareInfo, PrivateEvents,
        PublishLinkInfo, SetShareRequest, ShareAccess, ShareInfo,
    },
};
use calcraft_ui::{text as t, validate::check_username};
use leptos::prelude::*;

use super::{Ctx, ErrorText, api, submit};

fn access_text(access: ShareAccess) -> &'static str {
    match access {
        ShareAccess::Read => t::ACCESS_READ,
        ShareAccess::ReadWrite => t::ACCESS_READ_WRITE,
    }
}

fn origin() -> String {
    window().location().origin().unwrap_or_default()
}

/// The owner's view of a collection's shares (and, for calendars with
/// publishing on, its public links).
#[component]
pub fn SharePanel(info: CollectionInfo, publish: bool) -> impl IntoView {
    let ctx = Ctx::get();
    let base = format!("/v1/collections/{}/{}", info.kind.as_str(), info.slug);
    let shares = RwSignal::new(None::<Vec<ShareInfo>>);
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let username = RwSignal::new(String::new());
    let read_write = RwSignal::new(false);

    let list_path = format!("{base}/shares");
    let reload = Callback::new(move |()| {
        let path = list_path.clone();
        leptos::task::spawn_local(async move {
            match api::get::<Vec<ShareInfo>>(&path).await {
                Ok(list) => {
                    shares.try_set(Some(list));
                }
                Err(failure) => {
                    error.try_set(Some(ctx.fail(&failure)));
                }
            }
        });
    });
    reload.run(());

    let add_base = base.clone();
    let on_add = move |ev| {
        let base = add_base.clone();
        submit(
            ev,
            busy,
            error,
            move || {
                if username.get_untracked().trim().is_empty() {
                    return Err(t::ERR_SHARE_USER);
                }
                check_username(username.get_untracked().trim())
            },
            move || async move {
                let request = SetShareRequest {
                    access: if read_write.get_untracked() {
                        ShareAccess::ReadWrite
                    } else {
                        ShareAccess::Read
                    },
                };
                let path = format!("{base}/shares/{}", username.get_untracked().trim());
                api::send::<_, ShareInfo>("PUT", &path, &request)
                    .await
                    .map_err(|failure| ctx.fail(&failure))?;
                username.set(String::new());
                reload.run(());
                Ok(())
            },
        );
    };

    let remove_base = base.clone();
    let remove = move |name: String| {
        let path = format!("{remove_base}/shares/{name}");
        leptos::task::spawn_local(async move {
            match api::send_empty("DELETE", &path).await {
                Ok(()) => reload.run(()),
                Err(failure) => {
                    error.try_set(Some(ctx.fail(&failure)));
                }
            }
        });
    };
    // Subscriptions can only be shared read-only.
    let read_only = info.read_only;
    let calendar = info.kind == CollectionKind::Calendar;

    view! {
        <div class="share-panel">
            <h3>{t::SHARING_TITLE}</h3>
            <p class="hint">{t::SHARING_HINT}</p>
            {move || match shares.get() {
                None => view! { <p class="muted">{t::LOADING}</p> }.into_any(),
                Some(list) if list.is_empty() => {
                    view! { <p class="muted">{t::SHARING_NOBODY}</p> }.into_any()
                }
                Some(list) => {
                    let remove = remove.clone();
                    view! {
                        <ul class="items">
                            {list
                                .into_iter()
                                .map(|share| {
                                    let remove = remove.clone();
                                    let name = share.username.clone();
                                    let state = if share.accepted {
                                        t::SHARE_ACCEPTED
                                    } else {
                                        t::SHARE_PENDING
                                    };
                                    let mut line = format!(
                                        "{} · {} · {state}",
                                        share.username,
                                        access_text(share.access),
                                    );
                                    if !share.groups.is_empty() {
                                        line.push_str(&format!(
                                            " · {} {}",
                                            t::VIA_GROUP,
                                            share.groups.join(", ")
                                        ));
                                    }
                                    // Only a direct share can be removed
                                    // here; group shares below.
                                    let direct = share.direct.is_some();
                                    view! {
                                        <li class="item-head">
                                            <span>{line}</span>
                                            {direct
                                                .then(|| {
                                                    view! {
                                                        <button
                                                            type="button"
                                                            class="secondary"
                                                            on:click=move |_| remove(name.clone())
                                                        >
                                                            {t::SHARE_REMOVE}
                                                        </button>
                                                    }
                                                })}
                                        </li>
                                    }
                                })
                                .collect_view()}
                        </ul>
                    }
                        .into_any()
                }
            }}
            <form class="edit" on:submit=on_add>
                <label>
                    {t::SHARE_WITH_USER}
                    <input
                        type="text"
                        autocapitalize="none"
                        spellcheck="false"
                        maxlength="64"
                        bind:value=username
                    />
                </label>
                {(!read_only)
                    .then(|| {
                        view! {
                            <label class="inline">
                                <input type="checkbox" bind:checked=read_write />
                                {t::ACCESS_READ_WRITE}
                            </label>
                        }
                    })}
                <button type="submit" disabled=move || busy.get()>
                    {t::SHARE_ADD}
                </button>
            </form>
            <ErrorText error />
            <super::groups::GroupSharePanel base=base.clone() read_only on_change=reload />
            {(calendar && publish).then(|| view! { <PublishPanel base=base.clone() /> })}
        </div>
    }
}

fn private_text(value: PrivateEvents) -> &'static str {
    match value {
        PrivateEvents::Busy => t::PUBLISH_PRIVATE_BUSY,
        PrivateEvents::Hide => t::PUBLISH_PRIVATE_HIDE,
        PrivateEvents::Show => t::PUBLISH_PRIVATE_SHOW,
    }
}

/// Public links of a calendar.
#[component]
fn PublishPanel(base: String) -> impl IntoView {
    let ctx = Ctx::get();
    let links = RwSignal::new(None::<Vec<PublishLinkInfo>>);
    let created = RwSignal::new(None::<CreatedPublishLink>);
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let busy_only = RwSignal::new(false);
    let private = RwSignal::new(PrivateEvents::Busy);
    let attendees = RwSignal::new(false);
    let path = format!("{base}/publish");

    let list_path = path.clone();
    let reload = Callback::new(move |()| {
        let path = list_path.clone();
        leptos::task::spawn_local(async move {
            match api::get::<Vec<PublishLinkInfo>>(&path).await {
                Ok(list) => {
                    links.try_set(Some(list));
                }
                Err(failure) => {
                    error.try_set(Some(ctx.fail(&failure)));
                }
            }
        });
    });
    reload.run(());

    let create_path = path.clone();
    let on_create = move |ev| {
        let path = create_path.clone();
        submit(
            ev,
            busy,
            error,
            || Ok(()),
            move || async move {
                let request = CreatePublishLink {
                    busy_only: busy_only.get_untracked(),
                    private_events: private.get_untracked(),
                    include_attendees: attendees.get_untracked(),
                };
                let link = api::send::<_, CreatedPublishLink>("POST", &path, &request)
                    .await
                    .map_err(|failure| ctx.fail(&failure))?;
                created.set(Some(link));
                reload.run(());
                Ok(())
            },
        );
    };
    let revoke = move |id: String| {
        let path = format!("{path}/{id}");
        leptos::task::spawn_local(async move {
            match api::send_empty("DELETE", &path).await {
                Ok(()) => {
                    created.try_set(None);
                    reload.run(());
                }
                Err(failure) => {
                    error.try_set(Some(ctx.fail(&failure)));
                }
            }
        });
    };

    view! {
        <h3>{t::PUBLISH_TITLE}</h3>
        <p class="hint">{t::PUBLISH_HINT}</p>
        {move || {
            created
                .get()
                .map(|link| {
                    view! {
                        <div class="success" role="status">
                            <p>{t::PUBLISH_CREATED}</p>
                            <p><code>{link.url}</code></p>
                            <p><code>{link.webcal_url}</code></p>
                        </div>
                    }
                })
        }}
        {move || match links.get() {
            None => view! { <p class="muted">{t::LOADING}</p> }.into_any(),
            Some(list) if list.is_empty() => {
                view! { <p class="muted">{t::PUBLISH_NONE}</p> }.into_any()
            }
            Some(list) => {
                let revoke = revoke.clone();
                view! {
                    <ul class="items">
                        {list
                            .into_iter()
                            .map(|link| {
                                let revoke = revoke.clone();
                                let id = link.id.clone();
                                let line = t::publish_options(
                                    link.busy_only,
                                    private_text(link.private_events),
                                    link.include_attendees,
                                );
                                view! {
                                    <li class="item-head">
                                        <span>{line}</span>
                                        <button
                                            type="button"
                                            class="danger"
                                            on:click=move |_| revoke(id.clone())
                                        >
                                            {t::PUBLISH_REVOKE}
                                        </button>
                                    </li>
                                }
                            })
                            .collect_view()}
                    </ul>
                }
                    .into_any()
            }
        }}
        <form class="edit" on:submit=on_create>
            <label class="inline">
                <input type="checkbox" bind:checked=busy_only />
                {t::PUBLISH_BUSY_ONLY}
            </label>
            <label>
                {t::PUBLISH_PRIVATE}
                <select on:change=move |ev| {
                    private.set(match event_target_value(&ev).as_str() {
                        "hide" => PrivateEvents::Hide,
                        "show" => PrivateEvents::Show,
                        _ => PrivateEvents::Busy,
                    });
                }>
                    <option value="busy" selected=true>{t::PUBLISH_PRIVATE_BUSY}</option>
                    <option value="hide">{t::PUBLISH_PRIVATE_HIDE}</option>
                    <option value="show">{t::PUBLISH_PRIVATE_SHOW}</option>
                </select>
            </label>
            <label class="inline">
                <input type="checkbox" bind:checked=attendees />
                {t::PUBLISH_ATTENDEES}
            </label>
            <button type="submit" disabled=move || busy.get()>
                {t::PUBLISH_CREATE}
            </button>
        </form>
        <ErrorText error />
    }
}

/// Collections other users shared with the user: accept, decline, leave.
#[component]
pub fn SharedWithMe() -> impl IntoView {
    let ctx = Ctx::get();
    let shares = RwSignal::new(None::<Vec<IncomingShareInfo>>);
    let error = RwSignal::new(None::<String>);
    let reload = Callback::new(move |()| {
        leptos::task::spawn_local(async move {
            match api::get::<Vec<IncomingShareInfo>>("/v1/shared").await {
                Ok(list) => {
                    shares.try_set(Some(list));
                }
                Err(failure) => {
                    error.try_set(Some(ctx.fail(&failure)));
                }
            }
        });
    });
    reload.run(());
    let act = move |method: &'static str, path: String| {
        leptos::task::spawn_local(async move {
            let result = if method == "POST" {
                api::post_for::<IncomingShareInfo>(&path).await.map(|_| ())
            } else {
                api::send_empty(method, &path).await
            };
            match result {
                Ok(()) => reload.run(()),
                Err(failure) => {
                    error.try_set(Some(ctx.fail(&failure)));
                }
            }
        });
    };

    view! {
        <section>
            <h2>{t::SHARED_WITH_ME}</h2>
            <p class="hint">{t::SHARED_WITH_ME_HINT}</p>
            {move || match shares.get() {
                None => view! { <p class="muted">{t::LOADING}</p> }.into_any(),
                Some(list) if list.is_empty() => view! { <p class="muted empty">"—"</p> }.into_any(),
                Some(list) => {
                    view! {
                        <ul class="items">
                            {list
                                .into_iter()
                                .map(|share| {
                                    let title = share
                                        .display_name
                                        .clone()
                                        .filter(|n| !n.is_empty())
                                        .unwrap_or_else(|| share.owner.clone());
                                    let owner = share
                                        .owner_display_name
                                        .clone()
                                        .unwrap_or_else(|| share.owner.clone());
                                    let kind = match share.kind {
                                        CollectionKind::Calendar => t::CALENDAR,
                                        CollectionKind::Addressbook => t::ADDRESS_BOOK,
                                    };
                                    let mut line = format!(
                                        "{kind} · {} {owner} · {}",
                                        t::SHARED_BY,
                                        access_text(share.access),
                                    );
                                    if !share.groups.is_empty() {
                                        line.push_str(&format!(
                                            " · {} {}",
                                            t::VIA_GROUP,
                                            share.groups.join(", ")
                                        ));
                                    }
                                    let id = share.id.clone();
                                    let accept = format!("/v1/shared/{id}/accept");
                                    let leave = format!("/v1/shared/{id}");
                                    let url = share.href.clone().map(|href| format!("{}{href}", origin()));
                                    let accepted = share.accepted;
                                    view! {
                                        <li class="item">
                                            <div class="item-head">
                                                <div class="item-title">
                                                    <strong>{title}</strong>
                                                    <span class="muted">{line}</span>
                                                </div>
                                                <div class="actions">
                                                    {(!accepted)
                                                        .then(|| {
                                                            let accept = accept.clone();
                                                            view! {
                                                                <button type="button" on:click=move |_| act("POST", accept.clone())>
                                                                    {t::ACCEPT}
                                                                </button>
                                                            }
                                                        })}
                                                    <button
                                                        type="button"
                                                        class="secondary"
                                                        on:click=move |_| act("DELETE", leave.clone())
                                                    >
                                                        {if accepted { t::LEAVE } else { t::DECLINE }}
                                                    </button>
                                                </div>
                                            </div>
                                            {url
                                                .map(|url| {
                                                    view! {
                                                        <p class="dav-url">
                                                            <span class="muted">{t::DAV_URL} " "</span>
                                                            <code>{url}</code>
                                                        </p>
                                                    }
                                                })}
                                        </li>
                                    }
                                })
                                .collect_view()}
                        </ul>
                    }
                        .into_any()
                }
            }}
            <ErrorText error />
        </section>
    }
}
