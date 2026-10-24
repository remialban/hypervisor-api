//! Journal of who did what. Writing is best effort: a failure is logged, never returned.

use axum::{
    Json,
    extract::{Query, State},
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::{IntoParams, ToSchema};

use crate::{
    auth::{AuthUser, now},
    errors::ApiError,
    models::Message,
    state::SharedState,
};

#[derive(Serialize, ToSchema, FromRow)]
pub struct AuditEntry {
    pub id: i64,
    /// Unix time, in seconds.
    pub at: i64,
    pub user_id: Option<String>,
    pub username: Option<String>,
    /// For example `vm.start`, `auth.login`, `user.create`.
    pub action: String,
    /// What the action was about (a VM id, a user id...).
    pub target: Option<String>,
    pub detail: Option<String>,
    pub success: bool,
}

/// Records an action done by `actor`.
pub async fn record(
    state: &SharedState,
    actor: &AuthUser,
    action: &str,
    target: &str,
    detail: &str,
) {
    insert(
        state,
        Some(&actor.id),
        Some(&actor.username),
        action,
        target,
        detail,
        true,
    )
    .await;
}

/// Records an action with no authenticated user (failed login, registration...).
pub async fn record_anonymous(
    state: &SharedState,
    username: &str,
    action: &str,
    detail: &str,
    success: bool,
) {
    insert(state, None, Some(username), action, "", detail, success).await;
}

pub async fn insert(
    state: &SharedState,
    user_id: Option<&str>,
    username: Option<&str>,
    action: &str,
    target: &str,
    detail: &str,
    success: bool,
) {
    let result = sqlx::query(
        "INSERT INTO audit_log (at, user_id, username, action, target, detail, success)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(now())
    .bind(user_id)
    .bind(username)
    .bind(action)
    .bind(target)
    .bind(detail)
    .bind(success)
    .execute(&state.db)
    .await;
    if let Err(error) = result {
        eprintln!("Audit log write failed: {error}");
    }
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AuditQuery {
    /// Only the actions of this user.
    user_id: Option<String>,
    /// Only this action, for example `vm.delete`.
    action: Option<String>,
    /// Page size (default 100, max 500).
    limit: Option<i64>,
    /// Entries to skip (default 0).
    offset: Option<i64>,
}

#[utoipa::path(get, path = "/api/audit", params(AuditQuery), responses((status = 200, body = [AuditEntry]), (status = 403, body = Message)), tag = "Audit")]
pub async fn list(
    State(state): State<SharedState>,
    user: AuthUser,
    Query(query): Query<AuditQuery>,
) -> Result<Json<Vec<AuditEntry>>, ApiError> {
    user.require_admin()?;
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let offset = query.offset.unwrap_or(0).max(0);
    let entries = sqlx::query_as::<_, AuditEntry>(
        "SELECT id, at, user_id, username, action, target, detail, success FROM audit_log
         WHERE (?1 IS NULL OR user_id = ?1) AND (?2 IS NULL OR action = ?2)
         ORDER BY id DESC LIMIT ?3 OFFSET ?4",
    )
    .bind(query.user_id)
    .bind(query.action)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(entries))
}
