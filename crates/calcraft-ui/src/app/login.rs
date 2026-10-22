//! Login page: single sign-on button and/or password form with optional
//! second factor, depending on what the server offers.

use calcraft_core::api::{AuthMethods, LoginRequest, Me};
use calcraft_ui::{sso, text as t, validate::clean_code};
use leptos::{prelude::*, task::spawn_local};

use super::{Ctx, ErrorText, Session, api, submit};

#[component]
pub fn Login() -> impl IntoView {
    let ctx = Ctx::get();
    // `None` while loading. If the methods cannot be loaded, the password
    // form is shown; the server stays authoritative anyway.
    let methods = RwSignal::new(None::<AuthMethods>);
    spawn_local(async move {
        let loaded = api::get::<AuthMethods>("/v1/auth/methods")
            .await
            .unwrap_or(AuthMethods {
                password: true,
                oidc: None,
            });
        methods.try_set(Some(loaded));
    });

    view! {
        <main class="narrow">
            <h1>{t::LOGIN_TITLE}</h1>
            {move || ctx.notice.get().map(|notice| view! { <p class="success" role="status">{notice}</p> })}
            {move || ctx.sso_error.get().map(|message| view! { <p class="error" role="alert">{message}</p> })}
            {move || match methods.get() {
                None => view! { <p>{t::LOADING}</p> }.into_any(),
                Some(methods) => {
                    let sso_button = methods.oidc.map(|oidc| {
                        let href = sso::login_url(&oidc.login_url).to_owned();
                        view! {
                            <div class="card">
                                <a class="button" href=href>
                                    {sso::button_label(&oidc.label)}
                                </a>
                            </div>
                        }
                    });
                    let both = sso_button.is_some() && methods.password;
                    let nothing = sso_button.is_none() && !methods.password;
                    view! {
                        {sso_button}
                        {both.then(|| view! { <p class="muted separator">{t::LOGIN_OR_PASSWORD}</p> })}
                        {methods.password.then(|| view! { <PasswordForm /> })}
                        {nothing.then(|| view! { <p class="error" role="alert">{t::LOGIN_NO_METHOD}</p> })}
                    }
                        .into_any()
                }
            }}
        </main>
    }
}

/// Username and password, then the second factor if the server asks.
#[component]
fn PasswordForm() -> impl IntoView {
    let ctx = Ctx::get();
    let username = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let code = RwSignal::new(String::new());
    let need_code = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);

    let on_submit = move |ev| {
        submit(
            ev,
            busy,
            error,
            || {
                if username.get_untracked().trim().is_empty() || password.get_untracked().is_empty()
                {
                    return Err(t::ERR_LOGIN_FIELDS);
                }
                if need_code.get_untracked() && clean_code(&code.get_untracked()).is_empty() {
                    return Err(t::ERR_TOTP_REQUIRED);
                }
                Ok(())
            },
            move || async move {
                let request = LoginRequest {
                    username: username.get_untracked().trim().to_owned(),
                    password: password.get_untracked(),
                    code: need_code
                        .get_untracked()
                        .then(|| clean_code(&code.get_untracked())),
                };
                match api::send::<_, Me>("POST", "/v1/auth/login", &request).await {
                    Ok(me) => {
                        ctx.notice.set(None);
                        ctx.sso_error.set(None);
                        ctx.session.set(Session::LoggedIn(me));
                        Ok(())
                    }
                    Err(failure) if failure.is_totp_required() => {
                        need_code.set(true);
                        Err(t::LOGIN_ENTER_CODE.to_owned())
                    }
                    Err(failure) => {
                        code.set(String::new());
                        Err(failure.user_message())
                    }
                }
            },
        );
    };

    view! {
            <form class="card" on:submit=on_submit>
                <label>
                    {t::USERNAME}
                    <input
                        type="text"
                        name="username"
                        autocomplete="username"
                        autocapitalize="none"
                        spellcheck="false"
                        required
                        bind:value=username
                    />
                </label>
                <label>
                    {t::PASSWORD}
                    <input
                        type="password"
                        name="password"
                        autocomplete="current-password"
                        required
                        bind:value=password
                    />
                </label>
                <Show when=move || need_code.get()>
                    <label>
                        {t::TOTP_CODE}
                        <input
                            type="text"
                            name="code"
                            autocomplete="one-time-code"
                            autocapitalize="none"
                            spellcheck="false"
                            required
                            bind:value=code
                        />
                        <span class="hint">{t::TOTP_CODE_HINT}</span>
                    </label>
                </Show>
                <ErrorText error />
                <button type="submit" disabled=move || busy.get()>
                    {t::LOGIN}
                </button>
            </form>
    }
}
