//! Account settings: password and two-factor authentication.

use calcraft_core::api::{
    ChangePassword, RecoveryCodes, TotpDisable, TotpEnable, TotpSetup, UpdateScheduling,
};
use calcraft_ui::{
    group_secret, sso, text as t,
    validate::{check_new_password, clean_code},
};
use leptos::prelude::*;

use super::{Ctx, ErrorText, Session, SuccessText, api, submit};

#[component]
pub fn Settings() -> impl IntoView {
    let ctx = Ctx::get();
    let username = move || ctx.me().map(|me| me.username).unwrap_or_default();
    let display_name = move || ctx.me().and_then(|me| me.display_name).unwrap_or_default();
    let role = move || {
        if ctx.me().is_some_and(|me| me.is_admin) {
            t::ROLE_ADMIN
        } else {
            t::ROLE_USER
        }
    };

    view! {
        <h1>{t::SETTINGS_TITLE}</h1>
        <section>
            <h2>{t::ACCOUNT}</h2>
            <dl class="facts">
                <dt>{t::USERNAME}</dt>
                <dd>{username}</dd>
                <dt>{t::DISPLAY_NAME}</dt>
                <dd>{display_name}</dd>
                <dt>{t::ROLE}</dt>
                <dd>{role}</dd>
                <SsoFacts />
            </dl>
        </section>
        <super::groups::MyGroups />
        <Invitations />
        <PasswordForm />
        <TwoFactor />
    }
}

/// Whether other users may invite this user; nothing if scheduling is off
/// on the server.
#[component]
fn Invitations() -> impl IntoView {
    let ctx = Ctx::get();
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let toggle = move |ev: leptos::ev::Event| {
        let accept = event_target_checked(&ev);
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        leptos::task::spawn_local(async move {
            let request = UpdateScheduling {
                accept_invitations: accept,
            };
            let result = api::send_no_content("PATCH", "/v1/me/scheduling", &request).await;
            busy.try_set(false);
            match result {
                Ok(()) => ctx.refresh_me(),
                Err(failure) => {
                    error.try_set(Some(ctx.fail(&failure)));
                }
            }
        });
    };
    move || {
        ctx.me().and_then(|me| me.scheduling).map(|info| {
            let addresses = info.addresses.join(", ");
            view! {
                <section>
                    <h2>{t::INVITATIONS}</h2>
                    {(!addresses.is_empty())
                        .then(|| {
                            view! {
                                <p>
                                    {t::SCHEDULING_ADDRESSES} ": " <code>{addresses}</code>
                                </p>
                            }
                        })}
                    <label class="inline">
                        <input
                            type="checkbox"
                            prop:checked=info.accept_invitations
                            disabled=move || busy.get()
                            on:change=toggle
                        />
                        {t::ACCEPT_INVITATIONS}
                    </label>
                    <p class="hint">{t::INVITATIONS_HINT}</p>
                    <ErrorText error />
                </section>
            }
        })
    }
}

/// SSO link status as a row of the account facts; nothing if SSO is not
/// configured on the server.
#[component]
fn SsoFacts() -> impl IntoView {
    let ctx = Ctx::get();
    move || {
        ctx.me().and_then(|me| me.sso).map(|link| {
            let hint = if link.linked {
                sso::last_login(&link)
            } else {
                Some(t::SSO_NOT_LINKED_HINT.to_owned())
            };
            view! {
                <dt>{t::SSO}</dt>
                <dd>
                    {sso::link_status(&link)}
                    {hint.map(|hint| view! { <span class="hint">{hint}</span> })}
                </dd>
            }
        })
    }
}

#[component]
fn PasswordForm() -> impl IntoView {
    let ctx = Ctx::get();
    let current = RwSignal::new(String::new());
    let new = RwSignal::new(String::new());
    let confirmation = RwSignal::new(String::new());
    let code = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let totp_enabled = move || ctx.me().is_some_and(|me| me.totp_enabled);

    let on_submit = move |ev| {
        submit(
            ev,
            busy,
            error,
            move || {
                if current.get_untracked().is_empty() {
                    return Err(t::ERR_CURRENT_PASSWORD);
                }
                if ctx.totp_enabled_untracked() && clean_code(&code.get_untracked()).is_empty() {
                    return Err(t::ERR_TOTP_REQUIRED);
                }
                check_new_password(&new.get_untracked(), &confirmation.get_untracked())
            },
            move || async move {
                let with_code = ctx.totp_enabled_untracked();
                let request = ChangePassword {
                    current: current.get_untracked(),
                    new: new.get_untracked(),
                    code: with_code.then(|| clean_code(&code.get_untracked())),
                };
                match api::send_no_content("POST", "/v1/me/password", &request).await {
                    Ok(()) => {
                        // The server ended all sessions.
                        ctx.notice.set(Some(t::PASSWORD_CHANGED.to_owned()));
                        ctx.session.set(Session::LoggedOut);
                        Ok(())
                    }
                    Err(failure) => {
                        code.set(String::new());
                        Err(ctx.fail(&failure))
                    }
                }
            },
        );
    };

    view! {
        <section>
            <h2>{t::CHANGE_PASSWORD}</h2>
            <form class="card" on:submit=on_submit>
                <label>
                    {t::CURRENT_PASSWORD}
                    <input type="password" autocomplete="current-password" required bind:value=current />
                </label>
                <label>
                    {t::NEW_PASSWORD}
                    <input type="password" autocomplete="new-password" required bind:value=new />
                    <span class="hint">{t::PASSWORD_RULES}</span>
                </label>
                <label>
                    {t::CONFIRM_PASSWORD}
                    <input type="password" autocomplete="new-password" required bind:value=confirmation />
                </label>
                <Show when=totp_enabled>
                    <label>
                        {t::TOTP_CODE}
                        <input type="text" autocomplete="one-time-code" required bind:value=code />
                        <span class="hint">{t::TOTP_CODE_HINT}</span>
                    </label>
                </Show>
                <p class="hint">{t::PASSWORD_CHANGE_LOGOUT}</p>
                <ErrorText error />
                <button type="submit" disabled=move || busy.get()>
                    {t::CHANGE_PASSWORD}
                </button>
            </form>
        </section>
    }
}

#[component]
fn TwoFactor() -> impl IntoView {
    let ctx = Ctx::get();
    let totp_enabled = Memo::new(move |_| ctx.me().is_some_and(|me| me.totp_enabled));
    // Shown once after enabling; kept outside the enabled/disabled branches
    // so that refreshing the user does not hide them.
    let recovery = RwSignal::new(None::<Vec<String>>);
    let success = RwSignal::new(None::<String>);

    view! {
        <section>
            <h2>{t::TWO_FACTOR}</h2>
            <SuccessText message=success />
            {move || {
                recovery
                    .get()
                    .map(|codes| {
                        view! {
                            <div class="card secret-box">
                                <h3>{t::RECOVERY_CODES}</h3>
                                <p class="warning">{t::RECOVERY_CODES_HINT}</p>
                                <ul class="codes">
                                    {codes.into_iter().map(|c| view! { <li><code>{c}</code></li> }).collect_view()}
                                </ul>
                                <button type="button" on:click=move |_| recovery.set(None)>
                                    {t::RECOVERY_CODES_SAVED}
                                </button>
                            </div>
                        }
                    })
            }}
            {move || {
                if totp_enabled.get() {
                    view! { <TotpDisableForm success /> }.into_any()
                } else {
                    view! { <TotpSetupForm recovery success /> }.into_any()
                }
            }}
        </section>
    }
}

#[component]
fn TotpSetupForm(
    recovery: RwSignal<Option<Vec<String>>>,
    success: RwSignal<Option<String>>,
) -> impl IntoView {
    let ctx = Ctx::get();
    let setup = RwSignal::new(None::<TotpSetup>);
    let code = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);

    let start = move |_| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        error.set(None);
        success.set(None);
        leptos::task::spawn_local(async move {
            let result = api::post_for::<TotpSetup>("/v1/me/totp/setup").await;
            busy.try_set(false);
            match result {
                Ok(data) => {
                    setup.try_set(Some(data));
                }
                Err(failure) => {
                    error.try_set(Some(ctx.fail(&failure)));
                }
            }
        });
    };

    let on_enable = move |ev| {
        submit(
            ev,
            busy,
            error,
            move || {
                if clean_code(&code.get_untracked()).is_empty() {
                    Err(t::ERR_TOTP_REQUIRED)
                } else {
                    Ok(())
                }
            },
            move || async move {
                let request = TotpEnable {
                    code: clean_code(&code.get_untracked()),
                };
                match api::send::<_, RecoveryCodes>("POST", "/v1/me/totp/enable", &request).await {
                    Ok(codes) => {
                        recovery.set(Some(codes.codes));
                        success.set(Some(t::TOTP_ENABLED.to_owned()));
                        ctx.refresh_me();
                        Ok(())
                    }
                    Err(failure) => {
                        code.set(String::new());
                        Err(ctx.fail(&failure))
                    }
                }
            },
        );
    };

    view! {
        <p>{t::TOTP_OFF}</p>
        {move || match setup.get() {
            None => {
                view! {
                    <button type="button" on:click=start disabled=move || busy.get()>
                        {t::TOTP_SET_UP}
                    </button>
                }
                    .into_any()
            }
            Some(data) => {
                view! {
                    <form class="card" on:submit=on_enable>
                        <p>{t::TOTP_SETUP_HINT}</p>
                        <dl class="facts">
                            <dt>{t::TOTP_SECRET}</dt>
                            <dd><code class="secret">{group_secret(&data.secret)}</code></dd>
                            <dt>{t::TOTP_URI}</dt>
                            <dd><code class="wrap">{data.uri}</code></dd>
                        </dl>
                        <label>
                            {t::TOTP_CONFIRM_CODE}
                            <input
                                type="text"
                                inputmode="numeric"
                                autocomplete="one-time-code"
                                required
                                bind:value=code
                            />
                        </label>
                        <button type="submit" disabled=move || busy.get()>
                            {t::TOTP_ENABLE}
                        </button>
                    </form>
                }
                    .into_any()
            }
        }}
        <ErrorText error />
    }
}

#[component]
fn TotpDisableForm(success: RwSignal<Option<String>>) -> impl IntoView {
    let ctx = Ctx::get();
    let password = RwSignal::new(String::new());
    let code = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);

    let on_submit = move |ev| {
        submit(
            ev,
            busy,
            error,
            move || {
                if password.get_untracked().is_empty()
                    || clean_code(&code.get_untracked()).is_empty()
                {
                    Err(t::ERR_DISABLE_FIELDS)
                } else {
                    Ok(())
                }
            },
            move || async move {
                let request = TotpDisable {
                    password: password.get_untracked(),
                    code: clean_code(&code.get_untracked()),
                };
                match api::send_no_content("POST", "/v1/me/totp/disable", &request).await {
                    Ok(()) => {
                        success.set(Some(t::TOTP_DISABLED.to_owned()));
                        ctx.refresh_me();
                        Ok(())
                    }
                    Err(failure) => {
                        code.set(String::new());
                        Err(ctx.fail(&failure))
                    }
                }
            },
        );
    };

    view! {
        <p>{t::TOTP_ON}</p>
        <form class="card" on:submit=on_submit>
            <p class="hint">{t::TOTP_DISABLE_HINT}</p>
            <label>
                {t::PASSWORD}
                <input type="password" autocomplete="current-password" required bind:value=password />
            </label>
            <label>
                {t::TOTP_CODE}
                <input type="text" autocomplete="one-time-code" required bind:value=code />
                <span class="hint">{t::TOTP_CODE_HINT}</span>
            </label>
            <ErrorText error />
            <button type="submit" class="danger" disabled=move || busy.get()>
                {t::TOTP_DISABLE}
            </button>
        </form>
    }
}
