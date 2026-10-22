//! Trash: restore deleted entries or delete them permanently.
//!
//! Summaries and names come from calendar and contact data and are only
//! rendered as text.

use calcraft_core::{
    CollectionKind,
    api::{RestoredTrashEntry, TrashEntryInfo, TrashKind, TrashList},
};
use calcraft_ui::{format_size, format_timestamp, text as t};
use leptos::{prelude::*, task::spawn_local};

use super::{Ctx, ErrorText, SuccessText, api};

#[component]
pub fn Trash() -> impl IntoView {
    let ctx = Ctx::get();
    let list = RwSignal::new(None::<TrashList>);
    let error = RwSignal::new(None::<String>);
    let notice = RwSignal::new(None::<String>);
    let confirming_empty = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let reload = Callback::new(move |()| {
        spawn_local(async move {
            match api::get::<TrashList>("/v1/trash?limit=200").await {
                Ok(page) => {
                    error.try_set(None);
                    list.try_set(Some(page));
                }
                Err(failure) => {
                    error.try_set(Some(ctx.fail(&failure)));
                }
            }
        });
    });
    reload.run(());

    let empty_trash = move |_| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        notice.set(None);
        spawn_local(async move {
            let result = api::send_empty("DELETE", "/v1/trash").await;
            busy.try_set(false);
            confirming_empty.try_set(false);
            match result {
                Ok(()) => {
                    notice.try_set(Some(t::TRASH_EMPTIED.to_owned()));
                    reload.run(());
                }
                Err(failure) => {
                    error.try_set(Some(ctx.fail(&failure)));
                }
            }
        });
    };

    view! {
        <h1>{t::TRASH_TITLE}</h1>
        {move || {
            list.get()
                .map(|page| {
                    let hint = if page.retention_days == 0 {
                        t::TRASH_DISABLED.to_owned()
                    } else {
                        t::trash_hint(page.retention_days)
                    };
                    view! { <p class="hint">{hint}</p> }
                })
        }}
        <ErrorText error />
        <SuccessText message=notice />
        {move || match list.get() {
            None => view! { <p class="muted">{t::LOADING}</p> }.into_any(),
            Some(page) if page.entries.is_empty() => {
                view! { <p class="muted empty">{t::TRASH_EMPTY}</p> }.into_any()
            }
            Some(page) => {
                let usage = t::trash_usage(page.total, &format_size(page.bytes));
                let shown = page.entries.len();
                let more = (i64::try_from(shown).unwrap_or(i64::MAX) < page.total)
                    .then(|| t::trash_more(shown, page.total));
                view! {
                    <div class="trash-bar">
                        <span class="muted">{usage}</span>
                        <button
                            type="button"
                            class="danger"
                            on:click=move |_| confirming_empty.set(true)
                        >
                            {t::EMPTY_TRASH}
                        </button>
                    </div>
                    <Show when=move || confirming_empty.get()>
                        <div class="card confirm trash-confirm" role="alertdialog">
                            <p>{t::EMPTY_TRASH_CONFIRM}</p>
                            <button
                                type="button"
                                class="danger"
                                on:click=empty_trash
                                disabled=move || busy.get()
                            >
                                {t::EMPTY_TRASH}
                            </button>
                            <button
                                type="button"
                                class="secondary"
                                on:click=move |_| confirming_empty.set(false)
                            >
                                {t::CANCEL}
                            </button>
                        </div>
                    </Show>
                    <ul class="items">
                        {page
                            .entries
                            .into_iter()
                            .map(|entry| view! { <TrashRow entry notice on_change=reload /> })
                            .collect_view()}
                    </ul>
                    {more.map(|text| view! { <p class="muted small">{text}</p> })}
                }
                    .into_any()
            }
        }}
    }
}

#[component]
fn TrashRow(
    entry: TrashEntryInfo,
    notice: RwSignal<Option<String>>,
    on_change: Callback<()>,
) -> impl IntoView {
    let ctx = Ctx::get();
    let confirming = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);

    let kind_label = match (entry.kind, entry.collection_kind) {
        (TrashKind::Collection, CollectionKind::Calendar) => t::CALENDAR,
        (TrashKind::Collection, CollectionKind::Addressbook) => t::ADDRESS_BOOK,
        (TrashKind::Item, CollectionKind::Calendar) => t::TRASH_EVENT,
        (TrashKind::Item, CollectionKind::Addressbook) => t::TRASH_CONTACT,
    };
    let title = entry
        .summary
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| entry.name.clone())
        .unwrap_or_else(|| entry.collection.clone());
    let location = match entry.kind {
        TrashKind::Item => format!(
            "{kind_label} · {} · {}",
            entry.collection,
            entry.name.clone().unwrap_or_default()
        ),
        TrashKind::Collection => format!(
            "{kind_label} · {} · {}",
            entry.collection,
            t::trash_items(entry.items)
        ),
    };
    let dates = format!(
        "{} {} · {} {} · {}",
        t::TRASH_DELETED,
        format_timestamp(entry.deleted_at),
        t::TRASH_REMOVED,
        format_timestamp(entry.expires_at),
        format_size(entry.size)
    );
    let path = format!("/v1/trash/{}", entry.id);

    let restore_path = format!("{path}/restore");
    let restore = move |_| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        notice.set(None);
        let path = restore_path.clone();
        spawn_local(async move {
            let result = api::post_for::<RestoredTrashEntry>(&path).await;
            busy.try_set(false);
            match result {
                Ok(_) => {
                    notice.try_set(Some(t::TRASH_RESTORED.to_owned()));
                    on_change.run(());
                }
                Err(failure) => {
                    error.try_set(Some(ctx.fail(&failure)));
                }
            }
        });
    };

    let delete = move |_| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        notice.set(None);
        let path = path.clone();
        spawn_local(async move {
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
        <li class="item trash-item">
            <div class="item-head">
                <div class="item-title">
                    <strong>{title}</strong>
                    <span class="muted">{location}</span>
                </div>
                <div class="actions">
                    <button type="button" on:click=restore disabled=move || busy.get()>
                        {t::RESTORE}
                    </button>
                    <button type="button" class="danger" on:click=move |_| confirming.set(true)>
                        {t::DELETE_FOREVER}
                    </button>
                </div>
            </div>
            <p class="muted small">{dates}</p>
            <Show when=move || confirming.get()>
                <div class="confirm" role="alertdialog">
                    <p>{t::CONFIRM_DELETE_FOREVER}</p>
                    <button type="button" class="danger" on:click=delete.clone() disabled=move || busy.get()>
                        {t::DELETE_FOREVER}
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
