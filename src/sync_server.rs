//! Server-side sync management API.
//!
//! Provides endpoints for the web UI and CLI daemon to manage
//! per-space sync configuration and report/list file sync status.

use axum::{
    extract::{Path as AxumPath, Query, State},
    response::{IntoResponse, Redirect},
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use tracing::info;

use crate::handlers::{self, ApiError, AppState, TokenQuery};

// ── Data types ──────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SyncConfig {
    pub watch_dirs: Vec<String>,
    pub ignore_patterns: Vec<String>,
    pub poll_interval_secs: u64,
    pub debounce_ms: u64,
    pub enabled: bool,
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSyncConfigRequest {
    pub watch_dirs: Option<Vec<String>>,
    pub ignore_patterns: Option<Vec<String>>,
    pub poll_interval_secs: Option<u64>,
    pub debounce_ms: Option<u64>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncFileStatus {
    pub relative_path: String,
    pub content_hash: String,
    pub file_size: i64,
    pub remote_file_id: Option<String>,
    pub sync_status: String,
    pub last_modified: i64,
    pub last_synced_at: Option<String>,
    pub reported_at: String,
}

#[derive(Debug, Deserialize)]
pub struct ReportSyncStatusRequest {
    pub entries: Vec<SyncStatusEntry>,
}

#[derive(Debug, Deserialize)]
pub struct SyncStatusEntry {
    pub relative_path: String,
    pub content_hash: String,
    pub file_size: i64,
    pub remote_file_id: Option<String>,
    pub sync_status: String,
    pub last_modified: i64,
}

// ── Schema ──────────────────────────────────────────────────────────

pub async fn ensure_sync_schema(pool: &SqlitePool) -> Result<(), anyhow::Error> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS sync_configs (
            space_id TEXT PRIMARY KEY,
            watch_dirs TEXT NOT NULL DEFAULT '[]',
            ignore_patterns TEXT NOT NULL DEFAULT '[]',
            poll_interval_secs INTEGER NOT NULL DEFAULT 30,
            debounce_ms INTEGER NOT NULL DEFAULT 500,
            enabled INTEGER NOT NULL DEFAULT 1,
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS sync_file_status (
            space_id TEXT NOT NULL,
            relative_path TEXT NOT NULL,
            content_hash TEXT NOT NULL DEFAULT '',
            file_size INTEGER NOT NULL DEFAULT 0,
            remote_file_id TEXT,
            sync_status TEXT NOT NULL DEFAULT 'synced',
            last_modified INTEGER NOT NULL DEFAULT 0,
            last_synced_at TEXT,
            reported_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (space_id, relative_path)
        )",
    )
    .execute(pool)
    .await?;

    Ok(())
}

// ── Config handlers ─────────────────────────────────────────────────

/// `GET /sync/config?token=<space_token>`
pub async fn get_sync_config(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let token = handlers::extract_bearer_token(&headers);
    let space = handlers::resolve_space(&state, token).await?;

    let row = sqlx::query("SELECT * FROM sync_configs WHERE space_id = ?1")
        .bind(&space.space_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|e| ApiError::new(format!("DB error: {}", e)))?;

    let config = if let Some(ref r) = row {
        let watch_dirs_str: String = r.get("watch_dirs");
        let ignore_patterns_str: String = r.get("ignore_patterns");
        SyncConfig {
            watch_dirs: serde_json::from_str(&watch_dirs_str).unwrap_or_default(),
            ignore_patterns: serde_json::from_str(&ignore_patterns_str).unwrap_or_default(),
            poll_interval_secs: r.get::<i64, _>("poll_interval_secs") as u64,
            debounce_ms: r.get::<i64, _>("debounce_ms") as u64,
            enabled: r.get::<i64, _>("enabled") != 0,
            updated_at: r.get("updated_at"),
        }
    } else {
        SyncConfig {
            watch_dirs: vec![],
            ignore_patterns: vec![],
            poll_interval_secs: 30,
            debounce_ms: 500,
            enabled: true,
            updated_at: String::new(),
        }
    };

    Ok(Json(serde_json::to_value(&config).unwrap_or_default()))
}

/// `PUT /sync/config?token=<space_token>`
pub async fn update_sync_config(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<UpdateSyncConfigRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let token = handlers::extract_bearer_token(&headers);
    let space = handlers::resolve_space(&state, token).await?;

    // Build the updated config by merging with existing values.
    let existing = sqlx::query("SELECT * FROM sync_configs WHERE space_id = ?1")
        .bind(&space.space_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|e| ApiError::new(format!("DB error: {}", e)))?;

    let (watch_dirs, ignore_patterns, poll_interval, debounce, enabled) = if let Some(ref row) = existing {
        let wd: String = row.get("watch_dirs");
        let ip: String = row.get("ignore_patterns");
        let watch = body.watch_dirs.unwrap_or_else(|| serde_json::from_str(&wd).unwrap_or_default());
        let ignore = body.ignore_patterns.unwrap_or_else(|| serde_json::from_str(&ip).unwrap_or_default());
        let poll = body.poll_interval_secs.unwrap_or(row.get::<i64, _>("poll_interval_secs") as u64);
        let deb = body.debounce_ms.unwrap_or(row.get::<i64, _>("debounce_ms") as u64);
        let en = body.enabled.unwrap_or(row.get::<i64, _>("enabled") != 0);
        (watch, ignore, poll, deb, en)
    } else {
        (
            body.watch_dirs.unwrap_or_default(),
            body.ignore_patterns.unwrap_or_default(),
            body.poll_interval_secs.unwrap_or(30),
            body.debounce_ms.unwrap_or(500),
            body.enabled.unwrap_or(true),
        )
    };

    let watch_json = serde_json::to_string(&watch_dirs).unwrap_or_default();
    let ignore_json = serde_json::to_string(&ignore_patterns).unwrap_or_default();
    let enabled_int: i64 = if enabled { 1 } else { 0 };

    sqlx::query(
        "INSERT INTO sync_configs (space_id, watch_dirs, ignore_patterns, poll_interval_secs, debounce_ms, enabled, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))
         ON CONFLICT(space_id) DO UPDATE SET
            watch_dirs = excluded.watch_dirs,
            ignore_patterns = excluded.ignore_patterns,
            poll_interval_secs = excluded.poll_interval_secs,
            debounce_ms = excluded.debounce_ms,
            enabled = excluded.enabled,
            updated_at = excluded.updated_at",
    )
    .bind(&space.space_id)
    .bind(&watch_json)
    .bind(&ignore_json)
    .bind(poll_interval as i64)
    .bind(debounce as i64)
    .bind(enabled_int)
    .execute(&state.pool)
    .await
    .map_err(|e| ApiError::new(format!("Failed to save config: {}", e)))?;

    info!("Sync config updated for space {}", &space.space_id[..8.min(space.space_id.len())]);

    let config = SyncConfig {
        watch_dirs,
        ignore_patterns,
        poll_interval_secs: poll_interval,
        debounce_ms: debounce,
        enabled,
        updated_at: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    };

    Ok(Json(serde_json::to_value(&config).unwrap_or_default()))
}

/// `GET /sync/config/download?token=<space_token>`
///
/// Returns a `.backpack-sync.toml` file for download, generated from the
/// server-side sync config for this space.
pub async fn download_sync_config(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let token = handlers::extract_bearer_token(&headers);
    let space = handlers::resolve_space(&state, token).await?;

    let row = sqlx::query("SELECT * FROM sync_configs WHERE space_id = ?1")
        .bind(&space.space_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|e| ApiError::new(format!("DB error: {}", e)))?;

    let (watch_dirs, ignore_patterns, poll_interval, debounce, enabled) = match row {
        Some(ref r) => {
            let wd: String = r.get("watch_dirs");
            let ip: String = r.get("ignore_patterns");
            (
                serde_json::from_str::<Vec<String>>(&wd).unwrap_or_default(),
                serde_json::from_str::<Vec<String>>(&ip).unwrap_or_default(),
                r.get::<i64, _>("poll_interval_secs") as u64,
                r.get::<i64, _>("debounce_ms") as u64,
                r.get::<i64, _>("enabled") != 0,
            )
        }
        None => {
            return Err(ApiError::not_found(
                "No sync config found for this space. Configure sync first.",
            ));
        }
    };

    if !enabled {
        return Err(ApiError::not_found("Sync is disabled for this space."));
    }

    let watch_dir = watch_dirs.first().cloned().unwrap_or_else(|| "./watch".into());
    let ignores_str = ignore_patterns
        .iter()
        .map(|p| format!("\"{}\"", p.escape_default()))
        .collect::<Vec<_>>()
        .join(", ");

    let toml_content = format!(
        r#"# Backpack Sync Configuration
# Generated for space: {}
# Server: {}
#
# Add your space_token below to authenticate the sync daemon:
# space_token = "your-token-here"

watch_dir = "{}"
server_url = "{}"
poll_interval_secs = {}
debounce_ms = {}
ignore_patterns = [{}]
"#,
        space.space_id,
        state.config.llm_endpoint.replace("/v1", ""),
        watch_dir,
        state.config.llm_endpoint.replace("/v1", "").replace("https://", "http://"),
        poll_interval,
        debounce,
        ignores_str,
    );

    let headers = [
        (axum::http::header::CONTENT_TYPE, "application/octet-stream"),
        (
            axum::http::header::CONTENT_DISPOSITION,
            "attachment; filename=\".backpack-sync.toml\"",
        ),
    ];

    Ok((headers, toml_content))
}

// ── Status handlers ─────────────────────────────────────────────────

/// `GET /sync/status?token=<space_token>`
///
/// Returns all tracked file sync statuses for this space.
pub async fn get_sync_status(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let token = handlers::extract_bearer_token(&headers);
    let space = handlers::resolve_space(&state, token).await?;

    let rows = sqlx::query(
        "SELECT relative_path, content_hash, file_size, remote_file_id,
                sync_status, last_modified, last_synced_at, reported_at
         FROM sync_file_status WHERE space_id = ?1
         ORDER BY relative_path",
    )
    .bind(&space.space_id)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| ApiError::new(format!("DB error: {}", e)))?;

    let entries: Vec<SyncFileStatus> = rows
        .iter()
        .map(|r| SyncFileStatus {
            relative_path: r.get("relative_path"),
            content_hash: r.get("content_hash"),
            file_size: r.get("file_size"),
            remote_file_id: r.get("remote_file_id"),
            sync_status: r.get("sync_status"),
            last_modified: r.get("last_modified"),
            last_synced_at: r.get("last_synced_at"),
            reported_at: r.get("reported_at"),
        })
        .collect();

    let synced = entries.iter().filter(|e| e.sync_status == "synced").count();
    let pending = entries.iter().filter(|e| e.sync_status == "pending_upload" || e.sync_status == "pending_download").count();
    let conflicted = entries.iter().filter(|e| e.sync_status == "conflicted").count();
    let errors = entries.iter().filter(|e| e.sync_status == "error").count();

    Ok(Json(serde_json::json!({
        "space_id": space.space_id,
        "total_tracked": entries.len(),
        "synced": synced,
        "pending": pending,
        "conflicted": conflicted,
        "errors": errors,
        "entries": entries,
    })))
}

/// `POST /sync/status?token=<space_token>`
///
/// Called by the CLI daemon to report sync status for tracked files.
/// Accepts a batch of entries and upserts each one.
pub async fn report_sync_status(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<ReportSyncStatusRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let token = handlers::extract_bearer_token(&headers);
    let space = handlers::resolve_space(&state, token).await?;

    let mut count = 0;
    for entry in &body.entries {
        sqlx::query(
            "INSERT INTO sync_file_status (space_id, relative_path, content_hash, file_size, remote_file_id, sync_status, last_modified, last_synced_at, reported_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'), datetime('now'))
             ON CONFLICT(space_id, relative_path) DO UPDATE SET
                content_hash = excluded.content_hash,
                file_size = excluded.file_size,
                remote_file_id = excluded.remote_file_id,
                sync_status = excluded.sync_status,
                last_modified = excluded.last_modified,
                last_synced_at = CASE WHEN excluded.sync_status = 'synced' THEN datetime('now') ELSE sync_file_status.last_synced_at END,
                reported_at = excluded.reported_at",
        )
        .bind(&space.space_id)
        .bind(&entry.relative_path)
        .bind(&entry.content_hash)
        .bind(entry.file_size)
        .bind(&entry.remote_file_id)
        .bind(&entry.sync_status)
        .bind(entry.last_modified)
        .execute(&state.pool)
        .await
        .map_err(|e| ApiError::new(format!("DB error: {}", e)))?;
        count += 1;
    }

    Ok(Json(serde_json::json!({
        "reported": count,
        "space_id": space.space_id,
    })))
}

/// `DELETE /sync/status/:relative_path?token=<space_token>`
///
/// Removes a file from the sync status table (called when a file is
/// removed from tracking or deleted locally).
pub async fn delete_sync_status(
    State(state): State<AppState>,
    AxumPath(relative_path): AxumPath<String>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let token = handlers::extract_bearer_token(&headers);
    let space = handlers::resolve_space(&state, token).await?;

    sqlx::query("DELETE FROM sync_file_status WHERE space_id = ?1 AND relative_path = ?2")
        .bind(&space.space_id)
        .bind(&relative_path)
        .execute(&state.pool)
        .await
        .map_err(|e| ApiError::new(format!("DB error: {}", e)))?;

    Ok(Json(serde_json::json!({
        "deleted": true,
        "relative_path": relative_path,
    })))
}

// ── Daemon download ─────────────────────────────────────────────────

/// `GET /sync/daemon`
///
/// Redirects to the latest GitHub release binary for the requested platform.
/// Query params:
/// - `os` — "linux", "macos", or "windows" (default: "linux")
/// - `arch` — "x86_64" or "aarch64" (default: "x86_64")
#[derive(Deserialize)]
pub struct DaemonQuery {
    pub os: Option<String>,
    pub arch: Option<String>,
}

pub async fn download_daemon(
    Query(query): Query<DaemonQuery>,
) -> Redirect {
    let os = query.os.as_deref().unwrap_or("linux");
    let arch = query.arch.as_deref().unwrap_or("x86_64");
    let ext = if os == "windows" { ".exe" } else { "" };
    let filename = format!("backpack-{}-{}{}", os, arch, ext);
    let url = format!(
        "https://github.com/askscience/backpack/releases/latest/download/{}",
        filename
    );
    Redirect::to(&url)
}

/// `GET /sync/daemon/version`
///
/// Returns the latest daemon version and download URLs for all platforms.
/// Reads the version from the compiled binary's `CARGO_PKG_VERSION`.
#[derive(Serialize)]
pub struct DaemonVersion {
    pub latest_version: String,
    pub download_url: String,
    pub release_notes_url: String,
    pub platforms: Vec<PlatformInfo>,
}

#[derive(Serialize)]
pub struct PlatformInfo {
    pub os: String,
    pub arch: String,
    pub label: String,
    pub url: String,
}

pub async fn daemon_version() -> Json<DaemonVersion> {
    let ver = format!("v{}", env!("CARGO_PKG_VERSION"));
    let base = "https://github.com/askscience/backpack/releases/latest/download";

    Json(DaemonVersion {
        latest_version: ver.clone(),
        download_url: "https://github.com/askscience/backpack/releases/latest".into(),
        release_notes_url: format!("https://github.com/askscience/backpack/releases/tag/{}", ver),
        platforms: vec![
            PlatformInfo { os: "linux".into(), arch: "x86_64".into(), label: "Linux x86_64".into(), url: format!("{}/backpack-linux-x86_64", base) },
            PlatformInfo { os: "linux".into(), arch: "aarch64".into(), label: "Linux ARM64".into(), url: format!("{}/backpack-linux-aarch64", base) },
            PlatformInfo { os: "macos".into(), arch: "x86_64".into(), label: "macOS Intel".into(), url: format!("{}/backpack-macos-x86_64", base) },
            PlatformInfo { os: "macos".into(), arch: "aarch64".into(), label: "macOS Apple Silicon".into(), url: format!("{}/backpack-macos-aarch64", base) },
            PlatformInfo { os: "windows".into(), arch: "x86_64".into(), label: "Windows x86_64".into(), url: format!("{}/backpack-windows-x86_64.exe", base) },
        ],
    })
}
