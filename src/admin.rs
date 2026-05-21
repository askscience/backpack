//! Admin authentication and shared types for the admin API.
//!
//! The admin API is gated by an `ADMIN_TOKEN` environment variable.
//! When the token is not configured, all admin routes return `404`
//! without revealing the endpoints exist. When configured, only
//! requests with a matching `Authorization: Bearer <token>` header
//! are accepted — wrong or missing tokens also receive `404`.
//!
//! Token comparison uses constant-time comparison to prevent
//! timing side-channel attacks on the bearer token.

use axum::http::{header, StatusCode};

/// Checks admin authentication from request headers.
///
/// Returns `Ok(())` when the `Authorization: Bearer <token>` header
/// matches the configured `ADMIN_TOKEN`. Returns `Err(response)` (a
/// `404` response) otherwise, including when no admin token is
/// configured.
///
/// The response is pre-built so the middleware in `main.rs` does not
/// need the `IntoResponse` trait in scope.
pub fn check_admin_auth(
    configured_token: &Option<String>,
    headers: &axum::http::HeaderMap,
) -> Result<(), axum::response::Response<axum::body::Body>> {
    // No admin token configured → pretend the endpoints don't exist.
    let expected = match configured_token {
        Some(t) => t.as_bytes(),
        None => return Err(not_found_response()),
    };

    // Extract "Bearer <token>" from the Authorization header.
    let provided = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    match provided {
        // Constant-time comparison to prevent timing attacks.
        Some(p) if constant_time_eq(p.as_bytes(), expected) => Ok(()),
        _ => Err(not_found_response()),
    }
}

/// Builds a generic 404 response with no body.
/// Deliberately returns 404 (not 401 or 403) to avoid confirming
/// that admin endpoints exist on this server.
fn not_found_response() -> axum::response::Response<axum::body::Body> {
    let mut resp = axum::response::Response::new(axum::body::Body::from("Not Found"));
    *resp.status_mut() = StatusCode::NOT_FOUND;
    resp
}

/// Constant-time byte comparison.
///
/// Prevents timing side-channel attacks by ensuring comparison
/// takes the same amount of time regardless of where the first
/// differing byte occurs.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0, |acc, (x, y)| acc | (x ^ y))
        == 0
}

// ── Shared request / response types for admin endpoints ─────────────

/// Request body for `POST /api/admin/spaces` — create a new space.
#[derive(Debug, serde::Deserialize)]
pub struct CreateSpaceRequest {
    /// Human-readable label for the space (e.g. "bob-project").
    pub label: String,
    /// Quota in megabytes. `None` or `0` means unlimited.
    #[serde(default)]
    pub quota_mb: Option<u64>,
}

/// Request body for `POST /api/admin/spaces/:id/share` — share a space.
#[derive(Debug, serde::Deserialize)]
pub struct ShareSpaceRequest {
    /// Label for the person or purpose receiving this share.
    pub label: String,
}

/// Query parameters for `DELETE /api/admin/spaces/:id` — delete a space.
#[derive(Debug, serde::Deserialize)]
pub struct DeleteSpaceQuery {
    /// `"purge"` for immediate permanent deletion,
    /// `"archive"` to create a downloadable ZIP before deletion.
    pub mode: String,
    /// When `mode=archive`, optionally restrict the download to a
    /// specific share token holder.
    #[serde(default)]
    pub for_share: Option<String>,
}
