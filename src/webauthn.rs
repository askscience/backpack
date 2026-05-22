use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use url::Url;
use uuid::Uuid;
use webauthn_rs::prelude::*;

const CHALLENGE_TTL_SECS: u64 = 300;

pub struct WebauthnApp {
    pub webauthn: Webauthn,
    pub origin: Url,
    pub challenges: Mutex<HashMap<String, (PasskeyRegistration, String, Instant)>>,
    pub auth_challenges: Mutex<HashMap<String, (PasskeyAuthentication, Instant)>>,
}

impl WebauthnApp {
    pub fn new(rp_id: &str, origin: &str) -> Result<Self> {
        let origin = Url::parse(origin)?;
        let webauthn = WebauthnBuilder::new(rp_id, &origin)
            .context("Failed to build Webauthn")?
            .build()
            .context("Failed to build Webauthn")?;

        Ok(Self {
            webauthn,
            origin,
            challenges: Mutex::new(HashMap::new()),
            auth_challenges: Mutex::new(HashMap::new()),
        })
    }

    pub fn purge_expired_challenges(&self) {
        let now = Instant::now();
        {
            let mut reg = self.challenges.lock().unwrap();
            reg.retain(|_, (_, _, ts)| now.duration_since(*ts).as_secs() < CHALLENGE_TTL_SECS);
        }
        {
            let mut auth = self.auth_challenges.lock().unwrap();
            auth.retain(|_, (_, ts)| now.duration_since(*ts).as_secs() < CHALLENGE_TTL_SECS);
        }
    }

    pub fn start_registration(
        &self,
        user_id: &str,
        user_name: &str,
    ) -> Result<(CreationChallengeResponse, String)> {
        let user_unique_id = Uuid::new_v4();
        let challenge_id = Uuid::new_v4().to_string();

        let (ccr, reg_state) = self
            .webauthn
            .start_passkey_registration(
                user_unique_id,
                user_name,
                user_name,
                None,
            )
            .context("Failed to start registration")?;

        self.challenges
            .lock()
            .unwrap()
            .insert(challenge_id.clone(), (reg_state, user_id.to_string(), Instant::now()));

        Ok((ccr, challenge_id))
    }

    pub fn finish_registration(
        &self,
        challenge_id: &str,
        credential: &RegisterPublicKeyCredential,
    ) -> Result<Passkey> {
        let (reg_state, _user_id, _ts) = self
            .challenges
            .lock()
            .unwrap()
            .remove(challenge_id)
            .context("Registration challenge not found or expired")?;

        let passkey = self
            .webauthn
            .finish_passkey_registration(credential, &reg_state)
            .context("Failed to verify registration")?;

        Ok(passkey)
    }

    pub fn start_authentication(&self) -> Result<(RequestChallengeResponse, String)> {
        let challenge_id = Uuid::new_v4().to_string();

        let (rcr, auth_state) = self
            .webauthn
            .start_passkey_authentication(&[])
            .context("Failed to start authentication")?;

        self.auth_challenges
            .lock()
            .unwrap()
            .insert(challenge_id.clone(), (auth_state, Instant::now()));

        Ok((rcr, challenge_id))
    }

    pub fn finish_authentication(
        &self,
        challenge_id: &str,
        credential: &PublicKeyCredential,
    ) -> Result<AuthenticationResult> {
        let (auth_state, _ts) = self
            .auth_challenges
            .lock()
            .unwrap()
            .remove(challenge_id)
            .context("Authentication challenge not found or expired")?;

        let result = self
            .webauthn
            .finish_passkey_authentication(credential, &auth_state)
            .context("Failed to verify authentication")?;

        Ok(result)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WebauthnRegisterStartRequest {
    pub token: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WebauthnRegisterFinishRequest {
    pub challenge_id: String,
    pub credential: RegisterPublicKeyCredential,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WebauthnAuthFinishRequest {
    pub challenge_id: String,
    pub credential: PublicKeyCredential,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WebauthnStartResponse {
    pub challenge_id: String,
    pub public_key: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WebauthnFinishResponse {
    pub session_token: String,
    pub user_id: String,
    pub role: String,
}

pub async fn store_passkey(pool: &sqlx::SqlitePool, user_id: &str, passkey: &Passkey) -> Result<()> {
    let key_id = passkey.cred_id().to_vec();
    let credential = serde_json::to_vec(passkey)?;

    sqlx::query(
        "INSERT OR REPLACE INTO webauthn_keys (user_id, key_id, credential) VALUES (?1, ?2, ?3)",
    )
    .bind(user_id)
    .bind(&key_id)
    .bind(&credential)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn get_passkeys(pool: &sqlx::SqlitePool, user_id: &str) -> Result<Vec<Passkey>> {
    let rows = sqlx::query("SELECT credential FROM webauthn_keys WHERE user_id = ?1")
        .bind(user_id)
        .fetch_all(pool)
        .await?;

    let mut keys = Vec::new();
    for row in &rows {
        let bytes: Vec<u8> = row.get("credential");
        let key: Passkey = serde_json::from_slice(&bytes)?;
        keys.push(key);
    }
    Ok(keys)
}

pub async fn has_passkey(pool: &sqlx::SqlitePool, user_id: &str) -> Result<bool> {
    let row = sqlx::query("SELECT COUNT(*) as cnt FROM webauthn_keys WHERE user_id = ?1")
        .bind(user_id)
        .fetch_one(pool)
        .await?;
    Ok(row.get::<i64, _>("cnt") > 0)
}

pub async fn create_session(
    pool: &sqlx::SqlitePool,
    user_id: &str,
    role: &str,
) -> Result<String> {
    let token = Uuid::new_v4().to_string();
    let expires = chrono::Utc::now()
        .checked_add_signed(chrono::Duration::hours(24))
        .unwrap()
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();

    sqlx::query(
        "INSERT INTO sessions (session_token, user_id, role, expires_at) VALUES (?1, ?2, ?3, ?4)",
    )
    .bind(&token)
    .bind(user_id)
    .bind(role)
    .bind(&expires)
    .execute(pool)
    .await?;

    Ok(token)
}

pub async fn resolve_session(
    pool: &sqlx::SqlitePool,
    token: &str,
) -> Result<Option<(String, String)>> {
    let row = sqlx::query(
        "SELECT user_id, role FROM sessions WHERE session_token = ?1 AND expires_at > datetime('now')",
    )
    .bind(token)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| (r.get("user_id"), r.get("role"))))
}

pub async fn find_user_by_credential(
    pool: &sqlx::SqlitePool,
    cred_id: &CredentialID,
) -> Result<Option<String>> {
    let rows = sqlx::query("SELECT user_id, credential FROM webauthn_keys")
        .fetch_all(pool)
        .await?;

    for row in &rows {
        let bytes: Vec<u8> = row.get("credential");
        if let Ok(key) = serde_json::from_slice::<Passkey>(&bytes) {
            if key.cred_id() == cred_id {
                return Ok(Some(row.get("user_id")));
            }
        }
    }
    Ok(None)
}

pub async fn delete_session(pool: &sqlx::SqlitePool, token: &str) -> Result<()> {
    sqlx::query("DELETE FROM sessions WHERE session_token = ?1")
        .bind(token)
        .execute(pool)
    .await?;

    Ok(())
}

pub async fn purge_expired_sessions(pool: &sqlx::SqlitePool) -> Result<()> {
    sqlx::query("DELETE FROM sessions WHERE expires_at <= datetime('now')")
        .execute(pool)
        .await?;
    Ok(())
}
pub async fn ensure_webauthn_schema(pool: &sqlx::SqlitePool) -> Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS webauthn_keys (
            user_id TEXT NOT NULL,
            key_id BLOB NOT NULL,
            credential BLOB NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (user_id, key_id)
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS sessions (
            session_token TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            role TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            expires_at TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    Ok(())
}
