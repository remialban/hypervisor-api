use axum::{extract::{Path, State}, http::StatusCode, Json};
use uuid::Uuid;

use crate::{
    common::{erreur, non_implemente},
    models::{Disk, DiskRequest, Message, Network, NetworkRequest},
    state::SharedState,
};

type ApiError = (StatusCode, Json<Message>);

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/disks", params(("hypervisor_id" = String, Path)), responses((status = 200, body = [Disk]), (status = 500, body = Message)), tag = "Infrastructure")]
pub async fn disques(State(state): State<SharedState>, Path(hypervisor_id): Path<String>) -> Result<Json<Vec<Disk>>, ApiError> {
    let disks = sqlx::query_as::<_, Disk>("SELECT id, hypervisor_id, nom, taille_gb, format FROM disks WHERE hypervisor_id = ? ORDER BY nom")
        .bind(hypervisor_id).fetch_all(&state.db).await
        .map_err(|_| erreur(StatusCode::INTERNAL_SERVER_ERROR, "Erreur de lecture SQLite"))?;
    Ok(Json(disks))
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/disks", params(("hypervisor_id" = String, Path)), request_body = DiskRequest, responses((status = 201, body = Disk), (status = 500, body = Message)), tag = "Infrastructure")]
pub async fn creer_disque(State(state): State<SharedState>, Path(hypervisor_id): Path<String>, Json(body): Json<DiskRequest>) -> Result<(StatusCode, Json<Disk>), ApiError> {
    let disk = Disk { id: Uuid::new_v4().to_string(), hypervisor_id, nom: body.nom, taille_gb: body.taille_gb as i64, format: body.format };
    sqlx::query("INSERT INTO disks (id, hypervisor_id, nom, taille_gb, format) VALUES (?, ?, ?, ?, ?)")
        .bind(&disk.id).bind(&disk.hypervisor_id).bind(&disk.nom).bind(disk.taille_gb).bind(&disk.format)
        .execute(&state.db).await
        .map_err(|_| erreur(StatusCode::INTERNAL_SERVER_ERROR, "Erreur d'ecriture SQLite"))?;
    Ok((StatusCode::CREATED, Json(disk)))
}

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/disks/{disk_id}", params(("hypervisor_id" = String, Path), ("disk_id" = String, Path)), responses((status = 200, body = Disk), (status = 404, body = Message)), tag = "Infrastructure")]
pub async fn obtenir_disque(State(state): State<SharedState>, Path((hypervisor_id, disk_id)): Path<(String, String)>) -> Result<Json<Disk>, ApiError> {
    let disk = sqlx::query_as::<_, Disk>("SELECT id, hypervisor_id, nom, taille_gb, format FROM disks WHERE id = ? AND hypervisor_id = ?")
        .bind(disk_id).bind(hypervisor_id).fetch_optional(&state.db).await
        .map_err(|_| erreur(StatusCode::INTERNAL_SERVER_ERROR, "Erreur de lecture SQLite"))?
        .ok_or_else(|| erreur(StatusCode::NOT_FOUND, "Disque introuvable"))?;
    Ok(Json(disk))
}

#[utoipa::path(patch, path = "/api/hypervisors/{hypervisor_id}/disks/{disk_id}", params(("hypervisor_id" = String, Path), ("disk_id" = String, Path)), request_body = DiskRequest, responses((status = 200, body = Disk), (status = 404, body = Message)), tag = "Infrastructure")]
pub async fn modifier_disque(State(state): State<SharedState>, Path((hypervisor_id, disk_id)): Path<(String, String)>, Json(body): Json<DiskRequest>) -> Result<Json<Disk>, ApiError> {
    let result = sqlx::query("UPDATE disks SET nom = ?, taille_gb = ?, format = ? WHERE id = ? AND hypervisor_id = ?")
        .bind(body.nom).bind(body.taille_gb as i64).bind(body.format).bind(&disk_id).bind(&hypervisor_id)
        .execute(&state.db).await
        .map_err(|_| erreur(StatusCode::INTERNAL_SERVER_ERROR, "Erreur d'ecriture SQLite"))?;
    if result.rows_affected() == 0 { return Err(erreur(StatusCode::NOT_FOUND, "Disque introuvable")); }
    obtenir_disque(State(state), Path((hypervisor_id, disk_id))).await
}

#[utoipa::path(delete, path = "/api/hypervisors/{hypervisor_id}/disks/{disk_id}", params(("hypervisor_id" = String, Path), ("disk_id" = String, Path)), responses((status = 204), (status = 404, body = Message)), tag = "Infrastructure")]
pub async fn supprimer_disque(State(state): State<SharedState>, Path((hypervisor_id, disk_id)): Path<(String, String)>) -> Result<StatusCode, ApiError> {
    let result = sqlx::query("DELETE FROM disks WHERE id = ? AND hypervisor_id = ?").bind(disk_id).bind(hypervisor_id).execute(&state.db).await
        .map_err(|_| erreur(StatusCode::INTERNAL_SERVER_ERROR, "Erreur d'ecriture SQLite"))?;
    if result.rows_affected() == 0 { return Err(erreur(StatusCode::NOT_FOUND, "Disque introuvable")); }
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/networks", params(("hypervisor_id" = String, Path)), responses((status = 200, body = [Network]), (status = 500, body = Message)), tag = "Infrastructure")]
pub async fn reseaux(State(state): State<SharedState>, Path(hypervisor_id): Path<String>) -> Result<Json<Vec<Network>>, ApiError> {
    let networks = sqlx::query_as::<_, Network>("SELECT id, hypervisor_id, nom, bridge, statut FROM networks WHERE hypervisor_id = ? ORDER BY nom")
        .bind(hypervisor_id).fetch_all(&state.db).await
        .map_err(|_| erreur(StatusCode::INTERNAL_SERVER_ERROR, "Erreur de lecture SQLite"))?;
    Ok(Json(networks))
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/networks", params(("hypervisor_id" = String, Path)), request_body = NetworkRequest, responses((status = 201, body = Network), (status = 500, body = Message)), tag = "Infrastructure")]
pub async fn creer_reseau(State(state): State<SharedState>, Path(hypervisor_id): Path<String>, Json(body): Json<NetworkRequest>) -> Result<(StatusCode, Json<Network>), ApiError> {
    let network = Network { id: Uuid::new_v4().to_string(), hypervisor_id, nom: body.nom, bridge: body.bridge, statut: "inactif".into() };
    sqlx::query("INSERT INTO networks (id, hypervisor_id, nom, bridge, statut) VALUES (?, ?, ?, ?, ?)")
        .bind(&network.id).bind(&network.hypervisor_id).bind(&network.nom).bind(&network.bridge).bind(&network.statut)
        .execute(&state.db).await
        .map_err(|_| erreur(StatusCode::INTERNAL_SERVER_ERROR, "Erreur d'ecriture SQLite"))?;
    Ok((StatusCode::CREATED, Json(network)))
}

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/vnc", params(("hypervisor_id" = String, Path), ("vm_id" = String, Path)), responses((status = 501, body = Message)), tag = "Infrastructure")]
pub async fn vnc(Path((_hypervisor_id, _vm_id)): Path<(String, String)>) -> (StatusCode, Json<Message>) {
    non_implemente("La connexion VNC sera branchee a libvirt")
}
