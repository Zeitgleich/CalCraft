//! Trash: restore deleted entries or delete them permanently.
//!
//! Summaries and names come from calendar and contact data and are only
//! rendered as text.

use calcraft_core::{
    CollectionKind,
    api::{CollectionInfo, RestoredTrashEntry, TrashEntryInfo, TrashKind, TrashList},
};
use calcraft_ui::{format_size, format_timestamp, text as t};
use leptos::{prelude::*, task::spawn_local};

use super::{Ctx, ErrorText, SuccessText, api};

/// Entries per request (the server's maximum).
const PAGE: usize = 200;

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
            match api::get::<TrashList>(&format!("/v1/trash?limit={PAGE}")).await {
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

    let load_more = move |_| {
        if busy.get_untracked() {
            return;
        }
        let offset = list.with_untracked(|l| l.as_ref().map_or(0, |l| l.entries.len()));
        busy.set(true);
        spawn_local(async move {
            let path = format!("/v1/trash?limit={PAGE}&offset={offset}");
            let result = api::get::<TrashList>(&path).await;
            busy.try_set(false);
            match result {
                Ok(page) => {
                    list.try_update(|list| {
                        if let Some(list) = list {
                            // Entries deleted meanwhile shift the pages;
                            // skip ones that are already shown.
                            let known: Vec<String> =
                                list.entries.iter().map(|e| e.id.clone()).collect();
                            list.entries.extend(
                                page.entries.into_iter().filter(|e| !known.contains(&e.id)),
                            );
                            list.total = page.total;
                            list.bytes = page.bytes;
                        }
                    });
                }
                Err(failure) => {
                    error.try_set(Some(ctx.fail(&failure)));
                }
            }
        });
    };

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
                    {more
                        .map(|text| {
                            view! {
                                <div class="trash-more">
                                    <span class="muted small">{text}</span>
                                    <button
                                        type="button"
                                        class="secondary"
                                        on:click=load_more
                                        disabled=move || busy.get()
                                    >
                                        {t::TRASH_LOAD_MORE}
                                    </button>
                                </div>
                            }
                        })}
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
    // Collections an object can be restored into after its own collection
    // was deleted permanently (`None` until needed).
    let targets = RwSignal::new(None::<Vec<CollectionInfo>>);
    let target = RwSignal::new(String::new());

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
    let collection_kind = entry.collection_kind;

    // Restores into the original collection, or into `into` (a slug).
    let restore_path = format!("{path}/restore");
    let run_restore = move |into: Option<String>| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        notice.set(None);
        error.set(None);
        let path = match &into {
            // Slugs are [a-z0-9._-]: safe in a query string.
            Some(slug) => format!("{restore_path}?collection={slug}"),
            None => restore_path.clone(),
        };
        spawn_local(async move {
            let result = api::post_for::<RestoredTrashEntry>(&path).await;
            busy.try_set(false);
            match result {
                Ok(_) => {
                    notice.try_set(Some(t::TRASH_RESTORED.to_owned()));
                    on_change.run(());
                }
                Err(failure) => {
                    if failure.code == "collection_deleted" {
                        load_targets(collection_kind, targets, target, error, ctx);
                    }
                    error.try_set(Some(ctx.fail(&failure)));
                }
            }
        });
    };
    let restore = {
        let run_restore = run_restore.clone();
        move |_| run_restore(None)
    };
    let restore_into = move |_| {
        let slug = target.get_untracked();
        if !slug.is_empty() {
            run_restore(Some(slug));
        }
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
            {move || {
                targets
                    .get()
                    .map(|list| {
                        if list.is_empty() {
                            view! { <p class="muted small">{t::TRASH_NO_TARGET}</p> }.into_any()
                        } else {
                            view! {
                                <div class="confirm trash-target">
                                    <select
                                        aria-label=t::TRASH_RESTORE_INTO
                                        on:change=move |ev| target.set(event_target_value(&ev))
                                    >
                                        {list
                                            .into_iter()
                                            .map(|info| {
                                                let label = info
                                                    .display_name
                                                    .clone()
                                                    .filter(|n| !n.is_empty())
                                                    .map_or_else(
                                                        || info.slug.clone(),
                                                        |n| format!("{n} ({})", info.slug),
                                                    );
                                                let selected = target.get_untracked() == info.slug;
                                                view! {
                                                    <option value=info.slug selected=selected>
                                                        {label}
                                                    </option>
                                                }
                                            })
                                            .collect_view()}
                                    </select>
                                    <button
                                        type="button"
                                        on:click=restore_into.clone()
                                        disabled=move || busy.get()
                                    >
                                        {t::TRASH_RESTORE_INTO}
                                    </button>
                                </div>
                            }
                                .into_any()
                        }
                    })
            }}
        </li>
    }
}

/// Loads the user's collections of `kind` as restore targets.
fn load_targets(
    kind: CollectionKind,
    targets: RwSignal<Option<Vec<CollectionInfo>>>,
    target: RwSignal<String>,
    error: RwSignal<Option<String>>,
    ctx: Ctx,
) {
    spawn_local(async move {
        match api::get::<Vec<CollectionInfo>>("/v1/collections").await {
            Ok(list) => {
                let list: Vec<CollectionInfo> =
                    list.into_iter().filter(|c| c.kind == kind).collect();
                if let Some(first) = list.first() {
                    target.try_set(first.slug.clone());
                }
                targets.try_set(Some(list));
            }
            Err(failure) => {
                error.try_set(Some(ctx.fail(&failure)));
            }
        }
    });
}
