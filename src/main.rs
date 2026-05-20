mod catalog;
mod config;
mod db;
mod extraction;
mod handlers;
mod iroh;
mod spaces;
mod sync;
mod sync_hub;
mod vector;

use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{routing, Router};
use clap::{Parser, Subcommand};
use iroh_net::{key::SecretKey, ticket::NodeTicket, Endpoint, NodeAddr};
use reqwest::Client;
use sqlx::SqlitePool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tower_http::cors::{Any, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use crate::handlers::AppState;
use crate::spaces::{DeleteMode, SpaceManager};
use crate::sync::{SyncClient, SyncConfig, SyncEngine, SyncState};

const ALPN: &[u8] = b"backpack-http/1";

#[derive(Parser)]
#[command(name = "backpack", version, about = "AI Cloud Backpack — personal file catalog with LLM")]
struct Cli {
    /// Enable Iroh P2P connectivity (server mode only)
    #[arg(long, global = true)]
    iroh: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Set up and run the file sync daemon
    Sync {
        #[command(subcommand)]
        action: SyncAction,
    },
    /// Connect via Iroh P2P tunnel (client mode)
    Connect {
        /// Iroh ticket from `backpack --iroh`
        ticket: String,
        /// Local proxy port
        #[arg(long, default_value = "9090")]
        port: u16,
    },
    /// Manage isolated user spaces
    Space {
        #[command(subcommand)]
        action: SpaceAction,
    },
}

#[derive(Subcommand)]
enum SyncAction {
    /// Start the sync daemon (looks for .backpack-sync.toml in current or given directory)
    Start {
        /// Path to the configured watch directory
        #[arg(default_value = ".")]
        dir: String,
    },
    /// Initialize a directory for sync with a backpack server
    Init {
        /// Local directory to watch and sync
        #[arg(short, long)]
        dir: String,
        /// Backpack server URL (e.g. "http://localhost:8080")
        #[arg(short, long)]
        server: String,
        /// Optional space token for multi-user spaces
        #[arg(short, long)]
        space: Option<String>,
        /// Ignore patterns (e.g. "*.tmp")
        #[arg(long)]
        ignore: Vec<String>,
        /// Poll interval in seconds
        #[arg(long, default_value = "30")]
        poll_interval: u64,
        #[arg(long, default_value = "500")]
        debounce: u64,
    },
    /// Show sync status for a configured directory
    Status {
        /// Path to the configured watch directory
        #[arg(short, long)]
        dir: String,
    },
}

#[derive(Subcommand)]
enum SpaceAction {
    Create {
        #[arg(long)]
        label: String,
        #[arg(long, default_value = "0")]
        quota: u64,
    },
    Share {
        owner_token: String,
        #[arg(long)]
        label: String,
    },
    List,
    Info {
        token: String,
    },
    Delete {
        token: String,
        #[arg(long)]
        purge: bool,
        #[arg(long)]
        archive: bool,
        #[arg(long, requires = "archive")]
        for_share: Option<String>,
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_writer(std::io::stderr)
        .init();

    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    let config = config::Config::from_env().expect("Failed to load configuration");

    match cli.command {
        Some(Commands::Sync { action }) => {
            handle_sync(action).await;
        }
        Some(Commands::Connect { ticket, port }) => {
            if let Err(e) = run_p2p_client(&ticket, port).await {
                error!("P2P client error: {}", e);
                std::process::exit(1);
            }
        }
        Some(Commands::Space { action }) => {
            handle_space(action, &config).await;
        }
        None => {
            // Default: server mode
            run_server(config, cli.iroh).await;
        }
    }
}

// ── Sync daemon ──────────────────────────────────────────────────────

async fn handle_sync(action: SyncAction) {
    use std::path::PathBuf;
    match action {
        SyncAction::Start { dir } => {
            let sync_config = match SyncConfig::load_from_dir(&dir) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("No .backpack-sync.toml found in '{}': {}", dir, e);
                    eprintln!("Run `backpack sync init` first.");
                    std::process::exit(1);
                }
            };

            info!("Sync daemon starting");
            info!("  Watch dir:  {}", sync_config.watch_dir);
            info!("  Server:     {}", sync_config.server_url);
            info!("  Poll every: {}s", sync_config.poll_interval_secs);
            info!("  Debounce:   {}ms", sync_config.debounce_ms);

            let state = match SyncState::open(&sync_config.watch_dir).await {
                Ok(s) => s,
                Err(e) => { eprintln!("Failed to open sync state DB: {}", e); std::process::exit(1); }
            };

            let client = SyncClient::new(
                sync_config.server_url.clone(),
                sync_config.space_token.clone(),
            );

            let engine = std::sync::Arc::new(SyncEngine::new(sync_config, client, state));

            tokio::select! {
                result = engine.run() => {
                    if let Err(e) = result {
                        tracing::error!("Sync engine error: {}", e);
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    info!("Shutting down sync daemon");
                }
            }
        }
        SyncAction::Init { dir, server, space, ignore, poll_interval, debounce } => {
            let abs_dir = std::fs::canonicalize(&dir).unwrap_or_else(|_| {
                let p = PathBuf::from(&dir);
                std::fs::create_dir_all(&p).ok();
                p.canonicalize().unwrap_or(p)
            });
            let watch_dir = abs_dir.to_string_lossy().to_string();
            let sync_config = SyncConfig {
                watch_dir: watch_dir.clone(),
                server_url: server.trim_end_matches('/').to_string(),
                space_token: space,
                poll_interval_secs: poll_interval,
                ignore_patterns: ignore,
                debounce_ms: debounce,
                max_concurrency: 4,
            };
            if let Err(e) = sync_config.save() {
                eprintln!("Failed to save sync config: {}", e);
                std::process::exit(1);
            }
            println!("Sync config written to {}/.backpack-sync.toml", watch_dir);
            println!("  Watch dir:  {}", sync_config.watch_dir);
            println!("  Server:     {}", sync_config.server_url);
            println!("  Poll every: {}s", sync_config.poll_interval_secs);
            println!();
            println!("Run `backpack sync` in this directory to start syncing.");
        }
        SyncAction::Status { dir } => {
            match SyncConfig::load_from_dir(&dir) {
                Ok(config) => {
                    match SyncState::open(&config.watch_dir).await {
                        Ok(state) => {
                            match state.list_all().await {
                                Ok(entries) => {
                                    let total = entries.len();
                                    let mut synced = 0usize;
                                    let mut pending_upload = 0usize;
                                    let mut pending_download = 0usize;
                                    let mut conflicted = 0usize;
                                    let mut errors = 0usize;
                                    for e in &entries {
                                        match e.sync_status.as_str() {
                                            "synced" => synced += 1,
                                            "pending_upload" => pending_upload += 1,
                                            "pending_download" => pending_download += 1,
                                            "conflicted" => conflicted += 1,
                                            "error" => errors += 1,
                                            _ => {}
                                        }
                                    }
                                    println!("Sync Status");
                                    println!("===========");
                                    println!("  Watch dir:     {}", config.watch_dir);
                                    println!("  Server:        {}", config.server_url);
                                    println!("  Total tracked: {}", total);
                                    println!("  Synced:        {}", synced);
                                    println!("  Pending upload:{}", pending_upload);
                                    println!("  Pending dl:   {:>3}", pending_download);
                                    println!("  Conflicted:    {}", conflicted);
                                    println!("  Errors:        {}", errors);
                                    if total > 0 {
                                        println!();
                                        println!("Tracked files:");
                                        for entry in &entries {
                                            let icon = match entry.sync_status.as_str() {
                                                "synced" => "\u{2713}",
                                                "pending_upload" => "\u{2191}",
                                                "pending_download" => "\u{2193}",
                                                "conflicted" => "\u{26A0}",
                                                _ => "\u{2717}",
                                            };
                                            println!("  {}  {}", icon, entry.relative_path);
                                        }
                                    }
                                }
                                Err(e) => eprintln!("Failed to list sync state: {}", e),
                            }
                        }
                        Err(e) => eprintln!("Failed to open sync state: {}", e),
                    }
                }
                Err(e) => eprintln!("No .backpack-sync.toml found in {}: {}", dir, e),
            }
        }
    }
}

// ── Space management ──────────────────────────────────────────────────

async fn handle_space(action: SpaceAction, config: &config::Config) {
    let base = std::env::var("SPACES_DIR").unwrap_or_else(|_| "./spaces".into());
    let default_pool = db::init_db(&config.db_path).await.expect("Failed to init default DB");
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
                        entry.used_mb, entry.quota_mb, entry.status, entry.shares, entry.label,
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
}

// ── P2P client ───────────────────────────────────────────────────────

async fn run_p2p_client(ticket_str: &str, port: u16) -> anyhow::Result<()> {
    let ticket = NodeTicket::from_str(ticket_str)
        .context("Failed to parse ticket. Ensure you copied the full ticket string.")?;
    let node_addr: NodeAddr = ticket.into();
    let node_id = node_addr.node_id;

    info!("Resolving node: {} via DHT...", node_id);

    let endpoint = Endpoint::builder()
        .secret_key(SecretKey::generate())
        .alpns(vec![ALPN.to_vec()])
        .discovery_dht()
        .bind()
        .await
        .context("Failed to create Iroh endpoint")?;

    let conn = endpoint
        .connect(node_id, ALPN)
        .await
        .context("Failed to connect. Is the server online? DHT lookup may take a few seconds.")?;

    info!("Connected. Proxy listening on http://127.0.0.1:{}", port);

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .context("Failed to bind local proxy port")?;

    println!("Backpack proxy ready \u{2192} http://localhost:{}", port);
    println!("Use: curl http://localhost:{}/", port);

    let conn = Arc::new(conn);

    loop {
        let (tcp, peer_addr) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(e) => { error!("Accept error: {}", e); continue; }
        };
        let conn = conn.clone();
        tokio::spawn(async move {
            if let Err(e) = proxy_request(conn, tcp, peer_addr).await {
                error!("Proxy error from {}: {}", peer_addr, e);
            }
        });
    }
}

async fn proxy_request(
    conn: Arc<iroh_net::endpoint::Connection>,
    tcp: TcpStream,
    peer: SocketAddr,
) -> anyhow::Result<()> {
    let (send, recv) = conn
        .open_bi()
        .await
        .context("Failed to open bidirectional stream")?;
    let (mut tcp_read, mut tcp_write) = tokio::io::split(tcp);
    let quic_to_tcp = tokio::spawn(async move {
        let mut recv = recv;
        let mut buf = vec![0u8; 16384];
        loop {
            match recv.read(&mut buf).await {
                Ok(Some(0)) | Ok(None) | Err(_) => break,
                Ok(Some(n)) => { if tcp_write.write_all(&buf[..n]).await.is_err() { break; } }
            }
        }
    });
    let tcp_to_quic = tokio::spawn(async move {
        let mut send = send;
        let mut buf = vec![0u8; 16384];
        loop {
            match tcp_read.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => { if send.write_all(&buf[..n]).await.is_err() { break; } }
                Err(e) => { tracing::debug!("TCP read error: {}", e); break; }
            }
        }
    });
    let _ = tokio::join!(quic_to_tcp, tcp_to_quic);
    tracing::debug!("Bridge closed for {}", peer);
    Ok(())
}

// ── Server mode ──────────────────────────────────────────────────────

async fn run_server(config: config::Config, iroh_enabled: bool) {
    info!("Starting AI Cloud Backpack");
    info!("LLM provider: {}", config.llm_provider);
    info!("LLM model: {}", config.llm_model);
    info!("Embedding model: {}", config.embedding_model);
    info!("Upload dir: {}", config.upload_dir);
    info!("DB path: {}", config.db_path);

    let _skill_content = catalog::read_skill_prompt(&config.skill_path).unwrap_or_else(|e| {
        tracing::warn!("Could not read skill.md: {}. Using default prompt.", e);
        String::new()
    });

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

    let sync_hub = sync_hub::SyncHub::new();

    let state = AppState {
        pool: default_pool.clone(),
        config: Arc::new(config.clone()),
        client,
        spaces: Arc::new(space_manager),
        sync_hub,
    };

    let max_bytes = config.max_file_size_mb * 1024 * 1024;
    let cors = CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any);

    let app = Router::new()
        .route("/upload", routing::post(handlers::upload_handler))
        .route("/search", routing::get(handlers::search_handler))
        .route("/ask", routing::post(handlers::ask_handler))
        .route("/inventory", routing::get(handlers::inventory_handler))
        .route("/download/:id", routing::get(handlers::download_handler))
        .route("/files/:id", routing::delete(handlers::delete_handler))
        .route("/archive/dl/:id", routing::get(handlers::archive_download_handler))
        .route("/sync-token", routing::post(handlers::sync_token_handler))
        .route("/ws", routing::get(handlers::ws_handler))
        .route("/space/revoke-share", routing::post(handlers::revoke_share_handler))
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

    if iroh_enabled {
        let iroh_server = iroh::IrohServer::new().await.expect("Failed to start Iroh");
        let axum_port: u16 = addr
            .split(':').last().and_then(|p| p.parse().ok())
            .unwrap_or(8080);

        info!("NodeId:  {}", iroh_server.node_id());
        info!("Ticket:  {}", iroh_server.ticket());
        info!(" \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500} Share this ticket for P2P access \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}");

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
