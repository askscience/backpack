use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{SqlitePool, Row};
use std::path::PathBuf;
use std::str::FromStr;

use super::types::SyncEntry;

/// Persists the local sync state — which files are tracked, their hashes,
/// and the corresponding remote file IDs on the backpack server.
pub struct SyncState {
    pool: SqlitePool,
}

impl SyncState {
    pub async fn open(watch_dir: &str) -> Result<Self> {
        let db_path = PathBuf::from(watch_dir).join(".backpack-sync.db");
        let db_path_str = db_path.to_string_lossy().to_string();
        let opts = SqliteConnectOptions::from_str(&db_path_str)?
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await
            .with_context(|| format!("Failed to open sync state DB at {}", db_path.display()))?;
        let state = Self { pool };
        state.init_schema().await?;
        Ok(state)
    }

    async fn init_schema(&self) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS sync_entries (
                relative_path   TEXT PRIMARY KEY,
                content_hash    TEXT NOT NULL,
                file_size       INTEGER NOT NULL DEFAULT 0,
                remote_file_id  TEXT,
                remote_file_size INTEGER,
                sync_status     TEXT NOT NULL DEFAULT 'synced',
                last_local_modified INTEGER NOT NULL DEFAULT 0,
                last_synced_at  TEXT,
                created_at      TEXT NOT NULL DEFAULT (datetime('now'))
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .context("Failed to create sync_entries table")?;
        Ok(())
    }

    pub async fn upsert(&self, entry: &SyncEntry) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO sync_entries
                (relative_path, content_hash, file_size, remote_file_id, remote_file_size,
                 sync_status, last_local_modified, last_synced_at, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, COALESCE(
                (SELECT created_at FROM sync_entries WHERE relative_path = ?1),
                datetime('now')
            ))
            ON CONFLICT(relative_path) DO UPDATE SET
                content_hash     = excluded.content_hash,
                file_size        = excluded.file_size,
                remote_file_id   = excluded.remote_file_id,
                remote_file_size = excluded.remote_file_size,
                sync_status      = excluded.sync_status,
                last_local_modified = excluded.last_local_modified,
                last_synced_at   = excluded.last_synced_at
            "#,
        )
        .bind(&entry.relative_path)
        .bind(&entry.content_hash)
        .bind(entry.file_size)
        .bind(&entry.remote_file_id)
        .bind(entry.remote_file_size)
        .bind(&entry.sync_status)
        .bind(entry.last_local_modified)
        .bind(&entry.last_synced_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_by_path(&self, relative_path: &str) -> Result<Option<SyncEntry>> {
        let row = sqlx::query("SELECT * FROM sync_entries WHERE relative_path = ?1")
            .bind(relative_path)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| row_to_entry(&r)))
    }

    pub async fn get_by_remote_id(&self, remote_file_id: &str) -> Result<Option<SyncEntry>> {
        let row = sqlx::query("SELECT * FROM sync_entries WHERE remote_file_id = ?1")
            .bind(remote_file_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| row_to_entry(&r)))
    }

    pub async fn list_all(&self) -> Result<Vec<SyncEntry>> {
        let rows = sqlx::query("SELECT * FROM sync_entries ORDER BY relative_path")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(|r| row_to_entry(r)).collect())
    }

    pub async fn delete_by_path(&self, relative_path: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM sync_entries WHERE relative_path = ?1")
            .bind(relative_path)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    #[allow(dead_code)]
    pub async fn count_by_status(&self, status: &str) -> Result<usize> {
        let row = sqlx::query("SELECT COUNT(*) as cnt FROM sync_entries WHERE sync_status = ?1")
            .bind(status)
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get::<i64, _>("cnt") as usize)
    }

    #[allow(dead_code)]
    pub async fn total_count(&self) -> Result<usize> {
        let row = sqlx::query("SELECT COUNT(*) as cnt FROM sync_entries")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get::<i64, _>("cnt") as usize)
    }
}

fn row_to_entry(row: &sqlx::sqlite::SqliteRow) -> SyncEntry {
    SyncEntry {
        relative_path: row.get("relative_path"),
        content_hash: row.get("content_hash"),
        file_size: row.get("file_size"),
        remote_file_id: row.get("remote_file_id"),
        remote_file_size: row.get("remote_file_size"),
        sync_status: row.get("sync_status"),
        last_local_modified: row.get("last_local_modified"),
        last_synced_at: row.get("last_synced_at"),
        created_at: row.get("created_at"),
    }
}
