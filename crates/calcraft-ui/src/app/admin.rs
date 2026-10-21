//! User administration (administrators only; the server checks again).

use calcraft_core::api::{AdminUser, CreateUser, UpdateUser};
use calcraft_ui::{
    text as t,
    validate::{MAX_DISPLAY_NAME, check_length, check_new_password, check_username},
};
use leptos::prelude::*;

use super::{Ctx, ErrorText, api, submit};

#[component]
pub fn Admin() -> impl IntoView {
    let ctx = Ctx::get();
    let users = RwSignal::new(None::<Vec<AdminUser>>);
    let error = RwSignal::new(None::<String>);
    let reload = Callback::new(move |()| {
        leptos::task::spawn_local(async move {
            match api::get::<Vec<AdminUser>>("/v1/admin/users").await {
                Ok(list) => {
                    users.try_set(Some(list));
                }
                Err(failure) => {
                    error.try_set(Some(ctx.fail(&failure)));
                }
            }
        });
    });
    reload.run(());

    view! {
        <h1>{t::ADMIN_TITLE}</h1>
        <ErrorText error />
        <section>
            <h2>{t::USERS}</h2>
            {move || match users.get() {
                None => view! { <p class="muted">{t::LOADING}</p> }.into_any(),
                Some(list) => {
                    view! {
                        <table>
                            <thead>
                                <tr>
                                    <th>{t::USERNAME}</th>
                                    <th>{t::DISPLAY_NAME}</th>
                                    <th>{t::STATUS}</th>
                                    <th><span class="visually-hidden">{t::ACTIONS}</span></th>
                                </tr>
                            </thead>
                            <tbody>
                                {list
                                    .into_iter()
                                    .map(|user| view! { <UserRow user on_change=reload /> })
                                    .collect_view()}
                            </tbody>
                        </table>
                    }
                        .into_any()
                }
            }}
        </section>
        <CreateUserForm on_created=reload />
    }
}

#[component]
fn UserRow(user: AdminUser, on_change: Callback<()>) -> impl IntoView {
    let ctx = Ctx::get();
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let is_self = ctx.session.with_untracked(
        |s| matches!(s, super::Session::LoggedIn(me) if me.username == user.username),
    );
    let path = format!("/v1/admin/users/{}", user.username);

    let apply = move |update: UpdateUser| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        let path = path.clone();
        leptos::task::spawn_local(async move {
            let result = api::send_no_content("PATCH", &path, &update).await;
            busy.try_set(false);
            match result {
                Ok(()) => on_change.run(()),
                Err(failure) => {
                    error.try_set(Some(ctx.fail(&failure)));
                }
            }
        });
    };
    let toggle_disabled = {
        let apply = apply.clone();
        let disabled = user.disabled;
        move |_| {
            apply(UpdateUser {
                disabled: Some(!disabled),
                is_admin: None,
            })
        }
    };
    let is_admin = user.is_admin;
    let toggle_admin = move |_| {
        apply(UpdateUser {
            disabled: None,
            is_admin: Some(!is_admin),
        })
    };

    let mut status = Vec::new();
    if user.is_admin {
        status.push(t::ROLE_ADMIN);
    }
    status.push(if user.disabled {
        t::DISABLED
    } else {
        t::ACTIVE
    });
    if user.totp_enabled {
        status.push(t::TWO_FACTOR_ON);
    }

    view! {
        <tr class:disabled=user.disabled>
            <td>
                <code>{user.username.clone()}</code>
                {is_self.then(|| view! { " " <span class="muted">{t::YOU}</span> })}
            </td>
            <td>{user.display_name.clone().unwrap_or_default()}</td>
            <td>{status.join(" · ")}</td>
            <td class="actions">
                {(!is_self)
                    .then(|| {
                        view! {
                            <button
                                type="button"
                                class="secondary"
                                disabled=move || busy.get()
                                on:click=toggle_disabled
                            >
                                {if user.disabled { t::ENABLE } else { t::DISABLE }}
                            </button>
                            <button
                                type="button"
                                class="secondary"
                                disabled=move || busy.get()
                                on:click=toggle_admin
                            >
                                {if user.is_admin { t::REVOKE_ADMIN } else { t::GRANT_ADMIN }}
                            </button>
                        }
                    })}
                <ErrorText error />
            </td>
        </tr>
    }
}

#[component]
fn CreateUserForm(on_created: Callback<()>) -> impl IntoView {
    let ctx = Ctx::get();
    let username = RwSignal::new(String::new());
    let display_name = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let confirmation = RwSignal::new(String::new());
    let is_admin = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let success = RwSignal::new(None::<String>);

    let on_submit = move |ev| {
        success.set(None);
        submit(
            ev,
            busy,
            error,
            move || {
                check_username(username.get_untracked().trim())?;
                check_length(&display_name.get_untracked(), MAX_DISPLAY_NAME)?;
                check_new_password(&password.get_untracked(), &confirmation.get_untracked())
            },
            move || async move {
                let display = display_name.get_untracked().trim().to_owned();
                let request = CreateUser {
                    username: username.get_untracked().trim().to_owned(),
                    password: password.get_untracked(),
                    display_name: (!display.is_empty()).then_some(display),
                    is_admin: is_admin.get_untracked(),
                };
                let user = api::send::<_, AdminUser>("POST", "/v1/admin/users", &request)
                    .await
                    .map_err(|failure| ctx.fail(&failure))?;
                username.set(String::new());
                display_name.set(String::new());
                password.set(String::new());
                confirmation.set(String::new());
                is_admin.set(false);
                success.set(Some(t::user_created(&user.username)));
                on_created.run(());
                Ok(())
            },
        );
    };

    view! {
        <section>
            <h2>{t::CREATE_USER}</h2>
            <super::SuccessText message=success />
            <form class="card" on:submit=on_submit>
                <label>
                    {t::USERNAME}
                    <input
                        type="text"
                        required
                        autocomplete="off"
                        autocapitalize="none"
                        spellcheck="false"
                        maxlength="64"
                        bind:value=username
                    />
                    <span class="hint">{t::USERNAME_HINT}</span>
                </label>
                <label>
                    {t::DISPLAY_NAME}
                    <input type="text" autocomplete="off" maxlength=MAX_DISPLAY_NAME bind:value=display_name />
                </label>
                <label>
                    {t::PASSWORD}
                    <input type="password" autocomplete="new-password" required bind:value=password />
                    <span class="hint">{t::PASSWORD_RULES}</span>
                </label>
                <label>
                    {t::CONFIRM_PASSWORD}
                    <input type="password" autocomplete="new-password" required bind:value=confirmation />
                </label>
                <label class="inline">
                    <input type="checkbox" bind:checked=is_admin />
                    {t::MAKE_ADMIN}
                </label>
                <ErrorText error />
                <button type="submit" disabled=move || busy.get()>
                    {t::CREATE}
                </button>
            </form>
        </section>
    }
}
