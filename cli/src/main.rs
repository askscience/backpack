#![allow(deprecated)]

use std::net::SocketAddr;
use std::str::FromStr;

use anyhow::{Context, Result};
use clap::Parser;
use iroh_net::{
    key::SecretKey,
    ticket::NodeTicket,
    Endpoint, NodeAddr,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{error, info};

const ALPN: &[u8] = b"backpack-http/1";

#[derive(Parser)]
#[command(name = "backpack-cli", about = "Iroh P2P client for AI Cloud Backpack")]
struct Cli {
    /// Iroh ticket string (from server --iroh output — just the NodeId)
    ticket: String,

    /// Local proxy port (default: 9090)
    #[arg(long, default_value = "9090")]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    let ticket = NodeTicket::from_str(&cli.ticket)
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

    info!("Connected. Proxy listening on http://127.0.0.1:{}", cli.port);

    let listener = TcpListener::bind(("127.0.0.1", cli.port))
        .await
        .context("Failed to bind local proxy port")?;

    println!("Backpack proxy ready → http://localhost:{}", cli.port);
    println!("Use: curl http://localhost:{}/", cli.port);

    let conn = std::sync::Arc::new(conn);

    loop {
        let (tcp, peer_addr) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(e) => {
                error!("Accept error: {}", e);
                continue;
            }
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
    conn: std::sync::Arc<iroh_net::endpoint::Connection>,
    tcp: TcpStream,
    peer: SocketAddr,
) -> Result<()> {
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
                Ok(Some(n)) => {
                    if tcp_write.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    let tcp_to_quic = tokio::spawn(async move {
        let mut send = send;
        let mut buf = vec![0u8; 16384];
        loop {
            match tcp_read.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if send.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    tracing::debug!("TCP read error: {}", e);
                    break;
                }
            }
        }
    });

    let _ = tokio::join!(quic_to_tcp, tcp_to_quic);
    tracing::debug!("Bridge closed for {}", peer);
    Ok(())
}
