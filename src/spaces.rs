use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use tokio::fs;
use tracing::info;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct SpaceHandle {
    pub space_id: String,
    pub pool: SqlitePool,
    pub upload_dir: String,
    #[allow(dead_code)]
    pub quota_bytes: u64,
    #[allow(dead_code)]
    pub iroh_ticket: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct SpaceCreated {
    pub space_id: String,
    pub label: String,
    pub owner_token: String,
    pub quota_mb: u64,
    pub upload_dir: String,
    pub iroh_ticket: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ShareCreated {
    pub share_token: String,
    pub label: String,
    pub space_label: String,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct SpaceEntry {
    pub id: String,
    pub label: String,
    pub owner_token: String,
    pub quota_mb: u64,
    pub used_mb: f64,
    pub shares: usize,
    pub status: String,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct SpaceInfo {
    pub id: String,
    pub label: String,
    pub owner_token: String,
    pub quota_mb: u64,
    pub used_mb: f64,
    pub status: String,
    pub shares: Vec<ShareInfo>,
    pub archives: Vec<ArchiveInfo>,
    pub created_at: String,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ShareInfo {
    pub share_token: String,
    pub label: String,
    pub can_write: bool,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ArchiveInfo {
    pub id: String,
    pub for_share_label: Option<String>,
    pub download_token: String,
    pub expires_at: String,
    pub downloaded: bool,
}

#[derive(Clone, Debug)]
pub struct DeleteResult {
    pub purged: bool,
    pub archive_path: Option<String>,
    pub download_token: Option<String>,
}

#[derive(Clone, Debug)]
pub enum DeleteMode {
    Purge,
    Archive { for_share: Option<String> },
}

pub struct SpaceManager {
    registry: SqlitePool,
    base_dir: PathBuf,
    default_pool: SqlitePool,
    default_upload_dir: String,
    pools: Arc<Mutex<HashMap<String, SqlitePool>>>,
}

impl SpaceManager {
    pub async fn new(
        base_dir: &str,
        default_pool: SqlitePool,
        default_upload_dir: &str,
    ) -> Result<Self> {
        let base = PathBuf::from(base_dir);
        fs::create_dir_all(&base)
            .await
            .context("Failed to create spaces directory")?;

        let registry_path = base.join("spaces.db");
        let opts = SqliteConnectOptions::new()
            .filename(registry_path.to_string_lossy().to_string())
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);

        let registry = SqlitePoolOptions::new()
            .max_connections(3)
            .connect_with(opts)
            .await
            .context("Failed to open spaces registry")?;

        Self::migrate(&registry).await?;

        Ok(Self {
            registry,
            base_dir: base,
            default_pool,
            default_upload_dir: default_upload_dir.to_string(),
            pools: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    async fn migrate(registry: &SqlitePool) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS spaces (
                id TEXT PRIMARY KEY,
                label TEXT NOT NULL,
                owner_token TEXT UNIQUE NOT NULL,
                db_path TEXT NOT NULL,
                upload_dir TEXT NOT NULL,
                quota_bytes INTEGER NOT NULL DEFAULT 0,
                used_bytes INTEGER NOT NULL DEFAULT 0,
                status TEXT NOT NULL DEFAULT 'active',
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            )",
        )
        .execute(registry)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS share_tokens (
                id TEXT PRIMARY KEY,
                space_id TEXT NOT NULL REFERENCES spaces(id),
                share_token TEXT UNIQUE NOT NULL,
                label TEXT NOT NULL,
                can_write INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                expires_at TEXT
            )",
        )
        .execute(registry)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS archive_links (
                id TEXT PRIMARY KEY,
                space_id TEXT NOT NULL REFERENCES spaces(id),
                for_share_token TEXT,
                zip_path TEXT NOT NULL,
                download_token TEXT UNIQUE NOT NULL,
                expires_at TEXT NOT NULL,
                downloaded INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            )",
        )
        .execute(registry)
        .await?;

        Ok(())
    }

    pub async fn create(&self, label: &str, quota_mb: u64) -> Result<SpaceCreated> {
        let space_id = Uuid::new_v4().to_string();
        let owner_token = Uuid::new_v4().to_string().replace('-', "");
        let quota_bytes = quota_mb * 1024 * 1024;

        let space_dir = self.base_dir.join(&space_id);
        let upload_dir = space_dir.join("uploads");
        let db_path = space_dir.join("backpack.db");

        fs::create_dir_all(&upload_dir)
            .await
            .context("Failed to create space upload directory")?;

        let db_opts = SqliteConnectOptions::new()
            .filename(db_path.to_string_lossy().to_string())
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);

        let pool = SqlitePoolOptions::new()
            .max_connections(3)
            .connect_with(db_opts)
            .await
            .context("Failed to initialize space database")?;

        crate::db::init_db_pool(&pool).await?;

        sqlx::query(
            "INSERT INTO spaces (id, label, owner_token, db_path, upload_dir, quota_bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(&space_id)
        .bind(label)
        .bind(&owner_token)
        .bind(db_path.to_string_lossy().to_string())
        .bind(upload_dir.to_string_lossy().to_string())
        .bind(quota_bytes as i64)
        .execute(&self.registry)
        .await?;

        self.pools.lock().unwrap().insert(space_id.clone(), pool);

        info!("Space created: {} (label={}, quota={} MB)", space_id, label, quota_mb);

        Ok(SpaceCreated {
            space_id,
            label: label.to_string(),
            owner_token,
            quota_mb,
            upload_dir: upload_dir.to_string_lossy().to_string(),
            iroh_ticket: None,
        })
    }

    pub async fn share(&self, owner_token: &str, label: &str) -> Result<ShareCreated> {
        let row = sqlx::query(
            "SELECT id, label FROM spaces WHERE owner_token = ?1 AND status = 'active'",
        )
        .bind(owner_token)
        .fetch_optional(&self.registry)
        .await?;

        let (space_id, space_label): (String, String) = row
            .map(|r| (r.get("id"), r.get("label")))
            .ok_or_else(|| anyhow::anyhow!("Space not found or not active"))?;

        let share_token = Uuid::new_v4().to_string().replace('-', "");
        let share_id = Uuid::new_v4().to_string();

        sqlx::query(
            "INSERT INTO share_tokens (id, space_id, share_token, label)
             VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(&share_id)
        .bind(&space_id)
        .bind(&share_token)
        .bind(label)
        .execute(&self.registry)
        .await?;

        info!(
            "Share created: space={}, share_label={}, token={}",
            space_id, label, share_token
        );

        Ok(ShareCreated {
            share_token,
            label: label.to_string(),
            space_label,
        })
    }

    pub async fn delete(&self, token: &str, mode: DeleteMode) -> Result<DeleteResult> {
        let info = self.find_space_by_token(token).await?;
        let space_id = info.space_id;

        match mode {
            DeleteMode::Purge => {
                let space_dir = self.base_dir.join(&space_id);
                if space_dir.exists() {
                    fs::remove_dir_all(&space_dir)
                        .await
                        .context("Failed to remove space directory")?;
                }

                sqlx::query("DELETE FROM archive_links WHERE space_id = ?1")
                    .bind(&space_id)
                    .execute(&self.registry)
                    .await?;
                sqlx::query("DELETE FROM share_tokens WHERE space_id = ?1")
                    .bind(&space_id)
                    .execute(&self.registry)
                    .await?;
                sqlx::query("DELETE FROM spaces WHERE id = ?1")
                    .bind(&space_id)
                    .execute(&self.registry)
                    .await?;

                self.pools.lock().unwrap().remove(&space_id);

                info!("Space purged: {}", space_id);
                Ok(DeleteResult {
                    purged: true,
                    archive_path: None,
                    download_token: None,
                })
            }
            DeleteMode::Archive { for_share } => {
                let archive_id = Uuid::new_v4().to_string();
                let download_token = Uuid::new_v4().to_string().replace('-', "");
                let archives_dir = self.base_dir.join("archives");
                fs::create_dir_all(&archives_dir)
                    .await
                    .context("Failed to create archives directory")?;

                let zip_name = format!("{}.zip", &space_id);
                let zip_path = archives_dir.join(&zip_name);

                let space_dir = self.base_dir.join(&space_id);
                create_zip(&space_dir, &zip_path).await?;

                let expires_at = chrono::Utc::now()
                    .checked_add_signed(chrono::Duration::hours(24))
                    .unwrap()
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string();

                sqlx::query(
                    "INSERT INTO archive_links (id, space_id, for_share_token, zip_path, download_token, expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )
                .bind(&archive_id)
                .bind(&space_id)
                .bind(&for_share)
                .bind(zip_path.to_string_lossy().to_string())
                .bind(&download_token)
                .bind(&expires_at)
                .execute(&self.registry)
                .await?;

                sqlx::query("UPDATE spaces SET status = 'frozen' WHERE id = ?1")
                    .bind(&space_id)
                    .execute(&self.registry)
                    .await?;

                info!("Space archived: {} → {}", space_id, zip_path.display());

                Ok(DeleteResult {
                    purged: false,
                    archive_path: Some(zip_path.to_string_lossy().to_string()),
                    download_token: Some(download_token),
                })
            }
        }
    }

    pub async fn list(&self) -> Result<Vec<SpaceEntry>> {
        let rows = sqlx::query(
            r#"SELECT s.id, s.label, s.owner_token, s.quota_bytes, s.used_bytes, s.status,
               (SELECT COUNT(*) FROM share_tokens WHERE space_id = s.id) as share_count
               FROM spaces s ORDER BY s.created_at DESC"#,
        )
        .fetch_all(&self.registry)
        .await?;

        Ok(rows
            .iter()
            .map(|r| SpaceEntry {
                id: r.get("id"),
                label: r.get("label"),
                owner_token: r.get("owner_token"),
                quota_mb: r.get::<i64, _>("quota_bytes") as u64 / (1024 * 1024),
                used_mb: r.get::<i64, _>("used_bytes") as f64 / (1024.0 * 1024.0),
                shares: r.get::<i64, _>("share_count") as usize,
                status: r.get("status"),
            })
            .collect())
    }

    pub async fn info(&self, token: &str) -> Result<SpaceInfo> {
        let si = self.find_space_by_token(token).await?;

        let share_rows = sqlx::query(
            "SELECT share_token, label, can_write FROM share_tokens WHERE space_id = ?1",
        )
        .bind(&si.space_id)
        .fetch_all(&self.registry)
        .await?;

        let shares: Vec<ShareInfo> = share_rows
            .iter()
            .map(|r| ShareInfo {
                share_token: r.get("share_token"),
                label: r.get("label"),
                can_write: r.get::<i64, _>("can_write") != 0,
            })
            .collect();

        let archive_rows = sqlx::query(
            r#"SELECT id, for_share_token, download_token, expires_at, downloaded
               FROM archive_links WHERE space_id = ?1"#,
        )
        .bind(&si.space_id)
        .fetch_all(&self.registry)
        .await?;

        let archives: Vec<ArchiveInfo> = archive_rows
            .iter()
            .map(|r| ArchiveInfo {
                id: r.get("id"),
                for_share_label: r.get::<Option<String>, _>("for_share_token"),
                download_token: r.get("download_token"),
                expires_at: r.get("expires_at"),
                downloaded: r.get::<i64, _>("downloaded") != 0,
            })
            .collect();

        let row = sqlx::query(
            "SELECT id, label, owner_token, quota_bytes, used_bytes, status, created_at
             FROM spaces WHERE id = ?1",
        )
        .bind(&si.space_id)
        .fetch_one(&self.registry)
        .await?;

        Ok(SpaceInfo {
            id: row.get("id"),
            label: row.get("label"),
            owner_token: row.get("owner_token"),
            quota_mb: row.get::<i64, _>("quota_bytes") as u64 / (1024 * 1024),
            used_mb: row.get::<i64, _>("used_bytes") as f64 / (1024.0 * 1024.0),
            status: row.get("status"),
            shares,
            archives,
            created_at: row.get("created_at"),
        })
    }

    pub async fn resolve(&self, token: Option<&str>) -> Result<SpaceHandle> {
        let token = match token {
            Some(t) if !t.is_empty() => t,
            _ => {
                return Ok(SpaceHandle {
                    space_id: "default".into(),
                    pool: self.default_pool.clone(),
                    upload_dir: self.default_upload_dir.clone(),
                    quota_bytes: 0,
                    iroh_ticket: None,
                });
            }
        };

        let si = self.find_space_by_token(token).await?;

        if si.status != "active" {
            anyhow::bail!("Space is not active (status: {})", si.status);
        }

        let cached = {
            let cache = self.pools.lock().unwrap();
            cache.get(&si.space_id).cloned()
        };

        let pool = if let Some(p) = cached {
            p
        } else {
            let db_opts = SqliteConnectOptions::new()
                .filename(&si.db_path)
                .create_if_missing(false)
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);

            let new_pool = SqlitePoolOptions::new()
                .max_connections(3)
                .connect_with(db_opts)
                .await?;
            self.pools
                .lock()
                .unwrap()
                .insert(si.space_id.clone(), new_pool.clone());
            new_pool
        };

        Ok(SpaceHandle {
            space_id: si.space_id,
            pool,
            upload_dir: si.upload_dir,
            quota_bytes: si.quota_bytes,
            iroh_ticket: None,
        })
    }

    /// Check whether a space has been shared with at least one other person.
    /// Returns `true` if there is at least one active share token for the space.
    /// Used by the sync hub to gate real-time push — private spaces don't need
    /// WebSocket broadcast since only one user has access.
    pub async fn is_shared(&self, space_id: &str) -> Result<bool> {
        if space_id == "default" || space_id.is_empty() {
            return Ok(false);
        }
        let row = sqlx::query(
            "SELECT COUNT(*) as cnt FROM share_tokens
             JOIN spaces ON share_tokens.space_id = spaces.id
             WHERE share_tokens.space_id = ?1 AND spaces.status = 'active'",
        )
        .bind(space_id)
        .fetch_one(&self.registry)
        .await?;

        Ok(row.get::<i64, _>("cnt") > 0)
    }

    pub async fn add_usage(&self, space_id: &str, bytes: u64) -> Result<()> {
        if space_id == "default" || space_id.is_empty() {
            return Ok(());
        }
        sqlx::query("UPDATE spaces SET used_bytes = used_bytes + ?1 WHERE id = ?2")
            .bind(bytes as i64)
            .bind(space_id)
            .execute(&self.registry)
            .await?;
        Ok(())
    }

    pub async fn check_quota(&self, space_id: &str, add_bytes: u64) -> Result<bool> {
        if space_id == "default" || space_id.is_empty() {
            return Ok(true);
        }
        let row = sqlx::query("SELECT quota_bytes, used_bytes FROM spaces WHERE id = ?1")
            .bind(space_id)
            .fetch_optional(&self.registry)
            .await?;

        match row {
            Some(r) => {
                let quota: i64 = r.get("quota_bytes");
                let used: i64 = r.get("used_bytes");
                if quota == 0 {
                    Ok(true)
                } else {
                    Ok((used + add_bytes as i64) <= quota)
                }
            }
            None => Ok(false),
        }
    }

    pub async fn get_archive_info(
        &self,
        download_token: &str,
    ) -> Result<Option<(String, String, String)>> {
        let row = sqlx::query(
            "SELECT a.zip_path, a.expires_at, a.downloaded, a.for_share_token, s.status
             FROM archive_links a JOIN spaces s ON a.space_id = s.id
             WHERE a.download_token = ?1",
        )
        .bind(download_token)
        .fetch_optional(&self.registry)
        .await?;

        match row {
            Some(r) => {
                let zip_path: String = r.get("zip_path");
                let expires_at: String = r.get("expires_at");
                let downloaded: i64 = r.get("downloaded");
                let for_share_token: Option<String> = r.get("for_share_token");

                if downloaded != 0 {
                    anyhow::bail!("Archive already downloaded");
                }

                Ok(Some((zip_path, expires_at, for_share_token.unwrap_or_default())))
            }
            None => Ok(None),
        }
    }

    pub async fn mark_archive_downloaded(&self, download_token: &str) -> Result<()> {
        sqlx::query("UPDATE archive_links SET downloaded = 1 WHERE download_token = ?1")
            .bind(download_token)
            .execute(&self.registry)
            .await?;
        Ok(())
    }

    #[allow(dead_code)]
    pub async fn set_space_iroh(&self, space_id: &str, ticket: &str) -> Result<()> {
        sqlx::query("UPDATE spaces SET iroh_ticket = ?1 WHERE id = ?2")
            .bind(ticket)
            .bind(space_id)
            .execute(&self.registry)
            .await?;
        Ok(())
    }

    async fn find_space_by_token(&self, token: &str) -> Result<SpaceDbInfo> {
        let row = sqlx::query(
            "SELECT id, db_path, upload_dir, quota_bytes, status FROM spaces
             WHERE owner_token = ?1 AND status != 'deleted'
             UNION ALL
             SELECT s.id, s.db_path, s.upload_dir, s.quota_bytes, s.status
             FROM share_tokens st JOIN spaces s ON st.space_id = s.id
             WHERE st.share_token = ?1 AND s.status != 'deleted'",
        )
        .bind(token)
        .fetch_optional(&self.registry)
        .await?;

        row.map(|r| SpaceDbInfo {
            space_id: r.get("id"),
            db_path: r.get("db_path"),
            upload_dir: r.get("upload_dir"),
            quota_bytes: r.get::<i64, _>("quota_bytes") as u64,
            status: r.get("status"),
        })
        .ok_or_else(|| anyhow::anyhow!("Invalid or unknown space token"))
    }
}

struct SpaceDbInfo {
    space_id: String,
    db_path: String,
    upload_dir: String,
    quota_bytes: u64,
    status: String,
}

async fn create_zip(source_dir: &Path, zip_path: &Path) -> Result<()> {
    let file = std::fs::File::create(zip_path)
        .with_context(|| format!("Failed to create ZIP at {}", zip_path.display()))?;

    let mut zip_writer = zip::ZipWriter::new(file);
    let options =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    let mut paths: Vec<PathBuf> = Vec::new();
    if source_dir.exists() {
        let mut entries = fs::read_dir(source_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            paths.push(entry.path());
        }
    }

    for path in &paths {
        if path.is_dir() {
            add_dir_to_zip(&mut zip_writer, path, path, options)?;
        } else if path.is_file() {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file");
            zip_writer
                .start_file(name, options)
                .context("Failed to add file to ZIP")?;
            let data = std::fs::read(path).context("Failed to read file for ZIP")?;
            std::io::Write::write_all(&mut zip_writer, &data).context("Failed to write to ZIP")?;
        }
    }

    zip_writer.finish().context("Failed to finalize ZIP")?;
    Ok(())
}

fn add_dir_to_zip<W: std::io::Write + std::io::Seek>(
    zip: &mut zip::ZipWriter<W>,
    base: &Path,
    dir: &Path,
    options: zip::write::FileOptions,
) -> Result<()> {
    let mut entries = std::fs::read_dir(dir)?;
    while let Some(entry) = entries.next() {
        let entry = entry?;
        let path = entry.path();
        let rel = path.strip_prefix(base).unwrap_or(&path);
        let name = rel.to_string_lossy().to_string();

        if path.is_dir() {
            zip.add_directory(name, options)?;
            add_dir_to_zip(zip, base, &path, options)?;
        } else if path.is_file() {
            zip.start_file(name, options)?;
            let data = std::fs::read(&path)?;
            std::io::Write::write_all(zip, &data)?;
        }
    }
    Ok(())
}
