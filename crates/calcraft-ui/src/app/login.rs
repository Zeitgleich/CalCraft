//! Login form with optional second factor.

use calcraft_core::api::{LoginRequest, Me};
use calcraft_ui::{text as t, validate::clean_code};
use leptos::prelude::*;

use super::{Ctx, ErrorText, Session, api, submit};

#[component]
pub fn Login() -> impl IntoView {
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
        <main class="narrow">
            <h1>{t::LOGIN_TITLE}</h1>
            {move || ctx.notice.get().map(|notice| view! { <p class="success" role="status">{notice}</p> })}
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
        </main>
    }
}
