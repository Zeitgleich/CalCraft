//! App passwords for DAV clients.

use calcraft_core::api::{AppPasswordInfo, CreateAppPassword, CreatedAppPassword};
use calcraft_ui::{
    format_timestamp, text as t,
    validate::{MAX_APP_PASSWORD_NAME, check_app_password_name},
};
use leptos::prelude::*;

use super::{Ctx, ErrorText, api, submit};

#[component]
pub fn AppPasswords() -> impl IntoView {
    let ctx = Ctx::get();
    let items = RwSignal::new(None::<Vec<AppPasswordInfo>>);
    let error = RwSignal::new(None::<String>);
    let created = RwSignal::new(None::<CreatedAppPassword>);
    let reload = Callback::new(move |()| {
        leptos::task::spawn_local(async move {
            match api::get::<Vec<AppPasswordInfo>>("/v1/app-passwords").await {
                Ok(list) => {
                    items.try_set(Some(list));
                }
                Err(failure) => {
                    error.try_set(Some(ctx.fail(&failure)));
                }
            }
        });
    });
    reload.run(());

    let username = move || ctx.me().map(|me| me.username).unwrap_or_default();

    view! {
        <h1>{t::APP_PASSWORDS_TITLE}</h1>
        <p class="hint">{t::APP_PASSWORDS_HINT}</p>
        {move || {
            created
                .get()
                .map(|created_password| {
                    view! {
                        <div class="card secret-box" role="status">
                            <h3>{t::app_password_created(&created_password.name)}</h3>
                            <p class="warning">{t::APP_PASSWORD_ONCE}</p>
                            <dl class="facts">
                                <dt>{t::USERNAME}</dt>
                                <dd><code>{username}</code></dd>
                                <dt>{t::PASSWORD}</dt>
                                <dd><code class="secret wrap">{created_password.password}</code></dd>
                            </dl>
                            <button type="button" on:click=move |_| created.set(None)>
                                {t::APP_PASSWORD_DONE}
                            </button>
                        </div>
                    }
                })
        }}
        <ErrorText error />
        <section>
            {move || match items.get() {
                None => view! { <p class="muted">{t::LOADING}</p> }.into_any(),
                Some(list) if list.is_empty() => {
                    view! { <p class="muted">{t::NO_APP_PASSWORDS}</p> }.into_any()
                }
                Some(list) => {
                    view! {
                        <table>
                            <thead>
                                <tr>
                                    <th>{t::NAME}</th>
                                    <th>{t::CREATED}</th>
                                    <th>{t::LAST_USED}</th>
                                    <th><span class="visually-hidden">{t::ACTIONS}</span></th>
                                </tr>
                            </thead>
                            <tbody>
                                {list
                                    .into_iter()
                                    .map(|info| view! { <AppPasswordRow info on_change=reload /> })
                                    .collect_view()}
                            </tbody>
                        </table>
                    }
                        .into_any()
                }
            }}
        </section>
        <CreateForm created on_created=reload />
    }
}

#[component]
fn AppPasswordRow(info: AppPasswordInfo, on_change: Callback<()>) -> impl IntoView {
    let ctx = Ctx::get();
    let confirming = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let path = format!("/v1/app-passwords/{}", info.id);
    let confirm_text = t::confirm_revoke(&info.name);

    let revoke = move |_| {
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

    view! {
        <tr>
            <td>{info.name}</td>
            <td>{format_timestamp(info.created_at)}</td>
            <td>{info.last_used_at.map_or_else(|| t::NEVER.to_owned(), format_timestamp)}</td>
            <td>
                <div class="actions">
                    <Show
                        when=move || confirming.get()
                        fallback=move || {
                            view! {
                                <button type="button" class="danger" on:click=move |_| confirming.set(true)>
                                    {t::REVOKE}
                                </button>
                            }
                        }
                    >
                        <span class="confirm-inline">
                            <span>{confirm_text.clone()}</span>
                            <button type="button" class="danger" on:click=revoke.clone() disabled=move || busy.get()>
                                {t::REVOKE_CONFIRM}
                            </button>
                            <button type="button" class="secondary" on:click=move |_| confirming.set(false)>
                                {t::CANCEL}
                            </button>
                        </span>
                    </Show>
                    <ErrorText error />
                </div>
            </td>
        </tr>
    }
}

#[component]
fn CreateForm(
    created: RwSignal<Option<CreatedAppPassword>>,
    on_created: Callback<()>,
) -> impl IntoView {
    let ctx = Ctx::get();
    let name = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);

    let on_submit = move |ev| {
        submit(
            ev,
            busy,
            error,
            move || check_app_password_name(&name.get_untracked()),
            move || async move {
                let request = CreateAppPassword {
                    name: name.get_untracked().trim().to_owned(),
                };
                let result =
                    api::send::<_, CreatedAppPassword>("POST", "/v1/app-passwords", &request)
                        .await
                        .map_err(|failure| ctx.fail(&failure))?;
                name.set(String::new());
                created.set(Some(result));
                on_created.run(());
                Ok(())
            },
        );
    };

    view! {
        <section>
            <h2>{t::CREATE_APP_PASSWORD}</h2>
            <form class="card" on:submit=on_submit>
                <label>
                    {t::APP_PASSWORD_NAME}
                    <input type="text" required maxlength=MAX_APP_PASSWORD_NAME bind:value=name />
                    <span class="hint">{t::APP_PASSWORD_NAME_HINT}</span>
                </label>
                <ErrorText error />
                <button type="submit" disabled=move || busy.get()>
                    {t::CREATE}
                </button>
            </form>
        </section>
    }
}
