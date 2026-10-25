use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use uuid::Uuid;

use crate::{
    access, audit,
    auth::{AuthUser, now},
    common::error_response,
    models::{CreateHypervisor, Hypervisor, Message, UpdateHypervisor},
    state::SharedState,
};

type ApiError = (StatusCode, Json<Message>);

#[utoipa::path(get, path = "/api/hypervisors", responses((status = 200, body = [Hypervisor])), tag = "Hypervisors")]
pub async fn list(
    State(state): State<SharedState>,
    user: AuthUser,
) -> Result<Json<Vec<Hypervisor>>, ApiError> {
    // An admin sees everything; others only what they were granted or what hosts one of their VMs.
    let hypervisors = if user.is_admin() {
        sqlx::query_as::<_, Hypervisor>(
            "SELECT id, name, uri, status FROM hypervisors ORDER BY name",
        )
        .fetch_all(&state.db)
        .await
    } else {
        sqlx::query_as::<_, Hypervisor>(&format!(
            "SELECT id, name, uri, status FROM hypervisors WHERE id IN ({}) ORDER BY name",
            access::VISIBLE_HYPERVISORS
        ))
        .bind(&user.id)
        .bind(now())
        .fetch_all(&state.db)
        .await
    }
    .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database read failed"))?;
    let mut results = Vec::with_capacity(hypervisors.len());
    for hypervisor in hypervisors {
        results.push(present(&user, refresh_status(&state, hypervisor).await?));
    }
    Ok(Json(results))
}

/// The connection URI can reveal hosts and SSH users: only admins get it.
fn present(user: &AuthUser, mut hypervisor: Hypervisor) -> Hypervisor {
    if !user.is_admin() {
        hypervisor.uri = String::new();
    }
    hypervisor
}

#[utoipa::path(get, path = "/api/hypervisors/{id}", params(("id" = String, Path, description = "Hypervisor identifier")), responses((status = 200, body = Hypervisor), (status = 404, body = Message)), tag = "Hypervisors")]
pub async fn get(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<Hypervisor>, ApiError> {
    if !access::hypervisor_visible(&state, &user, &id).await? {
        return Err(error_response(
            StatusCode::NOT_FOUND,
            "Hypervisor not found",
        ));
    }
    let hypervisor = sqlx::query_as::<_, Hypervisor>(
        "SELECT id, name, uri, status FROM hypervisors WHERE id = $1",
    )
    .bind(&id)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database read failed"))?
    .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "Hypervisor not found"))?;
    Ok(Json(present(
        &user,
        refresh_status(&state, hypervisor).await?,
    )))
}

#[utoipa::path(post, path = "/api/hypervisors", request_body = CreateHypervisor, responses((status = 201, body = Hypervisor), (status = 502, body = Message)), tag = "Hypervisors")]
pub async fn create(
    State(state): State<SharedState>,
    user: AuthUser,
    Json(body): Json<CreateHypervisor>,
) -> Result<(StatusCode, Json<Hypervisor>), ApiError> {
    user.require_admin()?;
    if !test_connection(body.uri.clone()).await {
        return Err(error_response(
            StatusCode::BAD_GATEWAY,
            "Could not connect to libvirt with this URI",
        ));
    }
    let hypervisor = Hypervisor {
        id: Uuid::new_v4().to_string(),
        name: body.name,
        uri: body.uri,
        status: "connected".into(),
    };
    sqlx::query("INSERT INTO hypervisors (id, name, uri, status) VALUES ($1, $2, $3, $4)")
        .bind(&hypervisor.id)
        .bind(&hypervisor.name)
        .bind(&hypervisor.uri)
        .bind(&hypervisor.status)
        .execute(&state.db)
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database write failed"))?;
    audit::record(
        &state,
        &user,
        "hypervisor.create",
        &hypervisor.id,
        &hypervisor.name,
    )
    .await;
    Ok((StatusCode::CREATED, Json(hypervisor)))
}

#[utoipa::path(patch, path = "/api/hypervisors/{id}", params(("id" = String, Path, description = "Hypervisor identifier")), request_body = UpdateHypervisor, responses((status = 200, body = Hypervisor), (status = 404, body = Message), (status = 502, body = Message)), tag = "Hypervisors")]
pub async fn update(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(body): Json<UpdateHypervisor>,
) -> Result<Json<Hypervisor>, ApiError> {
    user.require_admin()?;
    let UpdateHypervisor { name, uri } = body;
    if let Some(uri) = uri.as_ref()
        && !test_connection(uri.clone()).await
    {
        return Err(error_response(
            StatusCode::BAD_GATEWAY,
            "Could not connect to libvirt with this URI",
        ));
    }
    let result = sqlx::query("UPDATE hypervisors SET name = COALESCE($1, name), uri = COALESCE($2, uri), status = CASE WHEN $3 IS NULL THEN status ELSE 'connected' END WHERE id = $4")
        .bind(name).bind(uri.clone()).bind(uri).bind(&id).execute(&state.db).await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database write failed"))?;
    if result.rows_affected() == 0 {
        return Err(error_response(
            StatusCode::NOT_FOUND,
            "Hypervisor not found",
        ));
    }
    audit::record(&state, &user, "hypervisor.update", &id, "").await;
    get(State(state), user, Path(id)).await
}

#[utoipa::path(delete, path = "/api/hypervisors/{id}", params(("id" = String, Path, description = "Hypervisor identifier")), responses((status = 204), (status = 404, body = Message), (status = 409, body = Message, description = "Lab instances are deployed on it")), tag = "Hypervisors")]
pub async fn delete(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    user.require_admin()?;
    // The VMs of lab instances live on the hypervisor: they are deleted with their instance.
    let instances: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM lab_instances i JOIN labs l ON l.id = i.lab_id WHERE l.hypervisor_id = $1",
    )
    .bind(&id)
    .fetch_one(&state.db)
    .await
    .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database read failed"))?;
    if instances > 0 {
        return Err(error_response(
            StatusCode::CONFLICT,
            &format!(
                "{instances} lab instance(s) are deployed on this hypervisor: delete them first"
            ),
        ));
    }
    let result = sqlx::query("DELETE FROM hypervisors WHERE id = $1")
        .bind(&id)
        .execute(&state.db)
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database write failed"))?;
    if result.rows_affected() == 0 {
        return Err(error_response(
            StatusCode::NOT_FOUND,
            "Hypervisor not found",
        ));
    }
    audit::record(&state, &user, "hypervisor.delete", &id, "").await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(serde::Serialize, utoipa::ToSchema, sqlx::FromRow)]
pub struct AccessEntry {
    pub user_id: String,
    pub username: String,
    pub created_at: i64,
}

#[utoipa::path(get, path = "/api/hypervisors/{id}/access", params(("id" = String, Path, description = "Hypervisor identifier")), responses((status = 200, body = [AccessEntry]), (status = 403, body = Message), (status = 404, body = Message)), tag = "Hypervisors")]
pub async fn list_access(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<Vec<AccessEntry>>, ApiError> {
    user.require_admin()?;
    ensure_exists(&state, &id).await?;
    let entries = sqlx::query_as::<_, AccessEntry>(
        "SELECT a.user_id, u.username, a.created_at FROM hypervisor_access a JOIN users u ON u.id = a.user_id WHERE a.hypervisor_id = $1 ORDER BY u.username")
        .bind(&id).fetch_all(&state.db).await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database read failed"))?;
    Ok(Json(entries))
}

/// Lets a user create VMs on this hypervisor (admin only).
#[utoipa::path(put, path = "/api/hypervisors/{id}/access/{user_id}", params(("id" = String, Path, description = "Hypervisor identifier"), ("user_id" = String, Path)), responses((status = 204), (status = 403, body = Message), (status = 404, body = Message)), tag = "Hypervisors")]
pub async fn grant_access(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((id, user_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    user.require_admin()?;
    ensure_exists(&state, &id).await?;
    let exists: Option<i64> = sqlx::query_scalar("SELECT 1::BIGINT FROM users WHERE id = $1")
        .bind(&user_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database read failed"))?;
    if exists.is_none() {
        return Err(error_response(StatusCode::NOT_FOUND, "User not found"));
    }
    sqlx::query("INSERT INTO hypervisor_access (user_id, hypervisor_id, created_at) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
        .bind(&user_id).bind(&id).bind(now()).execute(&state.db).await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database write failed"))?;
    audit::record(&state, &user, "hypervisor.grant", &id, &user_id).await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/api/hypervisors/{id}/access/{user_id}", params(("id" = String, Path, description = "Hypervisor identifier"), ("user_id" = String, Path)), responses((status = 204), (status = 403, body = Message), (status = 404, body = Message)), tag = "Hypervisors")]
pub async fn revoke_access(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((id, user_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    user.require_admin()?;
    let result =
        sqlx::query("DELETE FROM hypervisor_access WHERE user_id = $1 AND hypervisor_id = $2")
            .bind(&user_id)
            .bind(&id)
            .execute(&state.db)
            .await
            .map_err(|_| {
                error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database write failed")
            })?;
    if result.rows_affected() == 0 {
        return Err(error_response(StatusCode::NOT_FOUND, "Access not found"));
    }
    audit::record(&state, &user, "hypervisor.revoke", &id, &user_id).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn ensure_exists(state: &SharedState, id: &str) -> Result<(), ApiError> {
    let found: Option<i64> = sqlx::query_scalar("SELECT 1::BIGINT FROM hypervisors WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database read failed"))?;
    found
        .map(|_| ())
        .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "Hypervisor not found"))
}

async fn test_connection(uri: String) -> bool {
    tokio::task::spawn_blocking(move || {
        let Ok(connection) = virt::connect::Connect::open(Some(&uri)) else {
            return false;
        };
        connection.is_alive().unwrap_or(false)
    })
    .await
    .unwrap_or(false)
}

async fn refresh_status(
    state: &SharedState,
    mut hypervisor: Hypervisor,
) -> Result<Hypervisor, ApiError> {
    let status = if test_connection(hypervisor.uri.clone()).await {
        "connected"
    } else {
        "disconnected"
    };
    if hypervisor.status != status {
        sqlx::query("UPDATE hypervisors SET status = $1 WHERE id = $2")
            .bind(status)
            .bind(&hypervisor.id)
            .execute(&state.db)
            .await
            .map_err(|_| {
                error_response(StatusCode::INTERNAL_SERVER_ERROR, "Database write failed")
            })?;
        hypervisor.status = status.into();
    }
    Ok(hypervisor)
}
