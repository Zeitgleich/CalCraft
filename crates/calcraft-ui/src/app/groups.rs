//! Groups: administration (administrators only; the server checks again),
//! the user's own groups, and sharing a collection with a group.

use calcraft_core::api::{
    CreateGroup, GroupInfo, GroupShareInfo, GroupShareResult, MemberAddedInfo, SetShareRequest,
    ShareAccess,
};
use calcraft_ui::{
    text as t,
    validate::{MAX_DISPLAY_NAME, check_group_name, check_length, check_username},
};
use leptos::prelude::*;

use super::{Ctx, ErrorText, SuccessText, api, submit};

fn access_text(access: ShareAccess) -> &'static str {
    match access {
        ShareAccess::Read => t::ACCESS_READ,
        ShareAccess::ReadWrite => t::ACCESS_READ_WRITE,
    }
}

/// The groups of the server with their members, for administrators.
#[component]
pub fn AdminGroups() -> impl IntoView {
    let ctx = Ctx::get();
    let groups = RwSignal::new(None::<Vec<GroupInfo>>);
    let error = RwSignal::new(None::<String>);
    let reload = Callback::new(move |()| {
        leptos::task::spawn_local(async move {
            match api::get::<Vec<GroupInfo>>("/v1/admin/groups").await {
                Ok(list) => {
                    groups.try_set(Some(list));
                }
                Err(failure) => {
                    error.try_set(Some(ctx.fail(&failure)));
                }
            }
        });
    });
    reload.run(());

    let name = RwSignal::new(String::new());
    let display_name = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let on_create = move |ev| {
        submit(
            ev,
            busy,
            error,
            move || {
                check_group_name(name.get_untracked().trim())?;
                check_length(&display_name.get_untracked(), MAX_DISPLAY_NAME)
            },
            move || async move {
                let display = display_name.get_untracked().trim().to_owned();
                let request = CreateGroup {
                    name: name.get_untracked().trim().to_owned(),
                    display_name: (!display.is_empty()).then_some(display),
                };
                api::send::<_, GroupInfo>("POST", "/v1/admin/groups", &request)
                    .await
                    .map_err(|failure| ctx.fail(&failure))?;
                name.set(String::new());
                display_name.set(String::new());
                reload.run(());
                Ok(())
            },
        );
    };

    view! {
        <section>
            <h2>{t::GROUPS}</h2>
            <p class="hint">{t::GROUPS_HINT}</p>
            {move || match groups.get() {
                None => view! { <p class="muted">{t::LOADING}</p> }.into_any(),
                Some(list) if list.is_empty() => {
                    view! { <p class="muted">{t::GROUPS_NONE}</p> }.into_any()
                }
                Some(list) => {
                    view! {
                        <ul class="items">
                            {list
                                .into_iter()
                                .map(|group| view! { <AdminGroup group on_change=reload /> })
                                .collect_view()}
                        </ul>
                    }
                        .into_any()
                }
            }}
            <form class="edit" on:submit=on_create>
                <h3>{t::GROUP_CREATE}</h3>
                <label>
                    {t::GROUP_NAME}
                    <input
                        type="text"
                        required
                        autocomplete="off"
                        autocapitalize="none"
                        spellcheck="false"
                        maxlength="64"
                        bind:value=name
                    />
                </label>
                <label>
                    {t::DISPLAY_NAME}
                    <input
                        type="text"
                        autocomplete="off"
                        maxlength=MAX_DISPLAY_NAME
                        bind:value=display_name
                    />
                </label>
                <button type="submit" disabled=move || busy.get()>
                    {t::CREATE}
                </button>
            </form>
            <ErrorText error />
        </section>
    }
}

/// One group: its members (loaded when opened), adding and removing
/// members, deleting the group.
#[component]
fn AdminGroup(group: GroupInfo, on_change: Callback<()>) -> impl IntoView {
    let ctx = Ctx::get();
    let base = format!("/v1/admin/groups/{}", group.name);
    let details = RwSignal::new(None::<GroupInfo>);
    let open = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let success = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let confirming = RwSignal::new(false);
    let username = RwSignal::new(String::new());

    let load_path = base.clone();
    let load = Callback::new(move |()| {
        let path = load_path.clone();
        leptos::task::spawn_local(async move {
            match api::get::<GroupInfo>(&path).await {
                Ok(info) => {
                    details.try_set(Some(info));
                }
                Err(failure) => {
                    error.try_set(Some(ctx.fail(&failure)));
                }
            }
        });
    });
    let toggle = move |_| {
        let now_open = !open.get_untracked();
        open.set(now_open);
        if now_open {
            load.run(());
        }
    };

    let add_base = base.clone();
    let on_add = move |ev| {
        let base = add_base.clone();
        success.set(None);
        submit(
            ev,
            busy,
            error,
            move || check_username(username.get_untracked().trim()),
            move || async move {
                let path = format!("{base}/members/{}", username.get_untracked().trim());
                let added = api::send::<_, MemberAddedInfo>("PUT", &path, &())
                    .await
                    .map_err(|failure| ctx.fail(&failure))?;
                username.set(String::new());
                success.set(Some(t::group_shared(added.invited, 0)));
                load.run(());
                on_change.run(());
                Ok(())
            },
        );
    };
    let remove_base = base.clone();
    let remove = move |member: String| {
        let path = format!("{remove_base}/members/{member}");
        leptos::task::spawn_local(async move {
            match api::send_empty("DELETE", &path).await {
                Ok(()) => {
                    load.run(());
                    on_change.run(());
                }
                Err(failure) => {
                    error.try_set(Some(ctx.fail(&failure)));
                }
            }
        });
    };
    let delete_path = base.clone();
    let delete = move |_| {
        let path = delete_path.clone();
        leptos::task::spawn_local(async move {
            match api::send_empty("DELETE", &path).await {
                Ok(()) => on_change.run(()),
                Err(failure) => {
                    confirming.try_set(false);
                    error.try_set(Some(ctx.fail(&failure)));
                }
            }
        });
    };
    let title = group
        .display_name
        .clone()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| group.name.clone());
    let line = format!("{} · {}", group.name, t::members(group.member_count));

    view! {
        <li class="item">
            <div class="item-head">
                <div class="item-title">
                    <strong>{title}</strong>
                    <span class="muted">{line}</span>
                </div>
                <div class="actions">
                    <button type="button" class="secondary" on:click=toggle>
                        {t::GROUP_MEMBERS}
                    </button>
                    <Show
                        when=move || confirming.get()
                        fallback=move || {
                            view! {
                                <button
                                    type="button"
                                    class="secondary"
                                    on:click=move |_| confirming.set(true)
                                >
                                    {t::GROUP_DELETE}
                                </button>
                            }
                        }
                    >
                        <span class="confirm-inline">
                            <span>{t::GROUP_DELETE_CONFIRM}</span>
                            <button type="button" class="danger" on:click=delete.clone()>
                                {t::DELETE}
                            </button>
                            <button
                                type="button"
                                class="secondary"
                                on:click=move |_| confirming.set(false)
                            >
                                {t::CANCEL}
                            </button>
                        </span>
                    </Show>
                </div>
            </div>
            <Show when=move || open.get()>
                {
                    let remove = remove.clone();
                    move || match details.get() {
                        None => view! { <p class="muted">{t::LOADING}</p> }.into_any(),
                        Some(info) => {
                            let remove = remove.clone();
                            view! {
                                <ul class="items">
                                    {info
                                        .members
                                        .into_iter()
                                        .map(|member| {
                                            let remove = remove.clone();
                                            let name = member.username.clone();
                                            let mut line = member.username.clone();
                                            if let Some(display) = member.display_name {
                                                line.push_str(&format!(" · {display}"));
                                            }
                                            if member.disabled {
                                                line.push_str(&format!(" · {}", t::DISABLED));
                                            }
                                            view! {
                                                <li class="item-head">
                                                    <span>{line}</span>
                                                    <button
                                                        type="button"
                                                        class="secondary"
                                                        on:click=move |_| remove(name.clone())
                                                    >
                                                        {t::GROUP_REMOVE_MEMBER}
                                                    </button>
                                                </li>
                                            }
                                        })
                                        .collect_view()}
                                </ul>
                            }
                                .into_any()
                        }
                    }
                }
                <form class="edit" on:submit=on_add.clone()>
                    <label>
                        {t::USERNAME}
                        <input
                            type="text"
                            autocomplete="off"
                            autocapitalize="none"
                            spellcheck="false"
                            maxlength="64"
                            bind:value=username
                        />
                    </label>
                    <button type="submit" disabled=move || busy.get()>
                        {t::GROUP_ADD_MEMBER}
                    </button>
                </form>
            </Show>
            <SuccessText message=success />
            <ErrorText error />
        </li>
    }
}

/// The groups the user is a member of, with their members.
#[component]
pub fn MyGroups() -> impl IntoView {
    let groups = RwSignal::new(None::<Vec<GroupInfo>>);
    leptos::task::spawn_local(async move {
        // Not essential: nothing is shown on errors.
        if let Ok(list) = api::get::<Vec<GroupInfo>>("/v1/me/groups").await {
            groups.try_set(Some(list));
        }
    });
    move || {
        groups.get().filter(|list| !list.is_empty()).map(|list| {
            view! {
                <section>
                    <h2>{t::MY_GROUPS}</h2>
                    <p class="hint">{t::MY_GROUPS_HINT}</p>
                    <dl class="facts">
                        {list
                            .into_iter()
                            .map(|group| {
                                let title = group
                                    .display_name
                                    .clone()
                                    .filter(|name| !name.is_empty())
                                    .unwrap_or_else(|| group.name.clone());
                                let members: Vec<String> = group
                                    .members
                                    .into_iter()
                                    .map(|m| m.display_name.unwrap_or(m.username))
                                    .collect();
                                view! {
                                    <dt>{title}</dt>
                                    <dd>{members.join(", ")}</dd>
                                }
                            })
                            .collect_view()}
                    </dl>
                </section>
            }
        })
    }
}

/// The owner's group shares of a collection: list, add, remove.
#[component]
pub fn GroupSharePanel(base: String, read_only: bool, on_change: Callback<()>) -> impl IntoView {
    let ctx = Ctx::get();
    let shares = RwSignal::new(None::<Vec<GroupShareInfo>>);
    let error = RwSignal::new(None::<String>);
    let success = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let group = RwSignal::new(String::new());
    let read_write = RwSignal::new(false);
    // The user's groups, as suggestions.
    let mine = RwSignal::new(Vec::<String>::new());
    leptos::task::spawn_local(async move {
        if let Ok(list) = api::get::<Vec<GroupInfo>>("/v1/me/groups").await {
            mine.try_set(list.into_iter().map(|g| g.name).collect());
        }
    });
    let list_path = format!("{base}/group-shares");
    let reload = Callback::new(move |()| {
        let path = list_path.clone();
        leptos::task::spawn_local(async move {
            match api::get::<Vec<GroupShareInfo>>(&path).await {
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
        success.set(None);
        submit(
            ev,
            busy,
            error,
            move || {
                if group.get_untracked().trim().is_empty() {
                    return Err(t::ERR_SHARE_GROUP);
                }
                check_group_name(group.get_untracked().trim())
            },
            move || async move {
                let request = SetShareRequest {
                    access: if read_write.get_untracked() {
                        ShareAccess::ReadWrite
                    } else {
                        ShareAccess::Read
                    },
                };
                let path = format!("{base}/group-shares/{}", group.get_untracked().trim());
                let result = api::send::<_, GroupShareResult>("PUT", &path, &request)
                    .await
                    .map_err(|failure| ctx.fail(&failure))?;
                group.set(String::new());
                success.set(Some(t::group_shared(result.invited, result.skipped)));
                reload.run(());
                on_change.run(());
                Ok(())
            },
        );
    };
    let datalist = format!("groups-{}", base.replace('/', "-"));
    let remove = move |name: String| {
        let path = format!("{base}/group-shares/{name}");
        leptos::task::spawn_local(async move {
            match api::send_empty("DELETE", &path).await {
                Ok(()) => {
                    reload.run(());
                    on_change.run(());
                }
                Err(failure) => {
                    error.try_set(Some(ctx.fail(&failure)));
                }
            }
        });
    };
    let list_id = datalist.clone();

    view! {
        <h3>{t::GROUP_SHARES}</h3>
        <p class="hint">{t::GROUP_SHARE_HINT}</p>
        {move || match shares.get() {
            None => view! { <p class="muted">{t::LOADING}</p> }.into_any(),
            Some(list) if list.is_empty() => view! { <p class="muted">"—"</p> }.into_any(),
            Some(list) => {
                let remove = remove.clone();
                view! {
                    <ul class="items">
                        {list
                            .into_iter()
                            .map(|share| {
                                let remove = remove.clone();
                                let name = share.group.clone();
                                let line = format!(
                                    "{} · {} · {}",
                                    share.group,
                                    access_text(share.access),
                                    t::members(share.members),
                                );
                                view! {
                                    <li class="item-head">
                                        <span>{line}</span>
                                        <button
                                            type="button"
                                            class="secondary"
                                            on:click=move |_| remove(name.clone())
                                        >
                                            {t::SHARE_REMOVE}
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
        <form class="edit" on:submit=on_add>
            <label>
                {t::GROUP_SHARE_WITH}
                <input
                    type="text"
                    autocapitalize="none"
                    spellcheck="false"
                    maxlength="64"
                    list=list_id
                    bind:value=group
                />
            </label>
            <datalist id=datalist>
                {move || {
                    mine.get()
                        .into_iter()
                        .map(|name| view! { <option value=name></option> })
                        .collect_view()
                }}
            </datalist>
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
        <SuccessText message=success />
        <ErrorText error />
    }
}
