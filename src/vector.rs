use anyhow::Result;
use sqlx::SqlitePool;
use crate::db;

pub async fn search_similar(
    pool: &SqlitePool,
    query_embedding: &[f32],
    top_k: usize,
) -> Result<Vec<db::SearchResult>> {
    let all = db::get_all_embeddings(pool).await?;
    if all.is_empty() {
        return Ok(vec![]);
    }

    let mut scored: Vec<(String, f32)> = all
        .iter()
        .map(|(id, emb)| (id.clone(), cosine_similarity(query_embedding, emb)))
        .collect();

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(top_k);

    let mut results = Vec::new();
    for (id, score) in &scored {
        if let Ok(Some(file)) = db::get_file(pool, id).await {
            results.push(db::SearchResult {
                id: file.id,
                original_name: file.original_name,
                mime: file.mime,
                file_size: file.file_size,
                title: file.title,
                summary: file.summary,
                tags: file.tags,
                category: file.category,
                created_at: file.created_at,
                score: *score,
            });
        }
    }

    Ok(results)
}

pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    if len == 0 {
        return 0.0;
    }

    let mut dot = 0.0f64;
    let mut norm_a = 0.0f64;
    let mut norm_b = 0.0f64;

    for i in 0..len {
        let ai = a[i] as f64;
        let bi = b[i] as f64;
        dot += ai * bi;
        norm_a += ai * ai;
        norm_b += bi * bi;
    }

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    (dot / (norm_a.sqrt() * norm_b.sqrt())) as f32
}
