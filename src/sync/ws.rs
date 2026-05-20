use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, info, warn};

/// Notification received from the server WebSocket when a file changes in a shared space.
#[derive(Debug, Clone, Deserialize)]
pub struct WsSyncEvent {
    #[serde(rename = "type")]
    pub typ: String,
    pub file_id: String,
    pub original_name: String,
    pub file_size: i64,
    #[allow(dead_code)]
    pub timestamp: String,
}

/// Connect to the backpack server's WebSocket for real-time file change notifications.
pub async fn connect(
    server_url: &str,
    space_token: &str,
) -> Result<(mpsc::UnboundedReceiver<WsSyncEvent>, tokio::task::JoinHandle<()>)> {
    let base = server_url.trim_end_matches('/');
    let http_client = reqwest::Client::new();

    let token_url = format!("{}/sync-token?token={}", base, urlencode(space_token));
    info!("Requesting sync token from {}", token_url);

    let resp = http_client.post(&token_url).send().await
        .with_context(|| format!("Failed to reach sync-token endpoint at {}", token_url))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if status.as_u16() == 404 {
            info!("Sync token not available (space may be private/non-shared): {}", body);
            let (_tx, rx) = mpsc::unbounded_channel();
            let handle = tokio::spawn(async {});
            return Ok((rx, handle));
        }
        anyhow::bail!("Failed to get sync token: HTTP {} — {}", status, body);
    }

    #[derive(Deserialize)]
    struct SyncTokenResponse {
        sync_token: String,
        #[allow(dead_code)]
        space_id: String,
        #[allow(dead_code)]
        expires_in_secs: u64,
        #[allow(dead_code)]
        ws_endpoint: String,
    }

    let token_resp: SyncTokenResponse = resp.json().await
        .context("Failed to parse sync token response")?;
    info!("Sync token obtained, expires in {}s", token_resp.expires_in_secs);

    let (tx, rx) = mpsc::unbounded_channel();
    let server_host = server_url.trim_start_matches("http://").trim_start_matches("https://").trim_end_matches('/');
    let ws_url = if server_url.starts_with("https://") {
        format!("wss://{}/ws?sync_token={}", server_host, token_resp.sync_token)
    } else {
        format!("ws://{}/ws?sync_token={}", server_host, token_resp.sync_token)
    };

    let handle = tokio::spawn(async move { ws_loop(&ws_url, tx).await; });
    Ok((rx, handle))
}

async fn ws_loop(ws_url: &str, tx: mpsc::UnboundedSender<WsSyncEvent>) {
    let mut retry_delay = 1u64;
    const MAX_RETRY_DELAY: u64 = 60;

    loop {
        info!("Connecting to WebSocket: {}", ws_url);
        match connect_async(ws_url).await {
            Ok((ws_stream, _response)) => {
                info!("WebSocket connected");
                retry_delay = 1;
                let (mut write, mut read) = ws_stream.split();

                let read_handle = {
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        while let Some(msg_result) = read.next().await {
                            match msg_result {
                                Ok(Message::Text(text)) => {
                                    match serde_json::from_str::<WsSyncEvent>(&text) {
                                        Ok(event) => {
                                            debug!("WS event: {} file={}", event.typ, event.file_id);
                                            if tx.send(event).is_err() { break; }
                                        }
                                        Err(e) => warn!("Failed to parse WS message: {} — {}", e, &text[..200.min(text.len())]),
                                    }
                                }
                                Ok(Message::Ping(data)) => {
                                    if write.send(Message::Pong(data)).await.is_err() { break; }
                                }
                                Ok(Message::Close(_)) => { info!("WebSocket closed by server"); break; }
                                Ok(_) => {}
                                Err(e) => { warn!("WebSocket read error: {}", e); break; }
                            }
                        }
                    })
                };

                read_handle.await.ok();
                info!("WebSocket disconnected");
            }
            Err(e) => warn!("WebSocket connection failed: {}", e),
        }

        info!("Reconnecting in {}s...", retry_delay);
        tokio::time::sleep(std::time::Duration::from_secs(retry_delay)).await;
        retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);
    }
}

fn urlencode(s: &str) -> String {
    s.replace('%', "%25").replace(' ', "%20")
        .replace('#', "%23").replace('&', "%26").replace('+', "%2B")
}
