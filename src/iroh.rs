#![allow(deprecated)]

use std::net::SocketAddr;

use anyhow::{Context, Result};
use iroh_net::{
    endpoint::Incoming,
    key::SecretKey,
    ticket::NodeTicket,
    Endpoint, NodeAddr, NodeId,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{debug, info};

const ALPN: &[u8] = b"backpack-http/1";

pub struct IrohServer {
    endpoint: Endpoint,
    node_id: NodeId,
    ticket: String,
}

impl IrohServer {
    pub async fn new() -> Result<Self> {
        let key = SecretKey::generate();

        let endpoint = Endpoint::builder()
            .secret_key(key)
            .alpns(vec![ALPN.to_vec()])
            .discovery_dht()
            .bind()
            .await
            .context("Failed to create Iroh endpoint")?;

        let node_id = endpoint.node_id();
        let ticket = NodeTicket::new(NodeAddr::from(node_id)).to_string();

        info!("Iroh NodeId: {}", node_id);
        info!("Iroh ticket: {}", ticket);

        Ok(Self {
            endpoint,
            node_id,
            ticket,
        })
    }

    pub fn ticket(&self) -> &str {
        &self.ticket
    }

    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    pub async fn bridge_loop(&self, axum_port: u16) -> Result<()> {
        let addr = SocketAddr::from(([127, 0, 0, 1], axum_port));

        loop {
            match self.endpoint.accept().await {
                Some(incoming) => {
                    let remote = incoming.remote_address();
                    let tcp_addr = addr;
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(incoming, tcp_addr).await {
                            tracing::warn!("Iroh connection error from {remote}: {e}");
                        }
                    });
                }
                None => break,
            }
        }

        Ok(())
    }
}

async fn handle_connection(incoming: Incoming, axum_addr: SocketAddr) -> Result<()> {
    let conn = incoming
        .accept()
        .context("Failed to accept incoming connection")?;
    let conn = conn
        .await
        .context("Failed to establish Iroh connection")?;

    info!("Iroh connection accepted");

    loop {
        let (send, recv) = conn
            .accept_bi()
            .await
            .context("Failed to accept bidirectional stream")?;

        let tcp = TcpStream::connect(axum_addr)
            .await
            .context("Failed to connect to Axum")?;

        debug!("Bridging Iroh stream to Axum");
        tokio::spawn(bridge(send, recv, tcp));
    }
}

async fn bridge(
    mut quic_send: iroh_net::endpoint::SendStream,
    mut quic_recv: iroh_net::endpoint::RecvStream,
    tcp: TcpStream,
) {
    let (mut tcp_read, mut tcp_write) = tokio::io::split(tcp);

    let a = tokio::spawn(async move {
        let mut buf = vec![0u8; 16384];
        loop {
            match quic_recv.read(&mut buf).await {
                Ok(Some(0)) | Ok(None) | Err(_) => break,
                Ok(Some(n)) => {
                    if tcp_write.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    let b = tokio::spawn(async move {
        let mut buf = vec![0u8; 16384];
        loop {
            match tcp_read.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if quic_send.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let _ = tokio::join!(a, b);
}
