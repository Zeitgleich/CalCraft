//! E-mail reminder settings (account page) and the queue overview
//! (administration). Both show nothing while reminders are off on the
//! server (the API answers 404).

use calcraft_core::api::{
    AdminReminders, ReminderInfo, ReminderModeName, ReminderTest, UpdateReminders,
};
use calcraft_ui::{format_timestamp, text as t};
use leptos::{prelude::*, task::spawn_local};

use super::{Ctx, ErrorText, SuccessText, api};

fn mode_value(info: &ReminderInfo) -> &'static str {
    if !info.custom {
        return "default";
    }
    match info.mode {
        ReminderModeName::Off => "off",
        ReminderModeName::Email => "email",
        ReminderModeName::All => "all",
    }
}

fn mode_text(mode: ReminderModeName) -> &'static str {
    match mode {
        ReminderModeName::Off => t::REMINDER_MODE_OFF,
        ReminderModeName::Email => t::REMINDER_MODE_EMAIL,
        ReminderModeName::All => t::REMINDER_MODE_ALL,
    }
}

#[component]
pub fn Reminders() -> impl IntoView {
    let ctx = Ctx::get();
    let info = RwSignal::new(None::<ReminderInfo>);
    let error = RwSignal::new(None::<String>);
    let notice = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let reload = Callback::new(move |()| {
        spawn_local(async move {
            match api::get::<ReminderInfo>("/v1/me/reminders").await {
                Ok(value) => {
                    info.try_set(Some(value));
                }
                // Reminders are off on the server: no section.
                Err(failure) if failure.status == 404 => {}
                Err(failure) => {
                    error.try_set(Some(ctx.fail(&failure)));
                }
            }
        });
    });
    reload.run(());

    let update = move |request: UpdateReminders| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        notice.set(None);
        spawn_local(async move {
            let result = api::send_no_content("PATCH", "/v1/me/reminders", &request).await;
            busy.try_set(false);
            match result {
                Ok(()) => {
                    error.try_set(None);
                    reload.run(());
                }
                Err(failure) => {
                    error.try_set(Some(ctx.fail(&failure)));
                }
            }
        });
    };
    let send_test = move |_| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        notice.set(None);
        spawn_local(async move {
            let result = api::post_for::<ReminderTest>("/v1/me/reminders/test").await;
            busy.try_set(false);
            match result {
                Ok(outcome) => {
                    error.try_set(None);
                    notice.try_set(Some(
                        if outcome.queued {
                            t::REMINDER_TEST_SENT
                        } else {
                            t::REMINDER_TEST_DROPPED
                        }
                        .to_owned(),
                    ));
                    reload.run(());
                }
                Err(failure) => {
                    error.try_set(Some(ctx.fail(&failure)));
                }
            }
        });
    };

    move || {
        info.get().map(|info| {
            let default_label = format!(
                "{} ({})",
                t::REMINDER_MODE_DEFAULT,
                mode_text(info.default_mode)
            );
            let selected = mode_value(&info);
            let status = &info.status;
            let counts = t::reminder_counts(
                "Last 7 days",
                status.sent,
                status.pending,
                status.failed,
                status.dropped,
            );
            let problem = status.last_error.as_deref().map(|code| {
                let at = status
                    .last_error_at
                    .map(format_timestamp)
                    .unwrap_or_default();
                format!(
                    "{}: {} ({at})",
                    t::REMINDER_LAST_PROBLEM,
                    t::mail_error(code)
                )
            });
            let has_address = info.address.is_some();
            view! {
                <section>
                    <h2>{t::REMINDERS}</h2>
                    {match info.address.clone() {
                        Some(address) => {
                            view! {
                                <p>{t::REMINDERS_TO} ": " <code>{address}</code></p>
                            }
                                .into_any()
                        }
                        None => view! { <p class="error">{t::REMINDERS_NO_ADDRESS}</p> }.into_any(),
                    }}
                    <label>
                        {t::REMINDER_MODE}
                        <select
                            disabled=move || busy.get()
                            on:change=move |ev| {
                                update(UpdateReminders {
                                    mode: Some(event_target_value(&ev)),
                                    include_description: None,
                                });
                            }
                        >
                            <option value="default" selected=selected == "default">
                                {default_label}
                            </option>
                            <option value="off" selected=selected == "off">
                                {t::REMINDER_MODE_OFF}
                            </option>
                            <option value="email" selected=selected == "email">
                                {t::REMINDER_MODE_EMAIL}
                            </option>
                            <option value="all" selected=selected == "all">
                                {t::REMINDER_MODE_ALL}
                            </option>
                        </select>
                    </label>
                    <label class="inline">
                        <input
                            type="checkbox"
                            prop:checked=info.include_description
                            disabled=move || busy.get()
                            on:change=move |ev| {
                                update(UpdateReminders {
                                    mode: None,
                                    include_description: Some(event_target_checked(&ev)),
                                });
                            }
                        />
                        {t::REMINDER_DESCRIPTION}
                    </label>
                    <p class="hint">{t::REMINDERS_HINT}</p>
                    <p class="muted">{counts}</p>
                    {problem.map(|text| view! { <p class="muted">{text}</p> })}
                    <button
                        type="button"
                        disabled=move || busy.get() || !has_address
                        on:click=send_test
                    >
                        {t::REMINDER_TEST}
                    </button>
                    <SuccessText message=notice />
                    <ErrorText error />
                </section>
            }
        })
    }
}

/// Queue overview for administrators; nothing while reminders are off.
#[component]
pub fn ReminderQueue() -> impl IntoView {
    let info = RwSignal::new(None::<AdminReminders>);
    spawn_local(async move {
        if let Ok(value) = api::get::<AdminReminders>("/v1/admin/reminders").await {
            info.try_set(Some(value));
        }
    });
    move || {
        info.get().map(|info| {
            let status = &info.status;
            let counts = t::reminder_counts(
                "Last 24 hours",
                status.sent,
                status.pending,
                status.failed,
                status.dropped,
            );
            let success = info.last_success_at.map(format_timestamp);
            let failure = info.last_smtp_error.as_deref().map(|code| {
                let at = info
                    .last_smtp_error_at
                    .map(format_timestamp)
                    .unwrap_or_default();
                format!("{} ({at})", t::mail_error(code))
            });
            view! {
                <section>
                    <h2>{t::REMINDER_QUEUE}</h2>
                    <dl class="facts">
                        <dt>{t::REMINDER_MODE_DEFAULT}</dt>
                        <dd>{mode_text(info.default_mode)}</dd>
                        <dt>{t::REMINDER_SCHEDULED}</dt>
                        <dd>{info.scheduled}</dd>
                        <dt>{t::STATUS}</dt>
                        <dd>{counts}</dd>
                        {success
                            .map(|at| {
                                view! {
                                    <dt>{t::REMINDER_LAST_SUCCESS}</dt>
                                    <dd>{at}</dd>
                                }
                            })}
                        {failure
                            .map(|text| {
                                view! {
                                    <dt>{t::REMINDER_LAST_SMTP_ERROR}</dt>
                                    <dd>{text}</dd>
                                }
                            })}
                    </dl>
                </section>
            }
        })
    }
}
