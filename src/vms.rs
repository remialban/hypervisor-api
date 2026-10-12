use axum::{extract::{Path, State}, http::StatusCode, Json};
use uuid::Uuid;

use crate::{common::erreur, models::{CreateVm, Message, VirtualMachine}, state::SharedState};

type ApiError = (StatusCode, Json<Message>);

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/vms", params(("hypervisor_id" = String, Path)), responses((status = 200, body = [VirtualMachine]), (status = 500, body = Message)), tag = "VMs")]
pub async fn lister(State(state): State<SharedState>, Path(hypervisor_id): Path<String>) -> Result<Json<Vec<VirtualMachine>>, ApiError> {
    let vms = sqlx::query_as::<_, VirtualMachine>("SELECT id, hypervisor_id, nom, memoire_mb, vcpus, statut FROM vms WHERE hypervisor_id = ? ORDER BY nom")
        .bind(hypervisor_id).fetch_all(&state.db).await
        .map_err(|_| erreur(StatusCode::INTERNAL_SERVER_ERROR, "Erreur de lecture SQLite"))?;
    Ok(Json(vms))
}

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}", params(("hypervisor_id" = String, Path), ("vm_id" = String, Path)), responses((status = 200, body = VirtualMachine), (status = 404, body = Message)), tag = "VMs")]
pub async fn obtenir(State(state): State<SharedState>, Path((hypervisor_id, vm_id)): Path<(String, String)>) -> Result<Json<VirtualMachine>, ApiError> {
    let vm = sqlx::query_as::<_, VirtualMachine>("SELECT id, hypervisor_id, nom, memoire_mb, vcpus, statut FROM vms WHERE id = ? AND hypervisor_id = ?")
        .bind(vm_id).bind(hypervisor_id).fetch_optional(&state.db).await
        .map_err(|_| erreur(StatusCode::INTERNAL_SERVER_ERROR, "Erreur de lecture SQLite"))?
        .ok_or_else(|| erreur(StatusCode::NOT_FOUND, "VM introuvable"))?;
    Ok(Json(vm))
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/vms", params(("hypervisor_id" = String, Path)), request_body = CreateVm, responses((status = 201, body = VirtualMachine), (status = 500, body = Message)), tag = "VMs")]
pub async fn creer(State(state): State<SharedState>, Path(hypervisor_id): Path<String>, Json(body): Json<CreateVm>) -> Result<(StatusCode, Json<VirtualMachine>), ApiError> {
    let vm = VirtualMachine { id: Uuid::new_v4().to_string(), hypervisor_id, nom: body.nom, memoire_mb: body.memoire_mb as i64, vcpus: body.vcpus as i64, statut: "arretee".into() };
    sqlx::query("INSERT INTO vms (id, hypervisor_id, nom, memoire_mb, vcpus, statut) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(&vm.id).bind(&vm.hypervisor_id).bind(&vm.nom).bind(vm.memoire_mb).bind(vm.vcpus).bind(&vm.statut)
        .execute(&state.db).await
        .map_err(|_| erreur(StatusCode::INTERNAL_SERVER_ERROR, "Erreur d'ecriture SQLite"))?;
    Ok((StatusCode::CREATED, Json(vm)))
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/start", params(("hypervisor_id" = String, Path), ("vm_id" = String, Path)), responses((status = 200, body = VirtualMachine), (status = 404, body = Message)), tag = "VMs")]
pub async fn demarrer(State(state): State<SharedState>, Path((hypervisor_id, vm_id)): Path<(String, String)>) -> Result<Json<VirtualMachine>, ApiError> {
    changer_statut(&state, &hypervisor_id, &vm_id, "demarree").await
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/stop", params(("hypervisor_id" = String, Path), ("vm_id" = String, Path)), responses((status = 200, body = VirtualMachine), (status = 404, body = Message)), tag = "VMs")]
pub async fn eteindre(State(state): State<SharedState>, Path((hypervisor_id, vm_id)): Path<(String, String)>) -> Result<Json<VirtualMachine>, ApiError> {
    changer_statut(&state, &hypervisor_id, &vm_id, "arretee").await
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/clone", params(("hypervisor_id" = String, Path), ("vm_id" = String, Path)), request_body = CreateVm, responses((status = 201, body = VirtualMachine), (status = 500, body = Message)), tag = "VMs")]
pub async fn cloner(State(state): State<SharedState>, Path((hypervisor_id, _vm_id)): Path<(String, String)>, Json(body): Json<CreateVm>) -> Result<(StatusCode, Json<VirtualMachine>), ApiError> {
    creer(State(state), Path(hypervisor_id), Json(body)).await
}

#[utoipa::path(delete, path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}", params(("hypervisor_id" = String, Path), ("vm_id" = String, Path)), responses((status = 204), (status = 404, body = Message)), tag = "VMs")]
pub async fn supprimer(State(state): State<SharedState>, Path((hypervisor_id, vm_id)): Path<(String, String)>) -> Result<StatusCode, ApiError> {
    let result = sqlx::query("DELETE FROM vms WHERE id = ? AND hypervisor_id = ?").bind(vm_id).bind(hypervisor_id).execute(&state.db).await
        .map_err(|_| erreur(StatusCode::INTERNAL_SERVER_ERROR, "Erreur d'ecriture SQLite"))?;
    if result.rows_affected() == 0 { return Err(erreur(StatusCode::NOT_FOUND, "VM introuvable")); }
    Ok(StatusCode::NO_CONTENT)
}

async fn changer_statut(state: &SharedState, hypervisor_id: &str, vm_id: &str, statut: &str) -> Result<Json<VirtualMachine>, ApiError> {
    let result = sqlx::query("UPDATE vms SET statut = ? WHERE id = ? AND hypervisor_id = ?").bind(statut).bind(vm_id).bind(hypervisor_id).execute(&state.db).await
        .map_err(|_| erreur(StatusCode::INTERNAL_SERVER_ERROR, "Erreur d'ecriture SQLite"))?;
    if result.rows_affected() == 0 { return Err(erreur(StatusCode::NOT_FOUND, "VM introuvable")); }
    obtenir(State(state.clone()), Path((hypervisor_id.to_owned(), vm_id.to_owned()))).await
}
