use axum::{extract::{Path, State}, http::StatusCode, Json};
use uuid::Uuid;

use crate::{common::error_response, models::{CreateHypervisor, Hypervisor, Message, UpdateHypervisor}, state::SharedState};

type ApiError = (StatusCode, Json<Message>);

#[utoipa::path(get, path = "/api/hypervisors", responses((status = 200, body = [Hypervisor])), tag = "Hypervisors")]
pub async fn list(State(state): State<SharedState>) -> Result<Json<Vec<Hypervisor>>, ApiError> {
    let hypervisors = sqlx::query_as::<_, Hypervisor>("SELECT id, name, uri, status FROM hypervisors ORDER BY name")
        .fetch_all(&state.db).await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "SQLite read failed"))?;
    let mut results = Vec::with_capacity(hypervisors.len());
    for hypervisor in hypervisors { results.push(refresh_status(&state, hypervisor).await?); }
    Ok(Json(results))
}

#[utoipa::path(get, path = "/api/hypervisors/{id}", params(("id" = String, Path, description = "Hypervisor identifier")), responses((status = 200, body = Hypervisor), (status = 404, body = Message)), tag = "Hypervisors")]
pub async fn get(State(state): State<SharedState>, Path(id): Path<String>) -> Result<Json<Hypervisor>, ApiError> {
    let hypervisor = sqlx::query_as::<_, Hypervisor>("SELECT id, name, uri, status FROM hypervisors WHERE id = ?")
        .bind(&id).fetch_optional(&state.db).await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "SQLite read failed"))?
        .ok_or_else(|| error_response(StatusCode::NOT_FOUND, "Hypervisor not found"))?;
    Ok(Json(refresh_status(&state, hypervisor).await?))
}

#[utoipa::path(post, path = "/api/hypervisors", request_body = CreateHypervisor, responses((status = 201, body = Hypervisor), (status = 502, body = Message)), tag = "Hypervisors")]
pub async fn create(State(state): State<SharedState>, Json(body): Json<CreateHypervisor>) -> Result<(StatusCode, Json<Hypervisor>), ApiError> {
    if !test_connection(body.uri.clone()).await {
        return Err(error_response(StatusCode::BAD_GATEWAY, "Could not connect to libvirt with this URI"));
    }
    let hypervisor = Hypervisor { id: Uuid::new_v4().to_string(), name: body.name, uri: body.uri, status: "connected".into() };
    sqlx::query("INSERT INTO hypervisors (id, name, uri, status) VALUES (?, ?, ?, ?)")
        .bind(&hypervisor.id).bind(&hypervisor.name).bind(&hypervisor.uri).bind(&hypervisor.status)
        .execute(&state.db).await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "SQLite write failed"))?;
    Ok((StatusCode::CREATED, Json(hypervisor)))
}

#[utoipa::path(patch, path = "/api/hypervisors/{id}", params(("id" = String, Path, description = "Hypervisor identifier")), request_body = UpdateHypervisor, responses((status = 200, body = Hypervisor), (status = 404, body = Message), (status = 502, body = Message)), tag = "Hypervisors")]
pub async fn update(State(state): State<SharedState>, Path(id): Path<String>, Json(body): Json<UpdateHypervisor>) -> Result<Json<Hypervisor>, ApiError> {
    let UpdateHypervisor { name, uri } = body;
    if let Some(uri) = uri.as_ref() {
        if !test_connection(uri.clone()).await {
            return Err(error_response(StatusCode::BAD_GATEWAY, "Could not connect to libvirt with this URI"));
        }
    }
    let result = sqlx::query("UPDATE hypervisors SET name = COALESCE(?, name), uri = COALESCE(?, uri), status = CASE WHEN ? IS NULL THEN status ELSE 'connected' END WHERE id = ?")
        .bind(name).bind(uri.clone()).bind(uri).bind(&id).execute(&state.db).await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "SQLite write failed"))?;
    if result.rows_affected() == 0 { return Err(error_response(StatusCode::NOT_FOUND, "Hypervisor not found")); }
    get(State(state), Path(id)).await
}

#[utoipa::path(delete, path = "/api/hypervisors/{id}", params(("id" = String, Path, description = "Hypervisor identifier")), responses((status = 204), (status = 404, body = Message)), tag = "Hypervisors")]
pub async fn delete(State(state): State<SharedState>, Path(id): Path<String>) -> Result<StatusCode, ApiError> {
    let result = sqlx::query("DELETE FROM hypervisors WHERE id = ?").bind(id).execute(&state.db).await
        .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "SQLite write failed"))?;
    if result.rows_affected() == 0 { return Err(error_response(StatusCode::NOT_FOUND, "Hypervisor not found")); }
    Ok(StatusCode::NO_CONTENT)
}

async fn test_connection(uri: String) -> bool {
    tokio::task::spawn_blocking(move || {
        let Ok(connection) = virt::connect::Connect::open(Some(&uri)) else { return false; };
        connection.is_alive().unwrap_or(false)
    }).await.unwrap_or(false)
}

async fn refresh_status(state: &SharedState, mut hypervisor: Hypervisor) -> Result<Hypervisor, ApiError> {
    let status = if test_connection(hypervisor.uri.clone()).await { "connected" } else { "disconnected" };
    if hypervisor.status != status {
        sqlx::query("UPDATE hypervisors SET status = ? WHERE id = ?").bind(status).bind(&hypervisor.id).execute(&state.db).await
            .map_err(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "SQLite write failed"))?;
        hypervisor.status = status.into();
    }
    Ok(hypervisor)
}
