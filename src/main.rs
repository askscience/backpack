mod catalog;
mod config;
mod db;
mod extraction;
mod handlers;
mod iroh;
mod spaces;
mod vector;

use std::sync::Arc;

use axum::{routing, Router};
use clap::{Parser, Subcommand};
use reqwest::Client;
use sqlx::SqlitePool;
use tower_http::cors::{Any, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;
use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::handlers::AppState;
use crate::spaces::{DeleteMode, SpaceManager};

#[derive(Parser)]
#[command(name = "backpack", about = "AI Cloud Backpack — personal file catalog with LLM")]
struct Cli {
    /// Enable Iroh P2P connectivity (DHT discovery, no relays, no IP in ticket)
    #[arg(long)]
    iroh: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage isolated user spaces
    Space {
        #[command(subcommand)]
        action: SpaceAction,
    },
}

#[derive(Subcommand)]
enum SpaceAction {
    /// Create a new isolated space
    Create {
        /// Label for the space
        #[arg(long)]
        label: String,
        /// Quota in megabytes (0 = unlimited)
        #[arg(long, default_value = "0")]
        quota: u64,
    },
    /// Share an existing space with another person
    Share {
        /// Owner token of the space to share
        owner_token: String,
        /// Label for this share (e.g. name of the person)
        #[arg(long)]
        label: String,
    },
    /// List all spaces
    List,
    /// Show details of a space
    Info {
        /// Space token (owner or share)
        token: String,
    },
    /// Delete a space permanently or with archive
    Delete {
        /// Space token (owner or share)
        token: String,
        /// Permanently wipe the space
        #[arg(long)]
        purge: bool,
        /// Create a ZIP archive before deleting
        #[arg(long)]
        archive: bool,
        /// If --archive, restrict download to this share token
        #[arg(long, requires = "archive")]
        for_share: Option<String>,
    },
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

    // ── CLI-only mode: space management ─────────────────────────────
    if let Some(Commands::Space { action }) = cli.command {
        let base = std::env::var("SPACES_DIR").unwrap_or_else(|_| "./spaces".into());
        let default_pool = db::init_db(&config.db_path)
            .await
            .expect("Failed to init default DB");

        let manager = SpaceManager::new(&base, default_pool, &config.upload_dir)
            .await
            .expect("Failed to init space manager");

        match action {
            SpaceAction::Create { label, quota } => {
                let created = manager.create(&label, quota).await.expect("Failed to create space");
                serde_json::to_writer_pretty(std::io::stdout(), &created).unwrap();
                println!();
            }
            SpaceAction::Share { owner_token, label } => {
                let share = manager.share(&owner_token, &label).await.expect("Failed to share space");
                serde_json::to_writer_pretty(std::io::stdout(), &share).unwrap();
                println!();
            }
            SpaceAction::List => {
                let list = manager.list().await.expect("Failed to list spaces");
                if list.is_empty() {
                    println!("No spaces created yet.");
                } else {
                    for entry in &list {
                        println!(
                            "  {:<12}  {:>8.1} / {:>4} MB  {:<8}  shares: {}  label: {}",
                            entry.id[..12.min(entry.id.len())].to_string(),
                            entry.used_mb,
                            entry.quota_mb,
                            entry.status,
                            entry.shares,
                            entry.label,
                        );
                    }
                }
            }
            SpaceAction::Info { token } => {
                let info = manager.info(&token).await.expect("Failed to get space info");
                serde_json::to_writer_pretty(std::io::stdout(), &info).unwrap();
                println!();
            }
            SpaceAction::Delete { token, purge, archive, for_share } => {
                let mode = if purge {
                    DeleteMode::Purge
                } else if archive {
                    DeleteMode::Archive { for_share }
                } else {
                    eprintln!("Must specify --purge or --archive");
                    std::process::exit(1);
                };
                let result = manager.delete(&token, mode).await.expect("Failed to delete space");
                if result.purged {
                    println!("Space permanently deleted.");
                } else {
                    println!("Archive created: {}", result.archive_path.unwrap_or_default());
                    println!("Download link: /archive/dl/{}", result.download_token.unwrap_or_default());
                    println!("Expires in 24 hours.");
                }
            }
        }
        return;
    }

    // ── Server mode ─────────────────────────────────────────────────

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
    info!("Loaded skill prompt ({} bytes)", skill_content.len());

    std::fs::create_dir_all(&config.upload_dir).expect("Failed to create upload dir");
    if let Some(parent) = std::path::Path::new(&config.db_path).parent() {
        std::fs::create_dir_all(parent).expect("Failed to create db directory");
    }

    let default_pool: SqlitePool = db::init_db(&config.db_path)
        .await
        .expect("Failed to initialize database");
    info!("Database initialized");

    let spaces_base = std::env::var("SPACES_DIR").unwrap_or_else(|_| "./spaces".into());
    let space_manager = SpaceManager::new(&spaces_base, default_pool.clone(), &config.upload_dir)
        .await
        .expect("Failed to initialize space manager");
    info!("Space manager initialized");

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .expect("Failed to create HTTP client");

    let state = AppState {
        pool: default_pool.clone(),
        config: Arc::new(config.clone()),
        client,
        spaces: Arc::new(space_manager),
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
        .route("/archive/dl/:id", routing::get(handlers::archive_download_handler))
        .route("/", routing::get(|| async {
            axum::Json(serde_json::json!({
                "name": "AI Cloud Backpack",
                "version": env!("CARGO_PKG_VERSION"),
                "endpoints": {
                    "upload": "POST /upload?token=<optional>",
                    "search": "GET /search?token=<optional>&q=...",
                    "ask": "POST /ask?token=<optional>",
                    "inventory": "GET /inventory?token=<optional>",
                    "download": "GET /download/{id}?token=<optional>",
                    "delete": "DELETE /files/{id}?token=<optional>",
                    "archive_download": "GET /archive/dl/{token}"
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
        info!(" ─────── Share this ticket for P2P access ───────");

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
