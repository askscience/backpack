use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, error, info, warn};

use super::client::SyncClient;
use super::config::SyncConfig;
use super::state::SyncState;
use super::types::{SyncEntry, SyncEntryStatus, RemoteFileEntry};
use super::watcher::{self, FileWatcher, SyncEvent};
use super::ws;

/// Core sync engine that orchestrates bi-directional file synchronization
/// between a local watch directory and a backpack server.
pub struct SyncEngine {
    config: SyncConfig,
    client: SyncClient,
    state: SyncState,
}

impl SyncEngine {
    pub fn new(config: SyncConfig, client: SyncClient, state: SyncState) -> Self {
        Self { config, client, state }
    }

    pub async fn run(self: std::sync::Arc<Self>) -> Result<()> {
        let mut event_rx = {
            let watcher = FileWatcher::new(
                self.config.debounce_ms, self.config.ignore_patterns.clone(),
            );
            let (rx, _handle) = watcher.start(Path::new(&self.config.watch_dir)).await
                .context("Failed to start file watcher")?;
            rx
        };

        info!("Performing initial scan of {}", self.config.watch_dir);
        let _ = self.scan_local_changes().await;

        let poll_interval = std::time::Duration::from_secs(self.config.poll_interval_secs);
        let watch_engine = self.clone();
        let poll_engine = self.clone();

        let watch_handle = tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                if let Err(e) = watch_engine.handle_local_event(event).await {
                    error!("Error handling local event: {}", e);
                }
            }
            info!("Watch loop ended");
        });

        let poll_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(poll_interval);
            interval.tick().await;
            loop {
                interval.tick().await;
                if let Err(e) = poll_engine.poll_remote_changes().await {
                    error!("Error polling remote changes: {}", e);
                }
            }
        });

        tokio::spawn({
            let ws_engine = self.clone();
            let server_url = self.config.server_url.clone();
            let space_token = self.config.space_token.clone();
            async move {
                if let Some(ref token) = space_token {
                    match ws::connect(&server_url, token).await {
                        Ok((mut ws_rx, _ws_keepalive)) => {
                            info!("WebSocket push sync active");
                            while let Some(event) = ws_rx.recv().await {
                                if let Err(e) = ws_engine.handle_ws_event(event).await {
                                    error!("Error handling WS event: {}", e);
                                }
                            }
                            info!("WebSocket push sync ended");
                        }
                        Err(e) => {
                            warn!("WebSocket push sync unavailable: {}", e);
                            std::future::pending::<()>().await;
                        }
                    }
                } else {
                    info!("No space token, WebSocket push sync skipped");
                    std::future::pending::<()>().await;
                }
            }
        });

        tokio::select! {
            _ = watch_handle => info!("Watch loop exited"),
            _ = poll_handle => info!("Poll loop exited"),
        }

        Ok(())
    }

    async fn handle_ws_event(&self, event: ws::WsSyncEvent) -> Result<()> {
        debug!("WS: {} event for file {} ({})", event.typ, event.original_name, event.file_id);
        if event.typ == "deleted" { return Ok(()); }
        if let Ok(Some(_)) = self.state.get_by_remote_id(&event.file_id).await {
            debug!("WS: already tracking file {}", event.file_id);
            return Ok(());
        }

        let local_entries = self.state.list_all().await?;
        let same_name = local_entries.iter().find(|e| {
            Path::new(&e.relative_path).file_name().map(|f| f.to_string_lossy()).unwrap_or_default() == event.original_name
        });

        if let Some(local) = same_name {
            let current_hash = self.hash_local_file(&local.relative_path).await.unwrap_or_default();
            if current_hash == local.content_hash {
                info!("WS: updating remote ref for {}: {} -> {}", local.relative_path,
                      local.remote_file_id.as_deref().unwrap_or("-"), event.file_id);
                let mut updated = local.clone();
                updated.remote_file_id = Some(event.file_id.clone());
                updated.remote_file_size = Some(event.file_size);
                self.state.upsert(&updated).await?;
                return Ok(());
            }
            debug!("WS: file {} has local changes, skipping push", local.relative_path);
            return Ok(());
        }

        info!("WS: downloading new file: {} ({})", event.original_name, event.file_id);
        let dest_path = Path::new(&self.config.watch_dir).join(&event.original_name);

        if let Err(e) = self.client.download_file(&event.file_id, &dest_path).await {
            error!("WS: download failed for {}: {}", event.file_id, e);
            return Ok(());
        }

        let hash = sha256_file(&dest_path).await?;
        let size = tokio::fs::metadata(&dest_path).await.map(|m| m.len() as i64).unwrap_or(event.file_size);
        let mtime = file_modified_secs(&dest_path);
        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

        let entry = SyncEntry {
            relative_path: event.original_name.clone(),
            content_hash: hash, file_size: size,
            remote_file_id: Some(event.file_id), remote_file_size: Some(event.file_size),
            sync_status: SyncEntryStatus::Synced.to_string(),
            last_local_modified: mtime, last_synced_at: Some(now), created_at: String::new(),
        };
        self.state.upsert(&entry).await?;
        info!("WS: downloaded and tracked: {} -> {}", event.original_name, dest_path.display());
        Ok(())
    }

    pub async fn scan_local_changes(&self) -> Result<()> {
        let watch_dir = Path::new(&self.config.watch_dir);
        self.scan_dir_recursive(watch_dir, watch_dir).await
    }

    async fn scan_dir_recursive(&self, root: &Path, current: &Path) -> Result<()> {
        let mut read_dir = tokio::fs::read_dir(current).await
            .with_context(|| format!("Failed to read dir {}", current.display()))?;
        while let Some(entry) = read_dir.next_entry().await? {
            let path = entry.path();
            if watcher::should_ignore(&path, root, &self.config.ignore_patterns) { continue; }
            if entry.file_type().await?.is_dir() {
                Box::pin(self.scan_dir_recursive(root, &path)).await?;
            } else if let Err(e) = self.upload_if_changed(&path, root).await {
                warn!("Scan upload failed for {}: {}", path.display(), e);
            }
        }
        Ok(())
    }

    async fn handle_local_event(&self, event: SyncEvent) -> Result<()> {
        let root = Path::new(&self.config.watch_dir);
        match event {
            SyncEvent::Changed(path) => {
                if !path.exists() { return Ok(()); }
                self.upload_if_changed(&path, root).await?;
            }
            SyncEvent::Deleted(path) => { self.handle_local_delete(&path, root).await?; }
        }
        Ok(())
    }

    async fn upload_if_changed(&self, full_path: &Path, root: &Path) -> Result<()> {
        let relative = strip_root(full_path, root).unwrap_or_else(|| full_path.to_string_lossy().to_string());
        let file_data = tokio::fs::read(full_path).await
            .with_context(|| format!("Failed to read {}", full_path.display()))?;
        let hash = hex::encode(Sha256::digest(&file_data));
        let size = file_data.len() as i64;
        let mtime = file_modified_secs(full_path);

        if let Some(existing) = self.state.get_by_path(&relative).await? {
            if existing.content_hash == hash { return Ok(()); }
        }

        info!("Uploading: {} (hash={}, size={})", relative, &hash[..8], size);
        let upload_resp = self.client.upload_file(full_path, &original_name_from_relative(&relative)).await?;

        if let Some(existing) = self.state.get_by_path(&relative).await? {
            if let Some(ref old_id) = existing.remote_file_id {
                if old_id != &upload_resp.id {
                    info!("Cleaning up old remote file: {}", old_id);
                    let _ = self.client.delete_remote_file(old_id).await;
                }
            }
        }

        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let entry = SyncEntry {
            relative_path: relative, content_hash: hash, file_size: size,
            remote_file_id: Some(upload_resp.id.clone()), remote_file_size: Some(upload_resp.file_size),
            sync_status: SyncEntryStatus::Synced.to_string(),
            last_local_modified: mtime, last_synced_at: Some(now), created_at: String::new(),
        };
        self.state.upsert(&entry).await?;
        info!("Synced locally: {} -> remote id {}", upload_resp.original_name, upload_resp.id);
        Ok(())
    }

    async fn handle_local_delete(&self, full_path: &Path, root: &Path) -> Result<()> {
        let relative = strip_root(full_path, root).unwrap_or_else(|| full_path.to_string_lossy().to_string());
        if let Some(entry) = self.state.get_by_path(&relative).await? {
            if let Some(ref remote_id) = entry.remote_file_id {
                info!("Deleting remote file {} (local {} deleted)", remote_id, relative);
                if let Err(e) = self.client.delete_remote_file(remote_id).await {
                    warn!("Failed to delete remote file {}: {}", remote_id, e);
                }
            }
            self.state.delete_by_path(&relative).await?;
            info!("Untracked locally deleted file: {}", relative);
        }
        Ok(())
    }

    pub async fn poll_remote_changes(&self) -> Result<()> {
        let remote_files = match self.client.list_remote_files().await {
            Ok(files) => files,
            Err(e) => { warn!("Failed to fetch remote inventory: {}", e); return Ok(()); }
        };

        let remote_by_id: HashMap<String, &RemoteFileEntry> =
            remote_files.iter().map(|f| (f.id.clone(), f)).collect();
        let local_entries = self.state.list_all().await?;

        for remote in &remote_files {
            if let Some(local) = self.state.get_by_remote_id(&remote.id).await? {
                let current_hash = self.hash_local_file(&local.relative_path).await.unwrap_or_default();
                if current_hash == local.content_hash && local.remote_file_size == Some(remote.file_size) {
                    continue;
                }
                info!("Remote file updated: {} (id={})", remote.original_name, remote.id);
                if let Err(e) = self.download_and_track(remote).await {
                    error!("Failed to download remote update {}: {}", remote.id, e);
                }
                continue;
            }

            let same_name_local = local_entries.iter().find(|e| {
                original_name_from_relative(&e.relative_path) == remote.original_name
            });

            if let Some(local) = same_name_local {
                let current_hash = self.hash_local_file(&local.relative_path).await.unwrap_or_default();
                if !current_hash.is_empty() && current_hash != local.content_hash {
                    warn!("Conflict: '{}' modified both locally and remotely", remote.original_name);
                    self.resolve_conflict(local, remote).await?;
                } else {
                    info!("Downloading remote version of: {} (new id {})", remote.original_name, remote.id);
                    if let Err(e) = self.download_and_track(remote).await {
                        error!("Failed to download {}: {}", remote.id, e);
                    }
                    self.state.delete_by_path(&local.relative_path).await?;
                }
            } else {
                info!("New remote file: {} (id={})", remote.original_name, remote.id);
                if let Err(e) = self.download_and_track(remote).await {
                    error!("Failed to download {}: {}", remote.id, e);
                }
            }
        }

        for local in &local_entries {
            if let Some(ref remote_id) = local.remote_file_id {
                if !remote_by_id.contains_key(remote_id) {
                    let full_path = Path::new(&self.config.watch_dir).join(&local.relative_path);
                    info!("Remote file {} deleted — removing local: {}", remote_id, local.relative_path);
                    if full_path.exists() {
                        if let Err(e) = tokio::fs::remove_file(&full_path).await {
                            warn!("Failed to remove local file {}: {}", full_path.display(), e);
                        }
                    }
                    self.state.delete_by_path(&local.relative_path).await?;
                }
            }
        }
        Ok(())
    }

    async fn download_and_track(&self, remote: &RemoteFileEntry) -> Result<()> {
        let relative = remote.original_name.clone();
        let dest_path = Path::new(&self.config.watch_dir).join(&relative);
        self.client.download_file(&remote.id, &dest_path).await?;
        let hash = sha256_file(&dest_path).await?;
        let size = tokio::fs::metadata(&dest_path).await.map(|m| m.len() as i64).unwrap_or(remote.file_size);
        let mtime = file_modified_secs(&dest_path);
        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let entry = SyncEntry {
            relative_path: relative, content_hash: hash, file_size: size,
            remote_file_id: Some(remote.id.clone()), remote_file_size: Some(remote.file_size),
            sync_status: SyncEntryStatus::Synced.to_string(),
            last_local_modified: mtime, last_synced_at: Some(now), created_at: String::new(),
        };
        self.state.upsert(&entry).await?;
        info!("Downloaded: {} -> {}", remote.original_name, dest_path.display());
        Ok(())
    }

    async fn resolve_conflict(&self, local: &SyncEntry, remote: &RemoteFileEntry) -> Result<()> {
        let full_path = Path::new(&self.config.watch_dir).join(&local.relative_path);
        if full_path.exists() {
            let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
            let stem = full_path.file_stem().map(|s| s.to_string_lossy()).unwrap_or_default();
            let ext = full_path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
            let conflict_name = format!("{}.conflict.{}{}", stem, ts, ext);
            let conflict_path = full_path.with_file_name(&conflict_name);
            info!("Conflict backup: {} -> {}", full_path.display(), conflict_path.display());
            tokio::fs::rename(&full_path, &conflict_path).await?;
        }
        self.download_and_track(remote).await?;
        Ok(())
    }

    async fn hash_local_file(&self, relative_path: &str) -> Result<String> {
        let full_path = Path::new(&self.config.watch_dir).join(relative_path);
        sha256_file(&full_path).await
    }
}

fn strip_root(full_path: &Path, root: &Path) -> Option<String> {
    full_path.strip_prefix(root).ok().map(|p| p.to_string_lossy().to_string())
}

fn original_name_from_relative(relative: &str) -> String {
    Path::new(relative).file_name().unwrap_or_default().to_string_lossy().to_string()
}

async fn sha256_file(path: &Path) -> Result<String> {
    let data = tokio::fs::read(path).await
        .with_context(|| format!("Failed to read {} for hashing", path.display()))?;
    Ok(hex::encode(Sha256::digest(&data)))
}

fn file_modified_secs(path: &Path) -> i64 {
    std::fs::metadata(path).and_then(|m| m.modified())
        .map(|t| t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0))
        .unwrap_or(0)
}
