use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::config::Config;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub title: String,
    pub summary: String,
    pub tags: Vec<String>,
    pub category: String,
}

#[derive(Debug, Deserialize)]
struct OpenAiChatResponse {
    choices: Vec<OpenAiChoice>,
}
#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
}
#[derive(Debug, Deserialize)]
struct OpenAiMessage {
    content: String,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContent>,
}
#[derive(Debug, Deserialize)]
struct AnthropicContent {
    text: String,
}

#[derive(Debug, Deserialize)]
struct OllamaChatResponse {
    message: OllamaMessage,
}
#[derive(Debug, Deserialize)]
struct OllamaMessage {
    content: String,
}

pub fn read_skill_prompt(skill_path: &str) -> Result<String> {
    std::fs::read_to_string(skill_path)
        .with_context(|| format!("Failed to read skill prompt file at {}", skill_path))
}

pub async fn catalog_file(
    client: &Client,
    config: &Config,
    text: &str,
) -> Result<CatalogEntry> {
    let skill = read_skill_prompt(&config.skill_path)?;
    let response = call_chat_completion(client, config, &skill, text).await?;
    parse_catalog_response(&response)
}

async fn call_chat_completion(
    client: &Client,
    config: &Config,
    system: &str,
    user: &str,
) -> Result<String> {
    match config.llm_provider.as_str() {
        "anthropic" => call_anthropic(client, config, system, user).await,
        "ollama" => call_ollama(client, config, system, user).await,
        _ => call_openai_compatible(client, config, system, user).await,
    }
}

async fn call_openai_compatible(
    client: &Client,
    config: &Config,
    system: &str,
    user: &str,
) -> Result<String> {
    let url = format!("{}/chat/completions", config.llm_endpoint.trim_end_matches('/'));

    let body = serde_json::json!({
        "model": config.llm_model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ],
        "temperature": 0.1,
        "max_tokens": 1024
    });

    debug!("Calling LLM: {}", url);

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", config.llm_api_key))
        .json(&body)
        .send()
        .await
        .context("Failed to send LLM request")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("LLM request failed ({}): {}", status, text));
    }

    let parsed: OpenAiChatResponse = resp
        .json()
        .await
        .context("Failed to parse LLM response")?;

    parsed
        .choices
        .first()
        .map(|c| c.message.content.clone())
        .ok_or_else(|| anyhow::anyhow!("Empty LLM response"))
}

async fn call_anthropic(
    client: &Client,
    config: &Config,
    system: &str,
    user: &str,
) -> Result<String> {
    let url = format!("{}/messages", config.llm_endpoint.trim_end_matches('/'));

    let body = serde_json::json!({
        "model": config.llm_model,
        "max_tokens": 1024,
        "system": system,
        "messages": [
            {"role": "user", "content": user}
        ]
    });

    let resp = client
        .post(&url)
        .header("x-api-key", &config.llm_api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&body)
        .send()
        .await
        .context("Failed to send Anthropic request")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!(
            "Anthropic request failed ({}): {}",
            status,
            text
        ));
    }

    let parsed: AnthropicResponse = resp
        .json()
        .await
        .context("Failed to parse Anthropic response")?;

    parsed
        .content
        .first()
        .map(|c| c.text.clone())
        .ok_or_else(|| anyhow::anyhow!("Empty Anthropic response"))
}

async fn call_ollama(
    client: &Client,
    config: &Config,
    system: &str,
    user: &str,
) -> Result<String> {
    let base = config.llm_endpoint.trim_end_matches('/').trim_end_matches("/v1");

    let body = serde_json::json!({
        "model": config.llm_model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ],
        "stream": false,
        "options": {"temperature": 0.1}
    });

    let url = format!("{}/api/chat", base);
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .context("Failed to send Ollama request")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!(
            "Ollama request failed ({}): {}",
            status,
            text
        ));
    }

    let raw = resp.text().await.context("Failed to read Ollama response")?;

    if let Ok(parsed) = serde_json::from_str::<OllamaChatResponse>(&raw) {
        return Ok(parsed.message.content);
    }
    if let Ok(parsed) = serde_json::from_str::<OpenAiChatResponse>(&raw) {
        return Ok(parsed
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .unwrap_or_default());
    }

    Err(anyhow::anyhow!("Failed to parse Ollama response: {}", raw))
}

fn parse_catalog_response(raw: &str) -> Result<CatalogEntry> {
    let cleaned = raw
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    let entry: CatalogEntry = serde_json::from_str(cleaned)
        .context("Failed to parse catalog JSON from LLM response")?;
    Ok(entry)
}

pub async fn get_embedding(
    client: &Client,
    config: &Config,
    text: &str,
) -> Result<Vec<f32>> {
    match config.llm_provider.as_str() {
        "ollama" => get_embedding_ollama(client, config, text).await,
        _ => get_embedding_openai_compatible(client, config, text).await,
    }
}

async fn get_embedding_openai_compatible(
    client: &Client,
    config: &Config,
    text: &str,
) -> Result<Vec<f32>> {
    let endpoint = std::env::var("EMBEDDING_ENDPOINT")
        .unwrap_or_else(|_| config.llm_endpoint.clone());
    let api_key = std::env::var("EMBEDDING_API_KEY")
        .unwrap_or_else(|_| config.llm_api_key.clone());
    let url = format!("{}/embeddings", endpoint.trim_end_matches('/'));

    let body = serde_json::json!({
        "model": config.embedding_model,
        "input": text
    });

    debug!("Getting embedding from: {}", url);

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&body)
        .send()
        .await
        .context("Failed to get embedding")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let err_text = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!(
            "Embedding request failed ({}): {}",
            status,
            err_text
        ));
    }

    let raw = resp.text().await.context("Failed to read embedding response")?;

    let parsed: serde_json::Value =
        serde_json::from_str(&raw).context("Failed to parse embedding response")?;

    let embedding = parsed["data"][0]["embedding"]
        .as_array()
        .ok_or_else(|| {
            warn!("Unexpected embedding response format: {}", raw);
            anyhow::anyhow!("Missing embedding in response")
        })?;

    Ok(embedding.iter().map(|v| v.as_f64().unwrap_or(0.0) as f32).collect())
}

async fn get_embedding_ollama(
    client: &Client,
    config: &Config,
    text: &str,
) -> Result<Vec<f32>> {
    let base = config.llm_endpoint.trim_end_matches('/').trim_end_matches("/v1");
    let url = format!("{}/api/embeddings", base);

    let body = serde_json::json!({
        "model": config.embedding_model,
        "prompt": text
    });

    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .context("Failed to get Ollama embedding")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let err_text = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!(
            "Ollama embedding failed ({}): {}",
            status,
            err_text
        ));
    }

    let raw = resp.text().await.context("Failed to read Ollama embedding response")?;
    let parsed: serde_json::Value =
        serde_json::from_str(&raw).context("Failed to parse Ollama embedding response")?;

    let embedding = parsed["embedding"]
        .as_array()
        .or_else(|| parsed["data"][0]["embedding"].as_array())
        .ok_or_else(|| {
            warn!("Unexpected Ollama embedding response: {}", raw);
            anyhow::anyhow!("Missing embedding in Ollama response")
        })?;

    Ok(embedding.iter().map(|v| v.as_f64().unwrap_or(0.0) as f32).collect())
}

pub async fn call_chat_with_context(
    client: &Client,
    config: &Config,
    context: &str,
    question: &str,
) -> Result<String> {
    let system = r#"You are an intelligent assistant that answers questions based on the user's personal file collection.
Answer the question using ONLY the provided context. If the context doesn't contain enough information, say so clearly.
Be concise and cite which files you used in your answer when relevant."#;

    let user = format!(
        "Context from user's files:\n\n{}\n\nQuestion: {}",
        context, question
    );

    call_chat_completion(client, config, system, &user).await
}
