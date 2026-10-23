//! The Leptos application (client-side rendered).
//!
//! Rules that keep the UI compatible with the server's strict Content
//! Security Policy and safe against injected content:
//! - no inline scripts, no `style` attributes, no `inner_html`;
//! - user data is only ever rendered as text nodes or plain attribute
//!   values (colors are validated before they become an SVG `fill`).

mod account;
mod admin;
mod api;
mod app_passwords;
mod collections;
mod login;
mod reminders;
mod sharing;
mod subscriptions;
mod trash;

use calcraft_core::api::Me;
use calcraft_ui::{ApiFailure, Page, sso, text as t};
use leptos::{prelude::*, task::spawn_local};

/// Login state of the application.
#[derive(Debug, Clone, PartialEq)]
pub enum Session {
    Loading,
    LoggedOut,
    LoggedIn(Me),
    /// The API could not be reached or is disabled.
    Unavailable(String),
}

/// Shared state, provided as context.
#[derive(Clone, Copy)]
pub struct Ctx {
    pub session: RwSignal<Session>,
    /// Message shown on the login page (e.g. after a password change).
    pub notice: RwSignal<Option<String>>,
    /// Text for a failed SSO login (`/?sso_error=<code>`), shown on the
    /// login page.
    pub sso_error: RwSignal<Option<&'static str>>,
}

impl Ctx {
    pub fn get() -> Self {
        expect_context::<Ctx>()
    }

    /// The logged-in user (tracked).
    pub fn me(&self) -> Option<Me> {
        match self.session.get() {
            Session::LoggedIn(me) => Some(me),
            _ => None,
        }
    }

    /// Whether 2FA is on for the logged-in user (not tracked, for handlers).
    pub fn totp_enabled_untracked(&self) -> bool {
        self.session
            .with_untracked(|s| matches!(s, Session::LoggedIn(me) if me.totp_enabled))
    }

    /// Text for a failed call. An expired session returns to the login page.
    pub fn fail(&self, failure: &ApiFailure) -> String {
        if failure.is_unauthorized() {
            self.notice.set(Some(t::ERR_SESSION_EXPIRED.to_owned()));
            self.session.set(Session::LoggedOut);
        }
        failure.user_message()
    }

    /// Reloads the current user, e.g. after enabling 2FA.
    pub fn refresh_me(&self) {
        let ctx = *self;
        spawn_local(async move {
            match api::get::<Me>("/v1/me").await {
                Ok(me) => ctx.session.set(Session::LoggedIn(me)),
                Err(failure) => {
                    ctx.fail(&failure);
                }
            }
        });
    }
}

pub fn start() {
    leptos::mount::mount_to_body(App);
}

/// Reads a failed SSO login from the URL and removes the parameter, so
/// that a reload does not show the message again. Only the fixed text for
/// the code is kept, never the value itself.
fn take_sso_error() -> Option<&'static str> {
    let location = window().location();
    let (code, rest) = sso::take_error(&location.search().unwrap_or_default());
    let code = code?;
    let url = format!(
        "{}{rest}{}",
        location.pathname().unwrap_or_else(|_| "/".to_owned()),
        location.hash().unwrap_or_default()
    );
    if let Ok(history) = window().history() {
        let _ = history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&url));
    }
    Some(sso::error_message(&code))
}

fn current_page() -> Page {
    Page::from_hash(&window().location().hash().unwrap_or_default())
}

#[component]
fn App() -> impl IntoView {
    let ctx = Ctx {
        session: RwSignal::new(Session::Loading),
        notice: RwSignal::new(None),
        sso_error: RwSignal::new(take_sso_error()),
    };
    provide_context(ctx);

    spawn_local(async move {
        let session = match api::get::<Me>("/v1/me").await {
            Ok(me) => Session::LoggedIn(me),
            Err(failure) if failure.is_unauthorized() => Session::LoggedOut,
            Err(failure) => Session::Unavailable(failure.user_message()),
        };
        ctx.session.set(session);
    });

    // Only the login phase decides what is mounted; changes of the user
    // data (e.g. 2FA enabled) must not rebuild the pages and lose their
    // state.
    #[derive(Clone, Copy, PartialEq)]
    enum Phase {
        Loading,
        LoggedOut,
        LoggedIn,
        Unavailable,
    }
    let phase = Memo::new(move |_| match ctx.session.get() {
        Session::Loading => Phase::Loading,
        Session::LoggedOut => Phase::LoggedOut,
        Session::LoggedIn(_) => Phase::LoggedIn,
        Session::Unavailable(_) => Phase::Unavailable,
    });

    view! {
        <div class="layout">
            {move || match phase.get() {
                Phase::Loading => view! { <main class="narrow"><p>{t::LOADING}</p></main> }.into_any(),
                Phase::Unavailable => {
                    let message = match ctx.session.get_untracked() {
                        Session::Unavailable(message) => message,
                        _ => String::new(),
                    };
                    view! {
                        <main class="narrow">
                            <h1>{t::UNAVAILABLE_TITLE}</h1>
                            <p class="error" role="alert">{message}</p>
                            <p>{t::UNAVAILABLE_HINT}</p>
                        </main>
                    }
                        .into_any()
                }
                Phase::LoggedOut => view! { <login::Login /> }.into_any(),
                Phase::LoggedIn => view! { <Shell /> }.into_any(),
            }}
            <Footer />
        </div>
    }
}

/// Header with navigation and the current page.
#[component]
fn Shell() -> impl IntoView {
    let ctx = Ctx::get();
    let page = RwSignal::new(current_page());
    let listener = window_event_listener(leptos::ev::hashchange, move |_| {
        page.set(current_page());
    });
    on_cleanup(move || listener.remove());

    // A memo, so that other changes of the user data do not rebuild the
    // current page (which would e.g. lose freshly shown recovery codes).
    let is_admin = Memo::new(move |_| ctx.me().is_some_and(|me| me.is_admin));
    let is_admin = move || is_admin.get();
    let username = move || {
        ctx.me()
            .map(|me| me.display_name.unwrap_or(me.username))
            .unwrap_or_default()
    };
    let logout = move |_| {
        spawn_local(async move {
            // The local state is reset even if the request fails.
            let _ = api::send_empty("POST", "/v1/auth/logout").await;
            ctx.notice.set(Some(t::LOGGED_OUT.to_owned()));
            ctx.session.set(Session::LoggedOut);
        });
    };

    view! {
        <header class="topbar">
            <span class="brand">
                <Logo />
                {calcraft_core::NAME}
            </span>
            <nav aria-label=t::NAV_LABEL>
                {move || {
                    Page::ALL
                        .into_iter()
                        .filter(|p| p.visible(is_admin()))
                        .map(|p| {
                            view! {
                                <a
                                    href=p.hash()
                                    class:active=move || page.get() == p
                                    aria-current=move || (page.get() == p).then_some("page")
                                >
                                    {p.title()}
                                </a>
                            }
                        })
                        .collect_view()
                }}
            </nav>
            <span class="user">{username}</span>
            <button type="button" class="secondary" on:click=logout>
                {t::LOGOUT}
            </button>
        </header>
        <main>
            {move || {
                let shown = page.get();
                let shown = if shown.visible(is_admin()) { shown } else { Page::Overview };
                match shown {
                    Page::Overview => view! { <collections::Overview /> }.into_any(),
                    Page::Settings => view! { <account::Settings /> }.into_any(),
                    Page::AppPasswords => view! { <app_passwords::AppPasswords /> }.into_any(),
                    Page::Trash => view! { <trash::Trash /> }.into_any(),
                    Page::Admin => view! { <admin::Admin /> }.into_any(),
                }
            }}
        </main>
    }
}

/// The CalCraft mark: a calendar page with binder rings and a spark
/// (assets/brand/logo.svg). Inline, so that its colors follow the theme
/// through CSS classes; decorative, the name is always next to it.
#[component]
pub fn Logo() -> impl IntoView {
    view! {
        <svg class="logo" viewBox="0 0 64 64" aria-hidden="true" focusable="false">
            <rect class="logo-page" x="6" y="10" width="52" height="48" rx="12" />
            <path class="logo-band" d="M6 25V22A12 12 0 0 1 18 10H46A12 12 0 0 1 58 22V25Z" />
            <rect class="logo-gap" x="16" y="3" width="10" height="16" rx="5" />
            <rect class="logo-gap" x="38" y="3" width="10" height="16" rx="5" />
            <rect class="logo-ring" x="18" y="5" width="6" height="12" rx="3" />
            <rect class="logo-ring" x="40" y="5" width="6" height="12" rx="3" />
            <path
                class="logo-spark"
                d="M32 28.5Q33.9 38.9 44.5 41Q33.9 43.1 32 53.5Q30.1 43.1 19.5 41Q30.1 38.9 32 28.5Z"
            />
        </svg>
    }
}

/// Link to the source code of the running version (AGPL-3.0 §13).
#[component]
fn Footer() -> impl IntoView {
    view! {
        <footer>
            <span>{format!("{} {}", calcraft_core::NAME, calcraft_core::VERSION)}</span>
            " · "
            <a href=calcraft_core::SOURCE_URL rel="noopener noreferrer">
                {t::SOURCE_CODE}
            </a>
            " "
            <span class="muted">{t::LICENSE_NOTE}</span>
        </footer>
    }
}

/// Shows an error message if there is one.
#[component]
pub fn ErrorText(error: RwSignal<Option<String>>) -> impl IntoView {
    move || {
        error.get().map(|message| {
            view! {
                <p class="error" role="alert">
                    {message}
                </p>
            }
        })
    }
}

/// Shows a success message if there is one.
#[component]
pub fn SuccessText(message: RwSignal<Option<String>>) -> impl IntoView {
    move || {
        message.get().map(|message| {
            view! {
                <p class="success" role="status">
                    {message}
                </p>
            }
        })
    }
}

/// Runs a form submission: prevents the browser default, rejects double
/// submits and validation errors, then runs the request.
pub fn submit<F, Fut>(
    ev: leptos::ev::SubmitEvent,
    busy: RwSignal<bool>,
    error: RwSignal<Option<String>>,
    validate: impl FnOnce() -> Result<(), &'static str>,
    run: F,
) where
    F: FnOnce() -> Fut + 'static,
    Fut: std::future::Future<Output = Result<(), String>> + 'static,
{
    ev.prevent_default();
    if busy.get_untracked() {
        return;
    }
    if let Err(message) = validate() {
        error.set(Some(message.to_owned()));
        return;
    }
    error.set(None);
    busy.set(true);
    spawn_local(async move {
        let result = run().await;
        // The form may be gone by now (e.g. after a successful login).
        busy.try_set(false);
        if let Err(message) = result {
            error.try_set(Some(message));
        }
    });
}
