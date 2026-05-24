use std::sync::Arc;

use axum::{
    extract::{ws, WebSocketUpgrade, Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use axum_extra::extract::Multipart;
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::fs;
use tracing::{debug, error, info, warn};

use crate::catalog;
use crate::config::Config;
use crate::db::{self, FileRecord, f32_vec_to_bytes};
use crate::spaces::{SpaceHandle, SpaceManager};
use crate::sync_hub::{SyncHub, SyncEvent};
use crate::vector;
use crate::webauthn::{self, WebauthnApp};

#[derive(Clone)]
pub struct AppState {
    #[allow(dead_code)]
    pub pool: SqlitePool,
    pub config: Arc<Config>,
    pub client: Client,
    pub spaces: Arc<SpaceManager>,
    pub sync_hub: Arc<SyncHub>,
    pub webauthn: Arc<WebauthnApp>,
    pub rate_limiter: Arc<RateLimiter>,
}

#[derive(Debug, Serialize)]
pub struct ApiError {
    error: String,
    pub status: u16,
}

impl ApiError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self { error: msg.into(), status: 500 }
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Self { error: msg.into(), status: 404 }
    }

    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self { error: msg.into(), status: 403 }
    }

    pub fn quota(msg: impl Into<String>) -> Self {
        Self { error: msg.into(), status: 413 }
    }

    pub fn gone(msg: impl Into<String>) -> Self {
        Self { error: msg.into(), status: 410 }
    }

    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self { error: msg.into(), status: 400 }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let code = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let body = serde_json::json!({"error": self.error});
        (code, Json(body)).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        error!("{:?}", e);
        ApiError::new("Internal server error")
    }
}

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

pub struct RateLimiter {
    windows: Mutex<HashMap<String, Vec<Instant>>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self { windows: Mutex::new(HashMap::new()) }
    }

    pub fn check(&self, key: &str, max_per_min: usize) -> bool {
        let mut windows = self.windows.lock().unwrap();
        let now = Instant::now();
        let entries = windows.entry(key.to_string()).or_default();
        entries.retain(|t| now.duration_since(*t).as_secs() < 60);
        let allowed = entries.len() < max_per_min;
        if allowed {
            entries.push(now);
        }
        allowed
    }
}

#[derive(Deserialize)]
pub struct TokenQuery {
    pub token: Option<String>,
}

pub fn extract_bearer_token(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_string())
}

pub async fn resolve_space(state: &AppState, token: Option<String>) -> Result<SpaceHandle, ApiError> {
    state
        .spaces
        .resolve(token.as_deref())
        .await
        .map_err(|e| ApiError::forbidden(format!("Invalid space token: {}", e)))
}

#[derive(Serialize)]
pub struct UploadResponse {
    pub id: String,
    pub original_name: String,
    pub mime: String,
    pub file_size: i64,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub tags: Option<String>,
    pub category: Option<String>,
    pub extracted_text_length: usize,
    pub created_at: String,
}

#[derive(Serialize)]
pub struct BatchUploadResponse {
    pub total_files: usize,
    pub results: Vec<UploadResponse>,
}

#[derive(Serialize)]
pub struct InventoryResponse {
    pub total_files: usize,
    pub total_size: i64,
    pub categories: Vec<db::CategoryGroup>,
}

#[derive(Serialize)]
pub struct SearchResponse {
    pub query: String,
    pub results: Vec<db::SearchResult>,
}

#[derive(Deserialize)]
pub struct SearchQuery {
    pub q: String,
    pub token: Option<String>,
}

#[derive(Deserialize)]
pub struct AskRequest {
    pub question: String,
}

#[derive(Serialize)]
pub struct AskResponse {
    pub answer: String,
    pub sources: Vec<SourceInfo>,
}

#[derive(Serialize)]
pub struct SourceInfo {
    pub id: String,
    pub original_name: String,
    pub mime: String,
    pub file_size: i64,
    pub title: Option<String>,
    pub summary: Option<String>,
}

pub async fn upload_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    mut multipart: Multipart,
) -> Result<Json<BatchUploadResponse>, ApiError> {
    let token = extract_bearer_token(&headers);
    let space = resolve_space(&state, token).await?;

    if space.read_only() {
        return Err(ApiError::forbidden("Read-only share token cannot upload files"));
    }

    let mut files: Vec<(String, Vec<u8>, Option<String>)> = Vec::new();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::new(format!("Multipart error: {}", e)))?
    {
        let name = field.name().unwrap_or("file").to_string();
        if name == "file" || name.is_empty() {
            let file_name = field.file_name().unwrap_or("unnamed").to_string();
            if file_name.len() > 512 {
                return Err(ApiError::bad_request(format!("Filename too long (max 512): {}", &file_name[..128.min(file_name.len())])));
            }
            if is_blocked_extension(&file_name) {
                return Err(ApiError::forbidden(format!("File type not allowed: {}", file_name)));
            }
            let content_type = field.content_type().map(|c| c.to_string());
            let data = field
                .bytes()
                .await
                .map_err(|e| ApiError::new(format!("Failed to read file bytes: {}", e)))?
                .to_vec();
            files.push((file_name, data, content_type));
        }
    }

    if files.is_empty() {
        return Err(ApiError::new("No file provided"));
    }

    let total_bytes: u64 = files.iter().map(|(_, d, _)| d.len() as u64).sum();
    if !state
        .spaces
        .check_quota(&space.space_id, total_bytes)
        .await
        .map_err(|e| ApiError::new(e.to_string()))?
    {
        return Err(ApiError::quota("Quota exceeded"));
    }

    let siblings: Vec<String> = files.iter().map(|(n, _, _)| n.clone()).collect();

    let mut results = Vec::with_capacity(files.len());
    for (original_name, data, content_type) in files {
        let others: Vec<&str> =
            siblings.iter().filter(|&s| *s != original_name).map(|s| s.as_str()).collect();
        match process_single_file(&state, &space, original_name, data, content_type, &others).await {
            Ok(response) => results.push(response),
            Err(e) => warn!("Failed to process file: {:?}", e),
        }
    }

    let total_saved: u64 = results.iter().map(|r| r.file_size as u64).sum();
    let _ = state.spaces.add_usage(&space.space_id, total_saved).await;

    Ok(Json(BatchUploadResponse {
        total_files: results.len(),
        results,
    }))
}

async fn process_single_file(
    state: &AppState,
    space: &SpaceHandle,
    original_name: String,
    file_data: Vec<u8>,
    content_type: Option<String>,
    siblings: &[&str],
) -> Result<UploadResponse, ApiError> {
    let mime = content_type.unwrap_or_else(|| {
        mime_guess::from_path(&original_name)
            .first_or_octet_stream()
            .to_string()
    });

    let max_bytes = state.config.max_file_size_mb * 1024 * 1024;
    if file_data.len() as u64 > max_bytes {
        return Err(ApiError::new(format!(
            "File '{}' too large. Max size: {} MB",
            original_name, state.config.max_file_size_mb
        )));
    }

    let id = uuid::Uuid::new_v4().to_string();
    let safe_name = sanitize_filename(&original_name);
    let file_dir = format!("{}/{}", space.upload_dir, id);
    fs::create_dir_all(&file_dir)
        .await
        .map_err(|e| ApiError::new(format!("Failed to create upload dir: {}", e)))?;

    let file_path = format!("{}/{}", file_dir, &safe_name);
    let file_size = file_data.len() as i64;

    fs::write(&file_path, &file_data)
        .await
        .map_err(|e| ApiError::new(format!("Failed to write file: {}", e)))?;

    info!(
        "File saved: id={}, name={}, size={}",
        id, original_name, file_size
    );

    let extracted_text = crate::extraction::extract_text(&file_path, &mime, &original_name)
        .await
        .unwrap_or_else(|e| {
            warn!("Text extraction failed for '{}': {}", original_name, e);
            String::new()
        });

    let extracted_len = extracted_text.len();
    info!("Extracted {} chars from {}", extracted_len, original_name);

    let catalog_text = if siblings.is_empty() {
        extracted_text.clone()
    } else {
        format!(
            "[Batch upload. Sibling files in this folder: {}]\n\n{}",
            siblings.join(", "),
            extracted_text
        )
    };

    let (title, summary, tags, category, catalog_json, embedding) =
        if !extracted_text.trim().is_empty() {
            match catalog::catalog_file(&state.client, &state.config, &catalog_text).await {
                Ok(entry) => {
                    let tags_str = entry.tags.join(", ");
                    let catalog_str = serde_json::to_string(&entry).unwrap_or_default();

                    let embed_text =
                        format!("{} — {} — {}", entry.title, entry.summary, tags_str);
                    let emb_result =
                        catalog::get_embedding(&state.client, &state.config, &embed_text).await;

                    let title_val = entry.title;
                    let summary_val = entry.summary;
                    let category_val = entry.category;

                    match emb_result {
                        Ok(vec) => {
                            info!("Cataloged and embedded: {}", title_val);
                            (
                                Some(title_val),
                                Some(summary_val),
                                Some(tags_str),
                                Some(category_val),
                                Some(catalog_str),
                                Some(f32_vec_to_bytes(&vec)),
                            )
                        }
                        Err(e) => {
                            warn!("Embedding failed for '{}': {}", original_name, e);
                            (
                                Some(title_val),
                                Some(summary_val),
                                Some(tags_str),
                                Some(category_val),
                                Some(catalog_str),
                                None,
                            )
                        }
                    }
                }
                Err(e) => {
                    warn!("Cataloging failed for '{}': {}", original_name, e);
                    (None, None, None, None, None, None)
                }
            }
        } else {
            info!("No text extracted from '{}', skipping cataloging", original_name);
            (None, None, None, None, None, None)
        };

    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let record = FileRecord {
        id: id.clone(),
        original_name: original_name.clone(),
        mime: mime.clone(),
        file_path: file_path.clone(),
        file_size,
        extracted_text,
        title: title.clone(),
        summary: summary.clone(),
        tags: tags.clone(),
        category: category.clone(),
        catalog_json,
        embedding,
        created_at: now.clone(),
    };

    db::insert_file(&space.pool, &record)
        .await
        .map_err(|e| ApiError::new(format!("Failed to save file record: {}", e)))?;

    // Broadcast the file creation to any WebSocket sync clients
    // connected to this space. Only fires for shared spaces.
    {
        let space_id = space.space_id.clone();
        let hub = state.sync_hub.clone();
        let spaces = state.spaces.clone();
        let record_clone = record.clone();

        tokio::spawn(async move {
            let shared = spaces
                .is_shared(&space_id)
                .await
                .unwrap_or(false);

            hub.broadcast(
                &space_id,
                shared,
                SyncEvent {
                    typ: "created".into(),
                    file_id: record_clone.id,
                    original_name: record_clone.original_name,
                    file_size: record_clone.file_size,
                    timestamp: record_clone.created_at,
                },
            )
            .await;
        });
    }

    Ok(UploadResponse {
        id,
        original_name,
        mime,
        file_size,
        title: title.filter(|t| !t.is_empty()),
        summary: summary.filter(|s| !s.is_empty()),
        tags: tags.filter(|t| !t.is_empty()),
        category: category.filter(|c| !c.is_empty()),
        extracted_text_length: extracted_len,
        created_at: now,
    })
}

fn sanitize_filename(name: &str) -> String {
    std::path::Path::new(name)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("unnamed")
        .to_string()
}

pub async fn search_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(query): Query<SearchQuery>,
) -> Result<Json<SearchResponse>, ApiError> {
    let token = extract_bearer_token(&headers).or(query.token);
    let space = resolve_space(&state, token).await?;

    if query.q.trim().is_empty() {
        return Ok(Json(SearchResponse {
            query: query.q,
            results: vec![],
        }));
    }

    let embedding = catalog::get_embedding(&state.client, &state.config, &query.q)
        .await
        .map_err(|e| ApiError::new(format!("Embedding failed: {}", e)))?;

    let results = vector::search_similar(&space.pool, &embedding, 10)
        .await
        .map_err(|e| ApiError::new(format!("Search failed: {}", e)))?;

    Ok(Json(SearchResponse {
        query: query.q,
        results,
    }))
}

pub async fn inventory_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<InventoryResponse>, ApiError> {
    let token = extract_bearer_token(&headers);
    let space = resolve_space(&state, token).await?;

    let categories = db::list_files_grouped(&space.pool)
        .await
        .map_err(|e| ApiError::new(format!("Failed to list files: {}", e)))?;

    let total_files: usize = categories.iter().map(|g| g.count).sum();
    let total_size: i64 = categories.iter().flat_map(|g| g.files.iter()).map(|f| f.file_size).sum();

    Ok(Json(InventoryResponse {
        total_files,
        total_size,
        categories,
    }))
}

pub async fn download_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<std::collections::HashMap<String, String>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let token = extract_bearer_token(&headers)
        .or_else(|| params.get("token").cloned());
    let space = resolve_space(&state, token).await?;

    let file = db::get_file(&space.pool, &id)
        .await
        .map_err(|e| ApiError::new(format!("DB error: {}", e)))?
        .ok_or_else(|| ApiError::not_found("File not found"))?;

    if !std::path::Path::new(&file.file_path).exists() {
        return Err(ApiError::not_found("File not found on disk"));
    }

    async {
        let data = tokio::fs::read(&file.file_path)
            .await
            .map_err(|e| ApiError::new(format!("Failed to read file: {}", e)))?;

        let mime: mime_guess::Mime = file
            .mime
            .parse()
            .unwrap_or(mime_guess::mime::APPLICATION_OCTET_STREAM);

        let filename = file.original_name.clone();
        let headers = [
            (header::CONTENT_TYPE, mime.to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", filename),
            ),
        ];

        Ok::<_, ApiError>((headers, data))
    }
    .await
}

pub async fn inline_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let token = extract_bearer_token(&headers);
    let space = resolve_space(&state, token).await?;

    let file = db::get_file(&space.pool, &id)
        .await
        .map_err(|e| ApiError::new(format!("DB error: {}", e)))?
        .ok_or_else(|| ApiError::not_found("File not found"))?;

    if !std::path::Path::new(&file.file_path).exists() {
        return Err(ApiError::not_found("File not found on disk"));
    }

    let data = tokio::fs::read(&file.file_path)
        .await
        .map_err(|e| ApiError::new(format!("Failed to read file: {}", e)))?;

    let mime_str = file.mime.clone();
    let headers = [
        (header::CONTENT_TYPE, mime_str),
        (
            header::CONTENT_DISPOSITION,
            format!("inline; filename=\"{}\"", file.original_name),
        ),
    ];

    Ok::<_, ApiError>((headers, data))
}

pub async fn delete_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let token = extract_bearer_token(&headers);
    let space = resolve_space(&state, token).await?;

    if space.read_only() {
        return Err(ApiError::forbidden("Read-only share token cannot delete files"));
    }

    let file = db::get_file(&space.pool, &id)
        .await
        .map_err(|e| ApiError::new(format!("DB error: {}", e)))?
        .ok_or_else(|| ApiError::not_found("File not found"))?;

    let file_size = file.file_size;

    let file_dir = std::path::Path::new(&file.file_path)
        .parent()
        .map(|p| p.to_path_buf());

    if let Some(dir) = file_dir {
        if dir.exists() {
            let _ = tokio::fs::remove_dir_all(&dir).await;
        }
    }

    db::delete_file(&space.pool, &id)
        .await
        .map_err(|e| ApiError::new(format!("Failed to delete file record: {}", e)))?;

    let _ = state.spaces.add_usage(&space.space_id, file_size.max(0) as u64).await;

    info!("Deleted file: id={}, name={}", id, file.original_name);

    // Broadcast the deletion to any WebSocket sync clients.
    {
        let space_id = space.space_id.clone();
        let hub = state.sync_hub.clone();
        let spaces = state.spaces.clone();
        let file_id = id.clone();
        let original_name = file.original_name.clone();

        tokio::spawn(async move {
            let shared = spaces
                .is_shared(&space_id)
                .await
                .unwrap_or(false);

            hub.broadcast(
                &space_id,
                shared,
                SyncEvent {
                    typ: "deleted".into(),
                    file_id,
                    original_name,
                    file_size: file_size,
                    timestamp: chrono::Utc::now()
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string(),
                },
            )
            .await;
        });
    }

    Ok(Json(serde_json::json!({
        "deleted": true,
        "id": id
    })))
}

pub async fn ask_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<AskRequest>,
) -> Result<Json<AskResponse>, ApiError> {
    let token = extract_bearer_token(&headers);
    let space = resolve_space(&state, token).await?;

    if body.question.trim().is_empty() {
        return Err(ApiError::bad_request("Question cannot be empty"));
    }

    let query_embedding = catalog::get_embedding(&state.client, &state.config, &body.question)
        .await
        .map_err(|e| ApiError::new(format!("Embedding failed: {}", e)))?;

    let search_results =
        vector::search_similar(&space.pool, &query_embedding, 5).await?;

    let sources: Vec<SourceInfo> = search_results
        .iter()
        .map(|r| SourceInfo {
            id: r.id.clone(),
            original_name: r.original_name.clone(),
            mime: r.mime.clone(),
            file_size: r.file_size,
            title: r.title.clone(),
            summary: r.summary.clone(),
        })
        .collect();

    let ids: Vec<String> = search_results.iter().map(|r| r.id.clone()).collect();
    let full_texts = db::get_full_texts(&space.pool, &ids)
        .await
        .map_err(|e| ApiError::new(format!("Failed to fetch file texts: {}", e)))?;

    let context = build_rag_context(&full_texts);

    let answer = catalog::call_chat_with_context(
        &state.client,
        &state.config,
        &context,
        &body.question,
    )
    .await
    .map_err(|e| ApiError::new(format!("LLM query failed: {}", e)))?;

    Ok(Json(AskResponse { answer, sources }))
}

// ── Sync / WebSocket endpoints ─────────────────────────────────────

/// Query parameters for the sync-token endpoint.
#[derive(Deserialize)]
pub struct SyncTokenQuery {
    pub token: Option<String>,
    /// Optional label to identify this client (e.g. "laptop", "desktop").
    #[serde(default = "default_label")]
    pub label: String,
}

fn default_label() -> String {
    "sync-client".into()
}

/// `POST /sync-token?token=<space_token>&label=<optional>`
///
/// Requests a time-limited sync ticket for the WebSocket endpoint.
/// Only succeeds for shared spaces — private/single-user spaces
/// receive a `404 Not Found` since there's no team to sync with.
pub async fn sync_token_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(query): Query<SyncTokenQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let token = extract_bearer_token(&headers).or(query.token);
    let space = resolve_space(&state, token).await?;

    let shared = state
        .spaces
        .is_shared(&space.space_id)
        .await
        .map_err(|e| ApiError::new(format!("Failed to check space status: {}", e)))?;

    if !shared {
        return Err(ApiError::not_found(
            "Space is not shared. Real-time sync is only available for shared/team spaces.",
        ));
    }

    match state.sync_hub.issue_sync_token(&space.space_id, shared, &query.label).await {
        Some(sync_token) => Ok(Json(serde_json::json!({
            "sync_token": sync_token,
            "space_id": space.space_id,
            "expires_in_secs": 86400,
            "ws_endpoint": format!("/ws?sync_token={}", sync_token),
        }))),
        None => Err(ApiError::new("Failed to issue sync token")),
    }
}

/// Request body for the revoke-share endpoint.
#[derive(Deserialize)]
pub struct RevokeShareRequest {
    /// Owner token of the space (validates ownership).
    pub owner_token: String,
    /// Share token to revoke.
    pub share_token: String,
}

/// `POST /space/revoke-share`
///
/// Revokes a share invitation by hard-deleting the share token.
/// Only the space owner may revoke shares. After revocation:
/// - The share token no longer resolves for API access
/// - All WebSocket sync clients for this space receive a "revoked"
///   event and disconnect, falling back to poll-only mode
/// - All in-memory sync tickets for this space are invalidated
pub async fn revoke_share_handler(
    State(state): State<AppState>,
    Json(body): Json<RevokeShareRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if body.owner_token.is_empty() || body.share_token.is_empty() {
        return Err(ApiError::bad_request(
            "Both owner_token and share_token are required",
        ));
    }

    // Validate that the owner_token is valid (resolves to a space).
    // This also ensures the space is active.
    let _space = resolve_space(&state, Some(body.owner_token.clone())).await?;

    // Hard-delete the share token. SpaceManager validates ownership
    // by checking that the owner_token matches the space's owner.
    let space_id = state
        .spaces
        .revoke_share(&body.owner_token, &body.share_token)
        .await
        .map_err(|e| ApiError::forbidden(format!("Revoke failed: {}", e)))?;

    // The space_id from revoke_share should match the resolved one.
    // Notify any connected WebSocket clients that they've been revoked.
    state.sync_hub.broadcast_revoked(&space_id).await;

    // Invalidate all in-memory sync tokens for this space so clients
    // cannot reconnect via WebSocket.
    state.sync_hub.revoke_space_tokens(&space_id).await;

    info!(
        "Share revoked: space={}, share_token={}",
        space_id,
        &body.share_token[..12.min(body.share_token.len())]
    );

    Ok(Json(serde_json::json!({
        "revoked": true,
        "space_id": space_id,
        "share_token": body.share_token,
    })))
}

/// Query parameters for the WebSocket endpoint.
#[derive(Deserialize)]
pub struct WsQuery {
    pub sync_token: Option<String>,
}

/// `GET /ws?sync_token=<sync_token>`
///
/// Upgrades the connection to a WebSocket and streams `SyncEvent`
/// JSON frames in real-time as files are added, modified, or deleted
/// in the space.
pub async fn ws_handler(
    State(state): State<AppState>,
    Query(query): Query<WsQuery>,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, ApiError> {
    let sync_token = query
        .sync_token
        .ok_or_else(|| ApiError::bad_request("Missing sync_token parameter"))?;

    let space_id = state
        .sync_hub
        .validate_sync_token(&sync_token)
        .await
        .ok_or_else(|| ApiError::forbidden("Invalid or expired sync token"))?;

    info!("WebSocket sync connection for space={}", space_id);

    Ok(ws.on_upgrade(move |socket| handle_socket(socket, space_id, state.sync_hub)))
}

/// Bridge between the raw WebSocket and the broadcast channel.
/// Receives events from the hub and writes them to the client.
async fn handle_socket(socket: ws::WebSocket, space_id: String, hub: Arc<SyncHub>) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let mut broadcast_rx = hub.subscribe(&space_id).await;
    let space_id_debug = space_id.clone();

    // Spawn a task that reads broadcast events and writes to WebSocket.
    let forward_handle = tokio::spawn(async move {
        loop {
            match broadcast_rx.recv().await {
                Ok(event) => {
                    let json = serde_json::to_string(&event).unwrap_or_default();
                    if ws_tx
                        .send(ws::Message::Text(json.into()))
                        .await
                        .is_err()
                    {
                        break; // WebSocket closed.
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(
                        "WS client lagged behind broadcast, dropped {} events",
                        skipped
                    );
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    debug!("Broadcast channel closed for space={}", space_id_debug);
                    break;
                }
            }
        }
    });

    // Read loop: consume any incoming client messages (ping/pong/close).
    let read_handle = tokio::spawn(async move {
        while let Some(msg) = ws_rx.next().await {
            match msg {
                Ok(m) if matches!(m, ws::Message::Close(_)) => break,
                Err(_) => break,
                _ => {} // Text/Binary/Ping/Pong — keep connection alive.
            }
        }
    });

    tokio::select! {
        _ = forward_handle => {},
        _ = read_handle => {},
    }

    debug!("WebSocket client disconnected from space={}", space_id);
}

fn build_rag_context(files: &[db::FileRecord]) -> String {
    let mut ctx = String::new();
    for (i, file) in files.iter().enumerate() {
        ctx.push_str(&format!("[File {}] {}\n", i + 1, file.original_name));
        if let Some(ref title) = file.title {
            ctx.push_str(&format!("Title: {}\n", title));
        }
        if let Some(ref summary) = file.summary {
            ctx.push_str(&format!("Summary: {}\n", summary));
        }
        if let Some(ref tags) = file.tags {
            ctx.push_str(&format!("Tags: {}\n", tags));
        }

        let text = &file.extracted_text;
        let max_text_len = 3000usize;
        let display_text = if text.len() > max_text_len {
            format!(
                "{}... [truncated, full text {} chars]",
                &text[..max_text_len],
                text.len()
            )
        } else {
            text.clone()
        };
        ctx.push_str(&format!("Content:\n{}\n\n", display_text));
    }
    ctx
}

pub async fn archive_download_handler(
    State(state): State<AppState>,
    Path(download_token): Path<String>,
    Query(TokenQuery { token }): Query<TokenQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let (zip_path, expires_at, required_token) = state
        .spaces
        .get_archive_info(&download_token)
        .await
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("already downloaded") {
                ApiError::gone(msg)
            } else {
                ApiError::new(msg)
            }
        })?
        .ok_or_else(|| ApiError::not_found("Archive not found"))?;

    if !required_token.is_empty() {
        let provided = token.unwrap_or_default();
        if provided != required_token {
            return Err(ApiError::forbidden("Access denied for this archive"));
        }
    }

    let expiry = chrono::NaiveDateTime::parse_from_str(&expires_at, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|t| t.and_utc().timestamp());
    let now = chrono::Utc::now().timestamp();
    if let Some(exp) = expiry {
        if now > exp {
            return Err(ApiError::forbidden("Archive download link expired"));
        }
    }

    let data = tokio::fs::read(&zip_path)
        .await
        .map_err(|e| ApiError::new(format!("Failed to read archive: {}", e)))?;

    let filename = format!("{}.zip", download_token);
    let headers = [
        (header::CONTENT_TYPE, "application/zip".to_string()),
        (
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", filename),
        ),
    ];

    let _ = state.spaces.mark_archive_downloaded(&download_token).await;

    Ok((headers, data))
}

// ── WebAuthn ─────────────────────────────────────────────────────

pub async fn webauthn_register_start(
    State(state): State<AppState>,
    Json(body): Json<webauthn::WebauthnRegisterStartRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user_id = resolve_user_from_token(&state, &body.token).await?;
    let (ccr, challenge_id) = state
        .webauthn
        .start_registration(&user_id, &user_id)
        .map_err(|e| ApiError::new(format!("Registration failed: {}", e)))?;

    Ok(Json(serde_json::json!({
        "challenge_id": challenge_id,
        "public_key": ccr,
    })))
}

pub async fn webauthn_register_finish(
    State(state): State<AppState>,
    Json(body): Json<webauthn::WebauthnRegisterFinishRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user_id = {
        state.webauthn.challenges.lock().unwrap()
            .get(&body.challenge_id)
            .map(|(_, uid, _)| uid.clone())
            .ok_or_else(|| ApiError::bad_request("Session lost — please try again"))?
    };

    let passkey = state
        .webauthn
        .finish_registration(&body.challenge_id, &body.credential)
        .map_err(|e| ApiError::bad_request(format!("Registration failed: {}", e)))?;

    let pool = &state.pool;
    webauthn::store_passkey(pool, &user_id, &passkey)
        .await
        .map_err(|e| ApiError::new(format!("Storage failed: {}", e)))?;

    let role = if user_id == "admin" { "admin" } else { "user" };
    let session = webauthn::create_session(pool, &user_id, role)
        .await
        .map_err(|e| ApiError::new(format!("Session failed: {}", e)))?;

    // Resolve space token for non-admin users so the web UI can use it for API calls.
    let space_token = if user_id != "admin" {
        state.spaces.find_owner_token(&user_id).await.ok()
    } else {
        None
    };

    Ok(Json(serde_json::json!({
        "session_token": session,
        "user_id": user_id,
        "role": role,
        "space_token": space_token,
    })))
}

pub async fn webauthn_auth_start(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (rcr, challenge_id) = state
        .webauthn
        .start_authentication()
        .map_err(|e| ApiError::new(format!("Auth failed: {}", e)))?;

    Ok(Json(serde_json::json!({
        "challenge_id": challenge_id,
        "public_key": rcr,
    })))
}

pub async fn webauthn_auth_finish(
    State(state): State<AppState>,
    Json(body): Json<webauthn::WebauthnAuthFinishRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result = state
        .webauthn
        .finish_authentication(&body.challenge_id, &body.credential)
        .map_err(|e| ApiError::forbidden(format!("Auth failed: {}", e)))?;

    let user_id = webauthn::find_user_by_credential(&state.pool, result.cred_id())
        .await
        .map_err(|e| ApiError::new(format!("Lookup failed: {}", e)))?
        .unwrap_or_default();

    let role = if user_id == "admin" { "admin" } else { "user" };
    let session = webauthn::create_session(&state.pool, &user_id, role)
        .await
        .map_err(|e| ApiError::new(format!("Session failed: {}", e)))?;

    // Resolve space token for non-admin users so the web UI can use it for API calls.
    let space_token = if user_id != "admin" && !user_id.is_empty() {
        state.spaces.find_owner_token(&user_id).await.ok()
    } else {
        None
    };

    Ok(Json(serde_json::json!({
        "session_token": session,
        "user_id": user_id,
        "role": role,
        "space_token": space_token,
    })))
}

pub async fn webauthn_whoami(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let session_token = extract_bearer_token(&headers).unwrap_or_default();
    if session_token.is_empty() {
        return Err(ApiError::forbidden("No session"));
    }

    let session = webauthn::resolve_session(&state.pool, &session_token)
        .await
        .map_err(|_| ApiError::forbidden("Invalid session"))?
        .ok_or_else(|| ApiError::forbidden("Session expired"))?;

    Ok(Json(serde_json::json!({
        "user_id": session.0,
        "role": session.1,
    })))
}

/// `POST /api/webauthn/logout`
///
/// Invalidates the current WebAuthn session server-side.
/// The session token is extracted from the `Authorization: Bearer` header.
pub async fn webauthn_logout(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let session_token = extract_bearer_token(&headers).unwrap_or_default();
    if !session_token.is_empty() {
        webauthn::delete_session(&state.pool, &session_token)
            .await
            .map_err(|e| ApiError::new(format!("Logout failed: {}", e)))?;
    }
    Ok(Json(serde_json::json!({ "logged_out": true })))
}

async fn resolve_user_from_token(state: &AppState, token: &str) -> Result<String, ApiError> {
    if let Some(admin_token) = &state.config.admin_token {
        if token == admin_token.as_str() {
            return Ok("admin".to_string());
        }
    }

    let handle = state
        .spaces
        .resolve(Some(token))
        .await
        .map_err(|_| ApiError::forbidden("Invalid token"))?;

    Ok(handle.space_id)
}

// ── Share management ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateShareRequest {
    pub label: String,
}

pub async fn create_space_share(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<CreateShareRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if body.label.len() > 256 {
        return Err(ApiError::bad_request("Label too long (max 256 characters)"));
    }
    let token = extract_bearer_token(&headers).unwrap_or_default();
    let share = state
        .spaces
        .share(&token, &body.label)
        .await
        .map_err(|e| ApiError::forbidden(format!("Share failed: {}", e)))?;

    Ok(Json(serde_json::to_value(&share).unwrap_or_default()))
}

pub async fn list_space_shares(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let token = extract_bearer_token(&headers).unwrap_or_default();
    let shares = state
        .spaces
        .list_shares_for_owner(&token)
        .await
        .map_err(|e| ApiError::forbidden(format!("{}", e)))?;

    Ok(Json(serde_json::to_value(&shares).unwrap_or_default()))
}

pub async fn revoke_all_space_shares(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let token = extract_bearer_token(&headers).unwrap_or_default();
    let count = state
        .spaces
        .revoke_all_shares(&token)
        .await
        .map_err(|e| ApiError::forbidden(format!("{}", e)))?;

    if let Ok(space_id) = state.spaces.find_owner_token(&token).await {
        state.sync_hub.broadcast_revoked(&space_id).await;
        state.sync_hub.revoke_space_tokens(&space_id).await;
    }

    Ok(Json(serde_json::json!({
        "revoked": true,
        "count": count,
    })))
}

fn is_blocked_extension(filename: &str) -> bool {
    let blocked: &[&str] = &[".exe", ".dll", ".so", ".sh", ".bat", ".ps1", ".scr", ".msi", ".com", ".cmd", ".vbs", ".jar", ".app"];
    let lower = filename.to_lowercase();
    if let Some(dot) = lower.rfind('.') {
        blocked.contains(&lower[dot..].as_ref())
    } else {
        false
    }
}
