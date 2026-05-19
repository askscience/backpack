use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{SqlitePool, Row};
use std::str::FromStr;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FileRecord {
    pub id: String,
    pub original_name: String,
    pub mime: String,
    pub file_path: String,
    pub file_size: i64,
    pub extracted_text: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub tags: Option<String>,
    pub category: Option<String>,
    pub catalog_json: Option<String>,
    pub embedding: Option<Vec<u8>>,
    pub created_at: String,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct FileMetadata {
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

#[derive(Clone, Debug, serde::Serialize)]
pub struct SearchResult {
    pub id: String,
    pub original_name: String,
    pub mime: String,
    pub file_size: i64,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub tags: Option<String>,
    pub category: Option<String>,
    pub created_at: String,
    pub score: f32,
}

pub async fn init_db(db_url: &str) -> Result<SqlitePool> {
    let opts = SqliteConnectOptions::from_str(db_url)?
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(opts)
        .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS files (
            id TEXT PRIMARY KEY,
            original_name TEXT NOT NULL,
            mime TEXT NOT NULL,
            file_path TEXT NOT NULL,
            file_size INTEGER NOT NULL DEFAULT 0,
            extracted_text TEXT NOT NULL DEFAULT '',
            title TEXT,
            summary TEXT,
            tags TEXT,
            category TEXT,
            catalog_json TEXT,
            embedding BLOB,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        )
        "#,
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_files_category ON files(category);
        "#,
    )
    .execute(&pool)
    .await?;

    Ok(pool)
}

pub async fn insert_file(pool: &SqlitePool, record: &FileRecord) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO files (id, original_name, mime, file_path, file_size, extracted_text, title, summary, tags, category, catalog_json, embedding, created_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
        "#,
    )
    .bind(&record.id)
    .bind(&record.original_name)
    .bind(&record.mime)
    .bind(&record.file_path)
    .bind(record.file_size)
    .bind(&record.extracted_text)
    .bind(&record.title)
    .bind(&record.summary)
    .bind(&record.tags)
    .bind(&record.category)
    .bind(&record.catalog_json)
    .bind(&record.embedding)
    .bind(&record.created_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_file(pool: &SqlitePool, id: &str) -> Result<Option<FileRecord>> {
    let row = sqlx::query("SELECT * FROM files WHERE id = ?1")
        .bind(id)
        .fetch_optional(pool)
        .await?;

    Ok(row.map(|r| FileRecord {
        id: r.get("id"),
        original_name: r.get("original_name"),
        mime: r.get("mime"),
        file_path: r.get("file_path"),
        file_size: r.get("file_size"),
        extracted_text: r.get("extracted_text"),
        title: r.get("title"),
        summary: r.get("summary"),
        tags: r.get("tags"),
        category: r.get("category"),
        catalog_json: r.get("catalog_json"),
        embedding: r.get("embedding"),
        created_at: r.get("created_at"),
    }))
}

pub async fn delete_file(pool: &SqlitePool, id: &str) -> Result<bool> {
    let result = sqlx::query("DELETE FROM files WHERE id = ?1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn list_files(pool: &SqlitePool) -> Result<Vec<FileMetadata>> {
    let rows = sqlx::query(
        "SELECT id, original_name, mime, file_size, title, summary, tags, category, created_at, embedding FROM files ORDER BY created_at DESC",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .iter()
        .map(|r| FileMetadata {
            id: r.get("id"),
            original_name: r.get("original_name"),
            mime: r.get("mime"),
            file_size: r.get("file_size"),
            title: r.get("title"),
            summary: r.get("summary"),
            tags: r.get("tags"),
            category: r.get("category"),
            created_at: r.get("created_at"),
            has_embedding: {
                let emb: Option<Vec<u8>> = r.get("embedding");
                emb.is_some()
            },
        })
        .collect())
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct CategoryGroup {
    pub category: String,
    pub count: usize,
    pub files: Vec<FileMetadata>,
}

pub async fn list_files_grouped(pool: &SqlitePool) -> Result<Vec<CategoryGroup>> {
    let files = list_files(pool).await?;
    let mut groups: std::collections::BTreeMap<String, Vec<FileMetadata>> =
        std::collections::BTreeMap::new();

    for file in files {
        let cat = file.category.clone().unwrap_or_else(|| "other".into());
        groups.entry(cat).or_default().push(file);
    }

    Ok(groups
        .into_iter()
        .map(|(category, files)| {
            let count = files.len();
            CategoryGroup {
                category,
                count,
                files,
            }
        })
        .collect())
}

pub async fn get_all_embeddings(pool: &SqlitePool) -> Result<Vec<(String, Vec<f32>)>> {
    let rows =
        sqlx::query("SELECT id, embedding FROM files WHERE embedding IS NOT NULL")
            .fetch_all(pool)
            .await?;

    let mut results = Vec::new();
    for row in rows {
        let id: String = row.get("id");
        let blob: Vec<u8> = row.get("embedding");
        let floats = bytes_to_f32_vec(&blob);
        if !floats.is_empty() {
            results.push((id, floats));
        }
    }
    Ok(results)
}

fn bytes_to_f32_vec(bytes: &[u8]) -> Vec<f32> {
    if bytes.len() % 4 != 0 {
        return vec![];
    }
    let count = bytes.len() / 4;
    let mut vec = Vec::with_capacity(count);
    for i in 0..count {
        let start = i * 4;
        let raw: [u8; 4] = bytes[start..start + 4].try_into().unwrap();
        vec.push(f32::from_le_bytes(raw));
    }
    vec
}

pub fn f32_vec_to_bytes(vec: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vec.len() * 4);
    for &f in vec {
        bytes.extend_from_slice(&f.to_le_bytes());
    }
    bytes
}

pub async fn get_full_texts(pool: &SqlitePool, ids: &[String]) -> Result<Vec<FileRecord>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let placeholders: Vec<String> = ids.iter().enumerate().map(|(i, _)| format!("?{}", i + 1)).collect();
    let query = format!(
        "SELECT id, original_name, mime, file_path, file_size, extracted_text, title, summary, tags, category, catalog_json, embedding, created_at FROM files WHERE id IN ({})",
        placeholders.join(",")
    );

    let mut q = sqlx::query(&query);
    for id in ids {
        q = q.bind(id);
    }

    let rows = q.fetch_all(pool).await?;
    Ok(rows
        .iter()
        .map(|r| FileRecord {
            id: r.get("id"),
            original_name: r.get("original_name"),
            mime: r.get("mime"),
            file_path: r.get("file_path"),
            file_size: r.get("file_size"),
            extracted_text: r.get("extracted_text"),
            title: r.get("title"),
            summary: r.get("summary"),
            tags: r.get("tags"),
            category: r.get("category"),
            catalog_json: r.get("catalog_json"),
            embedding: r.get("embedding"),
            created_at: r.get("created_at"),
        })
        .collect())
}
