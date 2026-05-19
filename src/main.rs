mod catalog;
mod config;
mod db;
mod extraction;
mod handlers;
mod iroh;
mod vector;

use std::sync::Arc;

use axum::{routing, Router};
use clap::Parser;
use reqwest::Client;
use sqlx::SqlitePool;
use tower_http::cors::{Any, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;
use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::handlers::AppState;

#[derive(Parser)]
#[command(name = "backpack", about = "AI Cloud Backpack — personal file catalog with LLM")]
struct Cli {
    /// Enable Iroh P2P connectivity (DHT discovery, no relays, no IP in ticket)
    #[arg(long)]
    iroh: bool,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let _ = dotenvy::dotenv();

    let cli = Cli::parse();

    let config = config::Config::from_env().expect("Failed to load configuration");
    info!("Starting AI Cloud Backpack");
    info!("LLM provider: {}", config.llm_provider);
    info!("LLM model: {}", config.llm_model);
    info!("Embedding model: {}", config.embedding_model);
    info!("Upload dir: {}", config.upload_dir);
    info!("DB path: {}", config.db_path);

    let skill_content = catalog::read_skill_prompt(&config.skill_path).unwrap_or_else(|e| {
        tracing::warn!("Could not read skill.md: {}. Using default prompt.", e);
        String::new()
    });
    info!(
        "Loaded skill prompt ({} bytes)",
        skill_content.len()
    );

    std::fs::create_dir_all(&config.upload_dir).expect("Failed to create upload dir");
    if let Some(parent) = std::path::Path::new(&config.db_path).parent() {
        std::fs::create_dir_all(parent).expect("Failed to create db directory");
    }

    let pool: SqlitePool = db::init_db(&config.db_path)
        .await
        .expect("Failed to initialize database");
    info!("Database initialized");

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .expect("Failed to create HTTP client");

    let state = AppState {
        pool: pool.clone(),
        config: Arc::new(config.clone()),
        client,
    };

    let max_bytes = config.max_file_size_mb * 1024 * 1024;

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/upload", routing::post(handlers::upload_handler))
        .route("/search", routing::get(handlers::search_handler))
        .route("/ask", routing::post(handlers::ask_handler))
        .route("/inventory", routing::get(handlers::inventory_handler))
        .route("/download/:id", routing::get(handlers::download_handler))
        .route("/files/:id", routing::delete(handlers::delete_handler))
        .route("/", routing::get(|| async {
            axum::Json(serde_json::json!({
                "name": "AI Cloud Backpack",
                "version": env!("CARGO_PKG_VERSION"),
                "endpoints": {
                    "upload": "POST /upload",
                    "search": "GET /search?q=...",
                    "ask": "POST /ask",
                    "inventory": "GET /inventory",
                    "download": "GET /download/{id}",
                    "delete": "DELETE /files/{id}"
                }
            }))
        }))
        .layer(RequestBodyLimitLayer::new(max_bytes as usize))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());
    info!("HTTP listening on {}", addr);

    if cli.iroh {
        let iroh_server = iroh::IrohServer::new().await.expect("Failed to start Iroh");
        let axum_port: u16 = addr
            .split(':')
            .last()
            .and_then(|p| p.parse().ok())
            .unwrap_or(8080);

        info!("NodeId:  {}", iroh_server.node_id());
        info!("Ticket:  {}", iroh_server.ticket());
        info!(" ─────────────────────────────────────────────");
        info!(" Share the ticket to grant P2P access.");
        info!(" No relay used — direct QUIC connections only.");
        info!(" ─────────────────────────────────────────────");

        let bridge = iroh_server.bridge_loop(axum_port);

        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .expect("Failed to bind HTTP address");

        tokio::select! {
            _ = axum::serve(listener, app) => {},
            _ = bridge => {},
        }
    } else {
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .expect("Failed to bind HTTP address");

        axum::serve(listener, app).await.expect("Server error");
    }
}
