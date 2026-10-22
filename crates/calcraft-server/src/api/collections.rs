//! Calendars and address books of the logged-in user.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use calcraft_core::{
    CollectionKind, Slug,
    api::{CollectionInfo, CreateCollection, UpdateCollection},
};
use calcraft_store::{Collection, User};

use super::{ApiError, ApiJson, SessionUser};
use crate::{
    app::{AppState, DAV_ROOT},
    dav::calendar_order,
    validate::{MAX_DESCRIPTION, MAX_DISPLAY_NAME, clean_color, clean_text},
};

pub(super) fn info(collection: Collection, user: &User, order: Option<i64>) -> CollectionInfo {
    let home = match collection.kind {
        CollectionKind::Calendar => "calendars",
        CollectionKind::Addressbook => "addressbooks",
    };
    let components = match collection.kind {
        CollectionKind::Calendar => collection
            .components
            .split(',')
            .filter(|c| !c.is_empty())
            .map(str::to_owned)
            .collect(),
        CollectionKind::Addressbook => Vec::new(),
    };
    CollectionInfo {
        href: format!("{DAV_ROOT}{home}/{}/{}/", user.username, collection.slug),
        kind: collection.kind,
        slug: collection.slug.to_string(),
        display_name: collection.display_name,
        description: collection.description,
        color: collection.color,
        components,
        order,
        read_only: collection.read_only,
    }
}

/// Validates an optional text field: `None` keeps, `""` clears.
fn text_field(value: Option<&str>, max: usize) -> Result<Option<Option<String>>, ApiError> {
    value
        .map(|v| {
            clean_text(v, max)
                .map(|t| (!t.is_empty()).then_some(t))
                .ok_or(ApiError::BadRequest("invalid text"))
        })
        .transpose()
}

fn color_field(value: Option<&str>) -> Result<Option<Option<String>>, ApiError> {
    value
        .map(|v| {
            if v.trim().is_empty() {
                Ok(None)
            } else {
                clean_color(v)
                    .map(Some)
                    .ok_or(ApiError::BadRequest("color must be #RRGGBB or #RRGGBBAA"))
            }
        })
        .transpose()
}

pub async fn list(
    State(state): State<AppState>,
    session: SessionUser,
) -> Result<Json<Vec<CollectionInfo>>, ApiError> {
    let user = &session.user;
    let mut result = Vec::new();
    for kind in [CollectionKind::Calendar, CollectionKind::Addressbook] {
        let properties = state.store.dead_properties_of_owner(user.id, kind).await?;
        let mut infos: Vec<CollectionInfo> = state
            .store
            .collections(user.id, kind)
            .await?
            .into_iter()
            .map(|collection| {
                let id = collection.id;
                let order =
                    calendar_order(properties.iter().filter(|(c, _)| *c == id).map(|(_, p)| p));
                info(collection, user, order)
            })
            .collect();
        // Clients' order first (unordered ones last), then by name.
        infos.sort_by_cached_key(|info| {
            let name = info.display_name.as_deref().unwrap_or(&info.slug);
            (info.order.is_none(), info.order, name.to_lowercase())
        });
        result.extend(infos);
    }
    Ok(Json(result))
}

pub async fn create(
    State(state): State<AppState>,
    session: SessionUser,
    ApiJson(request): ApiJson<CreateCollection>,
) -> Result<(StatusCode, Json<CollectionInfo>), ApiError> {
    let user = &session.user;
    let slug: Slug = request
        .slug
        .parse()
        .map_err(|_| ApiError::BadRequest("invalid slug: use a-z, 0-9, '.', '_' or '-'"))?;
    let display_name = text_field(request.display_name.as_deref(), MAX_DISPLAY_NAME)?.flatten();
    let description = text_field(request.description.as_deref(), MAX_DESCRIPTION)?.flatten();
    let color = color_field(request.color.as_deref())?.flatten();
    let components = match request.kind {
        CollectionKind::Calendar => "VEVENT,VTODO",
        CollectionKind::Addressbook => "VCARD",
    };
    let collection = state
        .store
        .create_collection(
            user.id,
            request.kind,
            &slug,
            display_name.as_deref(),
            description.as_deref(),
            color.as_deref(),
            components,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(info(collection, user, None))))
}

/// Resolves a collection of the logged-in user from path segments. Unknown
/// and foreign collections are both "not found".
pub(super) async fn own_collection(
    state: &AppState,
    user: &User,
    kind: &str,
    slug: &str,
) -> Result<Collection, ApiError> {
    let kind: CollectionKind = kind.parse().map_err(|()| ApiError::NotFound)?;
    let slug: Slug = slug.parse().map_err(|_| ApiError::NotFound)?;
    state
        .store
        .collection(user.id, kind, &slug)
        .await?
        .ok_or(ApiError::NotFound)
}

pub async fn update(
    State(state): State<AppState>,
    session: SessionUser,
    Path((kind, slug)): Path<(String, String)>,
    ApiJson(request): ApiJson<UpdateCollection>,
) -> Result<StatusCode, ApiError> {
    let collection = own_collection(&state, &session.user, &kind, &slug).await?;
    // A subscription can be renamed and recoloured, nothing else.
    if collection.read_only && request.description.is_some() {
        return Err(ApiError::ReadOnly);
    }
    let display_name = text_field(request.display_name.as_deref(), MAX_DISPLAY_NAME)?;
    let description = text_field(request.description.as_deref(), MAX_DESCRIPTION)?;
    let color = color_field(request.color.as_deref())?;
    state
        .store
        .update_collection(
            collection.id,
            display_name.as_ref().map(Option::as_deref),
            description.as_ref().map(Option::as_deref),
            color.as_ref().map(Option::as_deref),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete(
    State(state): State<AppState>,
    session: SessionUser,
    Path((kind, slug)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let collection = own_collection(&state, &session.user, &kind, &slug).await?;
    state
        .store
        .delete_collection_to_trash(collection.id, crate::auth::now())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
