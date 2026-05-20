use serde::{Deserialize, Serialize};

/// A single file entry as returned by the server's /inventory endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteFileEntry {
    pub id: String,
    pub original_name: String,
    pub mime: String,
    pub file_size: i64,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub tags: Option<String>,
    pub category: Option<String>,
    pub created_at: String,
    pub has_embedding: bool,
}

/// /inventory response: list of category groups.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteCategoryGroup {
    pub category: String,
    pub count: usize,
    pub files: Vec<RemoteFileEntry>,
}

/// Full /inventory response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryResponse {
    pub total_files: usize,
    pub categories: Vec<RemoteCategoryGroup>,
}

/// /upload response for a single file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadResponse {
    pub id: String,
    pub original_name: String,
    pub mime: String,
    pub file_size: i64,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub tags: Option<String>,
    pub category: Option<String>,
    #[serde(default)]
    pub extracted_text_length: usize,
    pub created_at: String,
}

/// /upload batch response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchUploadResponse {
    pub total_files: usize,
    pub results: Vec<UploadResponse>,
}

/// Generic server error response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct ErrorResponse {
    pub error: String,
    #[serde(default)]
    pub status: Option<u16>,
}

/// Sync status per tracked file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SyncEntryStatus {
    Synced,
    PendingUpload,
    PendingDownload,
    Conflicted,
    Error(String),
}

impl ToString for SyncEntryStatus {
    fn to_string(&self) -> String {
        match self {
            SyncEntryStatus::Synced => "synced".to_string(),
            SyncEntryStatus::PendingUpload => "pending_upload".to_string(),
            SyncEntryStatus::PendingDownload => "pending_download".to_string(),
            SyncEntryStatus::Conflicted => "conflicted".to_string(),
            SyncEntryStatus::Error(_) => "error".to_string(),
        }
    }
}

/// Single row from the local sync-state database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncEntry {
    pub relative_path: String,
    pub content_hash: String,
    pub file_size: i64,
    pub remote_file_id: Option<String>,
    pub remote_file_size: Option<i64>,
    pub sync_status: String,
    pub last_local_modified: i64,
    pub last_synced_at: Option<String>,
    pub created_at: String,
}

/// Human-readable status report.
#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)]
pub struct StatusReport {
    pub watch_dir: String,
    pub server_url: String,
    pub total_tracked: usize,
    pub synced: usize,
    pub pending_upload: usize,
    pub pending_download: usize,
    pub conflicted: usize,
    pub errors: usize,
}
