use anyhow::{Context, Result};
use reqwest::StatusCode;
use reqwest::multipart;
use std::path::Path;
use tracing::{debug, info, warn};

use super::types::{BatchUploadResponse, InventoryResponse, RemoteFileEntry, UploadResponse};

/// HTTP client wrapping the backpack server's REST API.
pub struct SyncClient {
    inner: reqwest::Client,
    server_url: String,
    space_token: Option<String>,
}

impl SyncClient {
    pub fn new(server_url: String, space_token: Option<String>) -> Self {
        let inner = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .expect("Failed to create HTTP client");
        Self {
            inner,
            server_url: server_url.trim_end_matches('/').to_string(),
            space_token,
        }
    }

    pub async fn list_remote_files(&self) -> Result<Vec<RemoteFileEntry>> {
        let url = self.endpoint("/inventory");
        debug!("GET {}", url);
        let resp = self.inner.get(&url).headers(self.auth_headers()).send().await
            .with_context(|| format!("Failed to fetch inventory from {}", url))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("Failed to fetch inventory: HTTP {} — {}", status, body));
        }
        let inventory: InventoryResponse = resp.json().await
            .context("Failed to parse inventory response")?;
        let files: Vec<RemoteFileEntry> = inventory.categories.into_iter()
            .flat_map(|g| g.files).collect();
        info!("Fetched {} remote files", files.len());
        Ok(files)
    }

    pub async fn upload_file(&self, local_path: &Path, original_name: &str) -> Result<UploadResponse> {
        let url = self.endpoint("/upload");
        debug!("POST {} (name={})", url, original_name);
        let file_bytes = tokio::fs::read(local_path).await
            .with_context(|| format!("Failed to read file for upload: {}", local_path.display()))?;
        let file_name = original_name.to_string();
        let part = multipart::Part::bytes(file_bytes).file_name(file_name.clone())
            .mime_str(&mime_guess::from_path(original_name).first_or_octet_stream().to_string())
            .context("Failed to set MIME type")?;
        let form = multipart::Form::new().part("file", part);
        let resp = self.inner.post(&url).headers(self.auth_headers()).multipart(form).send().await
            .with_context(|| format!("Failed to upload {} to {}", original_name, url))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("Upload failed for '{}': HTTP {} — {}", original_name, status, body));
        }
        let batch: BatchUploadResponse = resp.json().await
            .context("Failed to parse upload response")?;
        batch.results.into_iter().next()
            .ok_or_else(|| anyhow::anyhow!("Upload response contained no results"))
    }

    pub async fn download_file(&self, file_id: &str, dest_path: &Path) -> Result<()> {
        let url = self.endpoint(&format!("/download/{}", file_id));
        debug!("GET {}", url);
        let resp = self.inner.get(&url).headers(self.auth_headers()).send().await
            .with_context(|| format!("Failed to download file {} from {}", file_id, url))?;
        if !resp.status().is_success() {
            if resp.status() == StatusCode::NOT_FOUND {
                anyhow::bail!("Remote file {} no longer exists (404)", file_id);
            }
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Download failed for {}: {}", file_id, body);
        }
        if let Some(parent) = dest_path.parent() {
            tokio::fs::create_dir_all(parent).await
                .with_context(|| format!("Failed to create dir {}", parent.display()))?;
        }
        let bytes = resp.bytes().await.context("Failed to read download response body")?;
        tokio::fs::write(dest_path, &bytes).await
            .with_context(|| format!("Failed to write downloaded file to {}", dest_path.display()))?;
        info!("Downloaded {} -> {}", file_id, dest_path.display());
        Ok(())
    }

    pub async fn delete_remote_file(&self, file_id: &str) -> Result<()> {
        let url = self.endpoint(&format!("/files/{}", file_id));
        debug!("DELETE {}", url);
        let resp = self.inner.delete(&url).headers(self.auth_headers()).send().await
            .with_context(|| format!("Failed to delete remote file {}", file_id))?;
        if resp.status() == StatusCode::NOT_FOUND {
            warn!("Remote file {} already deleted (404), continuing", file_id);
            return Ok(());
        }
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Delete failed for {}: {}", file_id, body);
        }
        info!("Deleted remote file {}", file_id);
        Ok(())
    }

    fn endpoint(&self, path: &str) -> String {
        if let Some(ref token) = self.space_token {
            format!("{}{}?token={}", self.server_url, path, urlencode(token))
        } else {
            format!("{}{}", self.server_url, path)
        }
    }

    fn auth_headers(&self) -> reqwest::header::HeaderMap {
        reqwest::header::HeaderMap::new()
    }
}

fn urlencode(s: &str) -> String {
    s.replace('%', "%25").replace(' ', "%20")
        .replace('#', "%23").replace('&', "%26").replace('+', "%2B")
}
