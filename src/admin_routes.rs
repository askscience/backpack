//! Admin API handlers for managing spaces.
//!
//! Every handler in this module is gated by the admin-auth middleware
//! (applied as a route-layer in `main.rs`). The middleware verifies
//! the `ADMIN_TOKEN` via the `Authorization: Bearer` header before
//! any handler executes.
//!
//! These handlers are thin wrappers around `SpaceManager` methods —
//! they extract parameters from the HTTP request, delegate to the
//! space manager, and format the JSON response.

use axum::{
    extract::{Path, Query, State},
    Json,
};

use crate::admin::{CreateSpaceRequest, DeleteSpaceQuery, ShareSpaceRequest, UpdateSpaceRequest};
use crate::handlers::{ApiError, AppState};
use crate::spaces::DeleteMode;

/// `POST /api/admin/spaces`
///
/// Creates a new isolated space with the given label and optional
/// quota (in MB). Returns the `space_id`, `owner_token`, and other
/// metadata. The caller is responsible for distributing the
/// `owner_token` to the intended user.
pub async fn create_space(
    State(state): State<AppState>,
    Json(body): Json<CreateSpaceRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let quota_mb = body.quota_mb.unwrap_or(0);

    let created = state
        .spaces
        .create(&body.label, quota_mb)
        .await
        .map_err(|e| ApiError::new(format!("Failed to create space: {}", e)))?;

    // Serialize via serde_json::Value so we own the JSON formatting.
    let json = serde_json::to_value(&created)
        .map_err(|e| ApiError::new(format!("Serialization error: {}", e)))?;

    Ok(Json(json))
}

/// `GET /api/admin/spaces`
///
/// Lists all spaces managed by this server, including their labels,
/// quotas, usage, share counts, and status.
pub async fn list_spaces(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let entries = state
        .spaces
        .list()
        .await
        .map_err(|e| ApiError::new(format!("Failed to list spaces: {}", e)))?;

    let json = serde_json::to_value(&entries)
        .map_err(|e| ApiError::new(format!("Serialization error: {}", e)))?;

    Ok(Json(json))
}

/// `GET /api/admin/spaces/:id`
///
/// Returns full metadata for a single space: label, quota, usage,
/// status, associated share tokens, and archive links.
///
/// The `:id` path parameter is the space's UUID (from `POST .../spaces`
/// response or `GET .../spaces` listing).
pub async fn space_info(
    State(state): State<AppState>,
    Path(space_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // Look up the owner_token for this space so we can delegate to
    // the existing token-based `SpaceManager::info` method.
    let owner_token = state
        .spaces
        .find_owner_token(&space_id)
        .await
        .map_err(|_| ApiError::not_found(format!("Space not found: {}", space_id)))?;

    let info = state
        .spaces
        .info(&owner_token)
        .await
        .map_err(|e| ApiError::new(format!("Failed to get space info: {}", e)))?;

    let json = serde_json::to_value(&info)
        .map_err(|e| ApiError::new(format!("Serialization error: {}", e)))?;

    Ok(Json(json))
}

/// `DELETE /api/admin/spaces/:id?mode=purge|archive&for_share=<token>`
///
/// Deletes a space. Two modes:
///
/// * `mode=purge` — Permanently removes all files, the database,
///   and the space record. Irreversible.
/// * `mode=archive` — Freezes the space, creates a downloadable ZIP
///   of all files + database, and returns a 24-hour download token.
///   Optionally restrict the archive to a specific share token via
///   `for_share=X`.
pub async fn delete_space(
    State(state): State<AppState>,
    Path(space_id): Path<String>,
    Query(query): Query<DeleteSpaceQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mode = match query.mode.as_str() {
        "purge" => DeleteMode::Purge,
        "archive" => DeleteMode::Archive {
            for_share: query.for_share,
        },
        other => {
            return Err(ApiError::bad_request(format!(
                "Unknown delete mode '{}'. Use 'purge' or 'archive'.",
                other
            )));
        }
    };

    // Resolve the space by its owner_token so we can delegate to
    // the existing `SpaceManager::delete` method.
    let owner_token = state
        .spaces
        .find_owner_token(&space_id)
        .await
        .map_err(|_| ApiError::not_found(format!("Space not found: {}", space_id)))?;

    let result = state
        .spaces
        .delete(&owner_token, mode)
        .await
        .map_err(|e| ApiError::new(format!("Failed to delete space: {}", e)))?;

    let json = serde_json::to_value(&result)
        .map_err(|e| ApiError::new(format!("Serialization error: {}", e)))?;

    Ok(Json(json))
}

/// `POST /api/admin/spaces/:id/share`
///
/// Creates a new share token for an existing space. The token can be
/// distributed to another user so they can access the same files.
/// Sharing does not increase or change the space's quota.
pub async fn share_space(
    State(state): State<AppState>,
    Path(space_id): Path<String>,
    Json(body): Json<ShareSpaceRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let owner_token = state
        .spaces
        .find_owner_token(&space_id)
        .await
        .map_err(|_| ApiError::not_found(format!("Space not found: {}", space_id)))?;

    let share = state
        .spaces
        .share(&owner_token, &body.label)
        .await
        .map_err(|e| ApiError::forbidden(format!("Share failed: {}", e)))?;

    let json = serde_json::to_value(&share)
        .map_err(|e| ApiError::new(format!("Serialization error: {}", e)))?;

    Ok(Json(json))
}

/// `POST /api/admin/spaces/:id/shares/:share_token/revoke`
///
/// Revokes a share token — hard-deletes it from the database.
/// After revocation the share token no longer resolves for API
/// access. All WebSocket sync clients connected via this space
/// receive a "revoked" event and disconnect.
pub async fn revoke_share_admin(
    State(state): State<AppState>,
    Path((space_id, share_token)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let owner_token = state
        .spaces
        .find_owner_token(&space_id)
        .await
        .map_err(|_| ApiError::not_found(format!("Space not found: {}", space_id)))?;

    let revoked_space_id = state
        .spaces
        .revoke_share(&owner_token, &share_token)
        .await
        .map_err(|e| ApiError::forbidden(format!("Revoke failed: {}", e)))?;

    // Notify connected WebSocket clients and invalidate sync tokens.
    state.sync_hub.broadcast_revoked(&revoked_space_id).await;
    state.sync_hub.revoke_space_tokens(&revoked_space_id).await;

    Ok(Json(serde_json::json!({
        "revoked": true,
        "space_id": revoked_space_id,
        "share_token": share_token,
    })))
}

/// `PUT /api/admin/spaces/:id`
///
/// Updates a space's label and/or quota. Both fields are optional —
/// only the provided fields are updated. Returns the updated `SpaceEntry`.
pub async fn update_space(
    State(state): State<AppState>,
    Path(space_id): Path<String>,
    Json(body): Json<UpdateSpaceRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let entry = state
        .spaces
        .update_space(&space_id, body.label.as_deref(), body.quota_mb)
        .await
        .map_err(|e| ApiError::new(format!("Failed to update space: {}", e)))?;

    let json = serde_json::to_value(&entry)
        .map_err(|e| ApiError::new(format!("Serialization error: {}", e)))?;

    Ok(Json(json))
}

/// `POST /api/admin/spaces/:id/regenerate-token`
///
/// Rotates the owner_token for a space. The old token is immediately
/// invalidated. Returns `{ "new_token": "..." }`.
pub async fn regenerate_token(
    State(state): State<AppState>,
    Path(space_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let new_token = state
        .spaces
        .regenerate_owner_token(&space_id)
        .await
        .map_err(|e| ApiError::new(format!("Failed to regenerate token: {}", e)))?;

    Ok(Json(serde_json::json!({
        "new_token": new_token,
        "space_id": space_id,
    })))
}

/// `POST /api/admin/spaces/:id/reactivate`
///
/// Reactivates a frozen space, changing its status back to `active`.
pub async fn reactivate_space(
    State(state): State<AppState>,
    Path(space_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state
        .spaces
        .reactivate_space(&space_id)
        .await
        .map_err(|e| ApiError::new(format!("Failed to reactivate space: {}", e)))?;

    Ok(Json(serde_json::json!({
        "reactivated": true,
        "space_id": space_id,
    })))
}

/// `GET /api/admin/shares`
///
/// Lists all share tokens across all spaces.
pub async fn list_all_shares(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let shares = state
        .spaces
        .list_all_shares()
        .await
        .map_err(|e| ApiError::new(format!("Failed to list shares: {}", e)))?;

    let json = serde_json::to_value(&shares)
        .map_err(|e| ApiError::new(format!("Serialization error: {}", e)))?;

    Ok(Json(json))
}

/// `DELETE /api/admin/shares/:share_token`
///
/// Revokes a share token directly by its token value. No space ID
/// lookup required.
pub async fn delete_share(
    State(state): State<AppState>,
    Path(share_token): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let space_id = state
        .spaces
        .delete_share_by_token(&share_token)
        .await
        .map_err(|e| ApiError::not_found(format!("Failed to delete share: {}", e)))?;

    state.sync_hub.broadcast_revoked(&space_id).await;
    state.sync_hub.revoke_space_tokens(&space_id).await;

    Ok(Json(serde_json::json!({
        "revoked": true,
        "space_id": space_id,
        "share_token": share_token,
    })))
}

/// `GET /api/admin/archives`
///
/// Lists all space archives with their expiry info and download status.
pub async fn list_archives(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let archives = state
        .spaces
        .list_all_archives()
        .await
        .map_err(|e| ApiError::new(format!("Failed to list archives: {}", e)))?;

    let json = serde_json::to_value(&archives)
        .map_err(|e| ApiError::new(format!("Serialization error: {}", e)))?;

    Ok(Json(json))
}
