use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::{broadcast, Mutex};
use tracing::{debug, info};
use uuid::Uuid;

/// Payload sent over WebSocket when a file changes on the server.
#[derive(Debug, Clone, Serialize)]
pub struct SyncEvent {
    #[serde(rename = "type")]
    pub typ: String, // "created" | "updated" | "deleted"
    pub file_id: String,
    pub original_name: String,
    pub file_size: i64,
    pub timestamp: String,
}

/// In-memory sync token issued to clients.
#[derive(Debug, Clone)]
struct SyncTicket {
    space_id: String,
    expires_at: chrono::DateTime<chrono::Utc>,
    #[allow(dead_code)]
    label: String,
}

/// Manages WebSocket broadcast channels and sync tokens per space.
///
/// Each space gets its own `broadcast::Sender<SyncEvent>`. When a file is
/// uploaded or deleted in a space, the hub notifies every connected client.
///
/// Sync tokens are ephemeral (in-memory, 24h TTL). They are only issued
/// for spaces that have been shared with at least one other person.
pub struct SyncHub {
    /// Broadcast senders keyed by space_id.
    channels: Mutex<HashMap<String, broadcast::Sender<SyncEvent>>>,

    /// Active sync tokens keyed by token string.
    tickets: Mutex<HashMap<String, SyncTicket>>,
}

impl SyncHub {
    /// Create a new, empty sync hub.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            channels: Mutex::new(HashMap::new()),
            tickets: Mutex::new(HashMap::new()),
        })
    }

    /// Issue a new sync token for `space_id`, valid for 24 hours.
    /// Returns `None` if the space has no shares (single-user space).
    pub async fn issue_sync_token(
        &self,
        space_id: &str,
        has_shares: bool,
        label: &str,
    ) -> Option<String> {
        if !has_shares {
            return None; // Only shared spaces get push sync.
        }

        let token = Uuid::new_v4().to_string().replace('-', "");
        let expires_at = chrono::Utc::now() + chrono::Duration::hours(24);

        let ticket = SyncTicket {
            space_id: space_id.to_string(),
            expires_at,
            label: label.to_string(),
        };

        self.tickets.lock().await.insert(token.clone(), ticket);
        info!(
            "Issued sync token for space={}, label={} (expires in 24h)",
            space_id, label
        );

        Some(token)
    }

    /// Validate a sync token. Returns the space_id if valid, `None` otherwise.
    pub async fn validate_sync_token(&self, token: &str) -> Option<String> {
        let tickets = self.tickets.lock().await;

        let ticket = tickets.get(token)?;

        if ticket.expires_at < chrono::Utc::now() {
            drop(tickets);
            self.evict_token(token).await;
            return None;
        }

        Some(ticket.space_id.clone())
    }

    /// Subscribe to file change events for a space. Returns a receiver
    /// that yields `SyncEvent`s. Also creates the broadcast channel if
    /// this is the first subscriber for the space.
    pub async fn subscribe(&self, space_id: &str) -> broadcast::Receiver<SyncEvent> {
        let mut channels = self.channels.lock().await;

        let sender = channels
            .entry(space_id.to_string())
            .or_insert_with(|| {
                let (tx, _) = broadcast::channel(64);
                info!("Created broadcast channel for space={}", space_id);
                tx
            });

        let rx = sender.subscribe();
        debug!(
            "Client subscribed to space={} ({} total receivers)",
            space_id,
            sender.receiver_count()
        );
        rx
    }

    /// Broadcast a file change event to every connected WebSocket client
    /// in the given space. Gated by `has_shares`: if the space is private,
    /// the broadcast is silently skipped.
    pub async fn broadcast(
        &self,
        space_id: &str,
        has_shares: bool,
        event: SyncEvent,
    ) {
        if !has_shares {
            debug!(
                "Skipping broadcast for private space={} (type={}, file={})",
                space_id, event.typ, event.file_id
            );
            return;
        }

        let channels = self.channels.lock().await;
        if let Some(sender) = channels.get(space_id) {
            let count = sender.receiver_count();
            if count > 0 {
                debug!(
                    "Broadcasting {} to {} clients in space={}",
                    event.typ, count, space_id
                );
                match sender.send(event) {
                    Ok(sent) => {
                        debug!("Delivered event to {} receivers", sent);
                    }
                    Err(broadcast::error::SendError(_)) => {
                        // No active receivers — the channel exists but
                        // all receivers have been dropped.
                        debug!("No active receivers for space={}", space_id);
                    }
                }
            }
        }
    }

    /// Broadcast a "revoked" system event to all connected clients in a space.
    /// Bypasses the `has_shares` gate — this is called AFTER the share has
    /// been deleted, so the gate would incorrectly block the notification.
    /// Connected clients receiving "revoked" will close their WebSocket
    /// and fall back to poll-only mode.
    pub async fn broadcast_revoked(&self, space_id: &str) {
        let event = SyncEvent {
            typ: "revoked".into(),
            file_id: String::new(),
            original_name: String::new(),
            file_size: 0,
            timestamp: chrono::Utc::now()
                .format("%Y-%m-%d %H:%M:%S")
                .to_string(),
        };

        let channels = self.channels.lock().await;
        if let Some(sender) = channels.get(space_id) {
            let count = sender.receiver_count();
            debug!(
                "Broadcasting 'revoked' to {} clients in space={}",
                count, space_id
            );
            let _ = sender.send(event);
        }
    }

    /// Remove all sync tokens issued for a space. When a share is revoked,
    /// any remaining sync tickets should be invalidated so clients cannot
    /// reconnect via WebSocket.
    pub async fn revoke_space_tokens(&self, space_id: &str) {
        let mut tickets = self.tickets.lock().await;
        let before = tickets.len();
        tickets.retain(|_, t| t.space_id != space_id);
        let removed = before - tickets.len();
        if removed > 0 {
            info!(
                "Revoked {} sync tokens for space={}",
                removed, space_id
            );
        }
    }

    /// Periodically remove expired tokens. Can be called via a background interval.
    #[allow(dead_code)]
    pub async fn purge_expired_tokens(&self) {
        let now = chrono::Utc::now();
        let mut tickets = self.tickets.lock().await;
        let before = tickets.len();

        tickets.retain(|_, t| t.expires_at > now);

        if before != tickets.len() {
            info!(
                "Purged {} expired sync tokens ({} remaining)",
                before - tickets.len(),
                tickets.len()
            );
        }
    }

    /// Remove a single token (used when validation fails).
    async fn evict_token(&self, token: &str) {
        let mut tickets = self.tickets.lock().await;
        tickets.remove(token);
        debug!("Evicted expired/stale sync token: {}", &token[..8]);
    }
}
