use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use uuid::Uuid;

use crate::{
    common::erreur,
    models::{CreateHypervisor, Hypervisor, Message, UpdateHypervisor},
    state::SharedState,
};

#[utoipa::path(
    get,
    path = "/api/hypervisors",
    responses((status = 200, body = [Hypervisor])),
    tag = "Hypervisors"
)]
pub async fn lister(
    State(state): State<SharedState>,
) -> Result<Json<Vec<Hypervisor>>, (StatusCode, Json<Message>)> {
    let hypervisors = sqlx::query_as::<_, Hypervisor>(
        "SELECT id, nom, uri, statut FROM hypervisors ORDER BY nom",
    )
    .fetch_all(&state.db)
    .await
    .map_err(|_| erreur(StatusCode::INTERNAL_SERVER_ERROR, "Erreur de lecture SQLite"))?;

    let mut resultats = Vec::with_capacity(hypervisors.len());
    for hypervisor in hypervisors {
        resultats.push(rafraichir_statut(&state, hypervisor).await?);
    }

    Ok(Json(resultats))
}

#[utoipa::path(
    get,
    path = "/api/hypervisors/{id}",
    params(("id" = String, Path, description = "Identifiant de l'hyperviseur")),
    responses((status = 200, body = Hypervisor), (status = 404, body = Message)),
    tag = "Hypervisors"
)]
pub async fn obtenir(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<Hypervisor>, (StatusCode, Json<Message>)> {
    let hypervisor = sqlx::query_as::<_, Hypervisor>(
        "SELECT id, nom, uri, statut FROM hypervisors WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.db)
    .await
    .map_err(|_| erreur(StatusCode::INTERNAL_SERVER_ERROR, "Erreur de lecture SQLite"))?
    .ok_or_else(|| erreur(StatusCode::NOT_FOUND, "Hyperviseur introuvable"))?;

    Ok(Json(rafraichir_statut(&state, hypervisor).await?))
}

#[utoipa::path(
    post,
    path = "/api/hypervisors",
    request_body = CreateHypervisor,
    responses((status = 201, body = Hypervisor), (status = 502, body = Message)),
    tag = "Hypervisors"
)]
pub async fn ajouter(
    State(state): State<SharedState>,
    Json(body): Json<CreateHypervisor>,
) -> Result<(StatusCode, Json<Hypervisor>), (StatusCode, Json<Message>)> {
    if !tester_connexion(body.uri.clone()).await {
        return Err(erreur(
            StatusCode::BAD_GATEWAY,
            "Connexion libvirt impossible avec cette URI",
        ));
    }

    let hypervisor = Hypervisor {
        id: Uuid::new_v4().to_string(),
        nom: body.nom,
        uri: body.uri,
        statut: "connecte".into(),
    };

    sqlx::query("INSERT INTO hypervisors (id, nom, uri, statut) VALUES (?, ?, ?, ?)")
        .bind(&hypervisor.id)
        .bind(&hypervisor.nom)
        .bind(&hypervisor.uri)
        .bind(&hypervisor.statut)
        .execute(&state.db)
        .await
        .map_err(|_| erreur(StatusCode::INTERNAL_SERVER_ERROR, "Erreur d'ecriture SQLite"))?;

    Ok((StatusCode::CREATED, Json(hypervisor)))
}

#[utoipa::path(
    patch,
    path = "/api/hypervisors/{id}",
    params(("id" = String, Path, description = "Identifiant de l'hyperviseur")),
    request_body = UpdateHypervisor,
    responses((status = 200, body = Hypervisor), (status = 404, body = Message), (status = 502, body = Message)),
    tag = "Hypervisors"
)]
pub async fn modifier(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    Json(body): Json<UpdateHypervisor>,
) -> Result<Json<Hypervisor>, (StatusCode, Json<Message>)> {
    let crate::models::UpdateHypervisor { nom, uri } = body;

    if let Some(uri) = uri.as_ref() {
        if !tester_connexion(uri.clone()).await {
            return Err(erreur(
                StatusCode::BAD_GATEWAY,
                "Connexion libvirt impossible avec cette URI",
            ));
        }
    }

    let result = sqlx::query(
        "UPDATE hypervisors
         SET nom = COALESCE(?, nom),
             uri = COALESCE(?, uri),
             statut = CASE WHEN ? IS NULL THEN statut ELSE 'connecte' END
         WHERE id = ?",
    )
    .bind(nom)
    .bind(uri.clone())
    .bind(uri)
    .bind(&id)
    .execute(&state.db)
    .await
    .map_err(|_| erreur(StatusCode::INTERNAL_SERVER_ERROR, "Erreur d'ecriture SQLite"))?;

    if result.rows_affected() == 0 {
        return Err(erreur(StatusCode::NOT_FOUND, "Hyperviseur introuvable"));
    }

    let hypervisor = sqlx::query_as::<_, Hypervisor>(
        "SELECT id, nom, uri, statut FROM hypervisors WHERE id = ?",
    )
    .bind(&id)
    .fetch_one(&state.db)
    .await
    .map_err(|_| erreur(StatusCode::INTERNAL_SERVER_ERROR, "Erreur de lecture SQLite"))?;

    Ok(Json(hypervisor))
}

#[utoipa::path(
    delete,
    path = "/api/hypervisors/{id}",
    params(("id" = String, Path, description = "Identifiant de l'hyperviseur")),
    responses((status = 204), (status = 404, body = Message)),
    tag = "Hypervisors"
)]
pub async fn supprimer(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Message>)> {
    let result = sqlx::query("DELETE FROM hypervisors WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await
        .map_err(|_| erreur(StatusCode::INTERNAL_SERVER_ERROR, "Erreur d'ecriture SQLite"))?;

    if result.rows_affected() == 0 {
        return Err(erreur(StatusCode::NOT_FOUND, "Hyperviseur introuvable"));
    }

    Ok(StatusCode::NO_CONTENT)
}

async fn tester_connexion(uri: String) -> bool {
    tokio::task::spawn_blocking(move || {
        let Ok(connexion) = virt::connect::Connect::open(Some(&uri)) else {
            return false;
        };

        connexion.is_alive().unwrap_or(false)
    })
    .await
    .unwrap_or(false)
}

async fn rafraichir_statut(
    state: &crate::state::AppState,
    mut hypervisor: Hypervisor,
) -> Result<Hypervisor, (StatusCode, Json<Message>)> {
    let statut = if tester_connexion(hypervisor.uri.clone()).await {
        "connecte"
    } else {
        "deconnecte"
    };

    if hypervisor.statut != statut {
        sqlx::query("UPDATE hypervisors SET statut = ? WHERE id = ?")
            .bind(statut)
            .bind(&hypervisor.id)
            .execute(&state.db)
            .await
            .map_err(|_| erreur(StatusCode::INTERNAL_SERVER_ERROR, "Erreur d'ecriture SQLite"))?;
        hypervisor.statut = statut.into();
    }

    Ok(hypervisor)
}
