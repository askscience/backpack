use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use axum_extra::extract::Multipart;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::fs;
use tracing::{error, info, warn};

use crate::catalog;
use crate::config::Config;
use crate::db::{self, FileRecord, f32_vec_to_bytes};
use crate::spaces::{SpaceHandle, SpaceManager};
use crate::vector;

#[derive(Clone)]
pub struct AppState {
    #[allow(dead_code)]
    pub pool: SqlitePool,
    pub config: Arc<Config>,
    pub client: Client,
    pub spaces: Arc<SpaceManager>,
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
        ApiError::new(e.to_string())
    }
}

#[derive(Deserialize)]
pub struct TokenQuery {
    pub token: Option<String>,
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
    pub title: Option<String>,
    pub summary: Option<String>,
}

async fn resolve_space(state: &AppState, token: Option<String>) -> Result<SpaceHandle, ApiError> {
    state
        .spaces
        .resolve(token.as_deref())
        .await
        .map_err(|e| ApiError::forbidden(format!("Invalid space token: {}", e)))
}

pub async fn upload_handler(
    State(state): State<AppState>,
    Query(TokenQuery { token }): Query<TokenQuery>,
    mut multipart: Multipart,
) -> Result<Json<BatchUploadResponse>, ApiError> {
    let space = resolve_space(&state, token).await?;

    let mut files: Vec<(String, Vec<u8>, Option<String>)> = Vec::new();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::new(format!("Multipart error: {}", e)))?
    {
        let name = field.name().unwrap_or("file").to_string();
        if name == "file" || name.is_empty() {
            let file_name = field.file_name().unwrap_or("unnamed").to_string();
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
    Query(query): Query<SearchQuery>,
) -> Result<Json<SearchResponse>, ApiError> {
    let space = resolve_space(&state, query.token).await?;

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
    Query(TokenQuery { token }): Query<TokenQuery>,
) -> Result<Json<InventoryResponse>, ApiError> {
    let space = resolve_space(&state, token).await?;

    let categories = db::list_files_grouped(&space.pool)
        .await
        .map_err(|e| ApiError::new(format!("Failed to list files: {}", e)))?;

    let total_files: usize = categories.iter().map(|g| g.count).sum();

    Ok(Json(InventoryResponse {
        total_files,
        categories,
    }))
}

pub async fn download_handler(
    State(state): State<AppState>,
    Query(TokenQuery { token }): Query<TokenQuery>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
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

pub async fn delete_handler(
    State(state): State<AppState>,
    Query(TokenQuery { token }): Query<TokenQuery>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let space = resolve_space(&state, token).await?;

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

    let _ = state.spaces.add_usage(&space.space_id, (-file_size as i64) as u64).await;

    info!("Deleted file: id={}, name={}", id, file.original_name);

    Ok(Json(serde_json::json!({
        "deleted": true,
        "id": id
    })))
}

pub async fn ask_handler(
    State(state): State<AppState>,
    Query(TokenQuery { token }): Query<TokenQuery>,
    Json(body): Json<AskRequest>,
) -> Result<Json<AskResponse>, ApiError> {
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
